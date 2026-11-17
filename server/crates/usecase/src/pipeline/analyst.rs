//! Аналитика (замена триажа, #1135): все строки Analys досок образуют
//! одну волну → одна fresh-сессия аналитика на волну (методичка
//! agents/analyst: sourcing, соразмерность рамок, вердикты через MCP).

use crate::error::UsecaseResult;
use crate::ports::{SessionSpec, TaskBoardRepo, TickStateStore};
use std::sync::Arc;
use uwuwu_domain::{Agent, AgentSlot, Status, Task};

/// Кулдаун волны по строке (анти-шторм, как у спавнов слотов).
const WAVE_COOLDOWN_MS: u64 = 15 * 60_000;
/// Цикл-гвард: три круга без выхода в Todo — тупик владельцу (канон
/// «третий круг»). Круг = возврат строки из Review Analys в Analys
/// (#1241): счётчик инкрементится наблюдением смены статусов, захват
/// волны кругом НЕ считается (умирающая безвердиктная сессия — ретрай
/// кулдауном, без эскалации).
const CYCLE_LIMIT: u32 = 3;

pub struct AnalystUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    /// Доски аналитики: (проект, доска, чекаут) — волна собирает со всех.
    pub boards: Vec<AnalystBoard>,
    /// Роутер «проект → executor»: спавн волны — гейтвей проекта доски.
    pub sessions: Arc<crate::router::SessionRouter>,
    pub state: Arc<dyn TickStateStore>,
    pub clock: Arc<dyn crate::ports::Clock>,
    /// Единый кап активных ходов исполнителя (policy.cup): волна не
    /// спавнится при полном капе (#1231).
    pub cup: usize,
}

/// Доска аналитики: где искать очередь и где спавнить агента.
#[derive(Debug, Clone)]
pub struct AnalystBoard {
    pub project: String,
    pub board: String,
    pub checkout: String,
}

/// Стабильный ключ строки в state-коллекциях.
fn board_row_key(board: &str, number: u64) -> String {
    format!("{board}:{number}")
}

impl AnalystUseCase {
    /// Один проход: по каждой доске — не больше одного спавна за тик,
    /// очередь доски летит в одну сессию пачкой (мультиборд: чекаут и
    /// проект — своей доски, C3 ревью).
    pub async fn tick(&self) -> UsecaseResult<usize> {
        // ≤1 живого аналитика: busy-признак («ход в полёте», #1231) —
        // outcome атрибут последнего финала и при новом ходе не
        // очищается, дубль-волна на 2+ ходе аналитика была незаметна.
        // Strict union (#1203): живой аналитик на офлайн-исполнителе не
        // должен порождать дубль-спавн волны на другом — как CUP, отказ
        // любого исполнителя честно останавливает волну до следующего тика
        let scan = self.sessions.list_sessions_strict().await?;
        if crate::router::has_busy_agent(&scan, Agent::Analyst) {
            tracing::debug!(event = "analys.alive_skip", "Живой аналитик — волна ждёт");
            return Ok(0);
        }
        // CUP-гвард (#1231): волна — запуск хода, ей нужен свободный слот
        // единого капа. Отложка — не спавн: кулдауны/цикл-гарды строк не
        // заряжаются (канон «failed spawn не заряжает»).
        let busy = crate::router::busy_count(&scan);
        if busy >= self.cup {
            tracing::info!(
                event = "analys.held_cup",
                active = busy,
                cap = self.cup,
                "Волна аналитики отложена: CUP cap"
            );
            return Ok(0);
        }
        let now = self.clock.now_ms();
        let mut spawned = 0usize;
        let mut live_rows: std::collections::HashSet<String> = Default::default();
        // непрочитанная доска — её строки не в live_rows; prune в этом тике
        // не выполняется, иначе сотрёт счётчики живых строк (R2-L2/дельта R3)
        let mut read_failed = false;
        for tb in &self.boards {
            let rows = match self.board.read_board(&tb.board).await {
                Ok(rows) => rows,
                Err(e) => {
                    read_failed = true;
                    tracing::warn!(event = "analys.board_failed",
                        board = %tb.board, error = %e, "Аналитика: доска не прочитана");
                    continue;
                }
            };
            let mut wave: Vec<&Task> = Vec::new();
            for t in &rows {
                live_rows.insert(board_row_key(&tb.board, t.number()));
                // цикл-гвард: счётчик строки вне цикла Analys↔Review Analys
                // устарел (строка уехала в пайп/терминал) — сброс
                if self.state.analys_loop(&tb.board, t.number()).is_some()
                    && !matches!(t.status(), Status::Analys | Status::ReviewAnalys)
                {
                    self.state.clear_analys_loop(&tb.board, t.number());
                }
                // ревью-гвард (R2-C1): строка покинула Review Analys — счётчик
                // ревью-спавнов устарел, легитимный перезаход начинает с нуля
                if self
                    .state
                    .analys_review_loop(&tb.board, t.number())
                    .is_some()
                    && t.status() != Status::ReviewAnalys
                {
                    self.state.clear_analys_review_loop(&tb.board, t.number());
                }
                // Круг-наблюдение (#1241): счётчик цикл-гварда считают
                // реальные круги — строка сходила в Review Analys и
                // вернулась. Пара хранит последний увиденный статус цикла:
                // возврат из ревью инкрементит, уход в ревью только
                // фиксируется. Захват волны круг НЕ сжигает — умирающая
                // без вердикта сессия аналитика ретраится кулдауном
                // без ложной эскалации (инцидент 17.11: три волны за час
                // сжигали счётчик при одном видимом переходе).
                if let Some((last, rounds)) = self.state.analys_loop(&tb.board, t.number()) {
                    match t.status() {
                        Status::Analys if last == Status::ReviewAnalys => {
                            self.state.set_analys_loop(
                                &tb.board,
                                t.number(),
                                Status::Analys,
                                rounds + 1,
                            );
                        }
                        Status::ReviewAnalys if last == Status::Analys => {
                            self.state.set_analys_loop(
                                &tb.board,
                                t.number(),
                                Status::ReviewAnalys,
                                rounds,
                            );
                        }
                        _ => {}
                    }
                }
                if t.status() != Status::Analys || t.title().is_empty() {
                    continue;
                }
                // ⏰-тайтлы — крон-семейство (остаток миграции ADR-103):
                // ран-карточки и мастера-строки мимо аналитики
                if t.title().starts_with('⏰') {
                    continue;
                }
                let fresh = self
                    .state
                    .last_analys_ms(&tb.board, t.number())
                    .map(|ts| now.saturating_sub(ts) >= WAVE_COOLDOWN_MS)
                    .unwrap_or(true);
                if !fresh {
                    continue;
                }
                let rounds = self
                    .state
                    .analys_loop(&tb.board, t.number())
                    .map(|(_, r)| r)
                    .unwrap_or(0);
                if rounds >= CYCLE_LIMIT {
                    self.escalate(tb, t, rounds).await;
                    continue;
                }
                wave.push(t);
            }
            if wave.is_empty() {
                continue;
            }
            // порядок пачки — приоритет очереди (high → … → пусто)
            wave.sort_by_key(|t| t.priority().rank());
            // Спавн ДО зарядки счётчиков (канон «failed spawn не заряжает»,
            // tick_tests failed_spawn_does_not_charge_loop_or_cooldown):
            // сбой гейтвея не сжигает круг волны и не заряжает кулдаун.
            let spec = SessionSpec {
                directory: tb.checkout.clone(),
                title: "аналитика волны".into(),
                agent: AgentSlot::Role(Agent::Analyst),
                custom_agent: None,
                model: None,
                variant: None,
                bootstrap: false,
            };
            let sessions = self.sessions.project(&tb.project);
            let session = sessions.create_session(&spec).await?;
            let prompt = analyst_prompt(&tb.project, &wave);
            sessions.prompt(&session, &prompt).await?;
            for t in &wave {
                let rounds = self
                    .state
                    .analys_loop(&tb.board, t.number())
                    .map(|(_, r)| r)
                    .unwrap_or(0);
                // Захват волны круг не сжигает (#1241): пара только
                // фиксируется (свежие строки — (Analys, 0)), rounds не
                // растут — ретрай безвердиктной волны не является петлёй
                self.state
                    .set_analys_loop(&tb.board, t.number(), Status::Analys, rounds);
                self.state.mark_analys_spawn(&tb.board, t.number(), now);
            }
            tracing::info!(event = "analys.spawned", board = %tb.board, rows = wave.len(),
                session = %session, "Analyst wave spawned");
            spawned += 1;
        }
        // гигиена состояния (R2-L3): счётчики обеих гвардов строк, ушедших
        // с досок (deleted/closed), из state.json вычищаются. Частичный сбой
        // чтения — prune откладывается: неполный live-набор стёр бы счётчики
        // живых строк непрочитанной доски
        if !read_failed {
            self.state.prune_analys_guards(&live_rows);
        }
        Ok(spawned)
    }

    /// Тупик цикла: строка переведена в Needs attention + коммент владельцу.
    /// Порядок: статус → сброс счётчика (сбой статуса счётчик сохраняет —
    /// круг не теряется, R2-C1).
    async fn escalate(&self, tb: &AnalystBoard, task: &Task, rounds: u32) {
        tracing::warn!(
            event = "analys.cycle_escalated",
            number = task.number(),
            rounds,
            "Цикл Analys↔Review Analys без выхода в Todo"
        );
        if let Err(e) = self
            .board
            .set_status(&tb.board, task.number(), Status::NeedsAttention)
            .await
        {
            tracing::error!(event = "analys.escalate_failed", number = task.number(), error = %e, "Эскалация не применилась");
            return;
        }
        // статус применился — счётчик круга сбрасывается (не раньше)
        self.state.clear_analys_loop(&tb.board, task.number());
        if let Err(e) = self
            .board
            .post_comment(
                task.number(),
                &format!(
                    "⚠︎ Цикл-гвард аналитики: {rounds} реальных круга (Review Analys → Analys) \
                     без выхода в Todo — строка ходила на ревью и возвращалась. Строка переведена \
                     в Needs attention — разбор владельца."
                ),
            )
            .await
        {
            tracing::error!(event = "analys.escalate_comment_failed", number = task.number(), error = %e, "Коммент эскалации не ушёл");
        }
    }
}

fn analyst_prompt(project: &str, rows: &[&Task]) -> String {
    let mut list = String::new();
    for t in rows {
        list.push_str(&format!("- #{} «{}»\n", t.number(), t.title()));
    }
    format!(
        "Аналитика волны (проект {project}) — строки в Analys, пачка целиком:\n\n{list}\n\
         Действуй по методичке репо agents/analyst. Через MCP uwuwu для каждой строки:\n\
         1. uwuwu_task_get {{number}} — заявка целиком; источники: Design Spec проекта,\n\
            README/Домены, ADR, дословные цитаты владельца в заявке/треде.\n\
         2. Проработка по типу (feature — US/UC/НФТ/«Как тестируем»; bug — симптом +\n\
            фрейм воспроизведения; spike/docs — гипотеза + «что считаем ответом»);\n\
            артефакт — переработанное тело (uwuwu_task_body) по TASK-TEMPLATE-костяку типа.\n\
         3. Вердикты: проработано → uwuwu_task_edit {{number, status: \"Review Analys\"}};\n\
            дубль/мусор → uwuwu_task_close {{number, not_planned: true}} с обоснованием;\n\
            реальный блокер → Needs attention; развилка не по ФТ/НФТ → 🚩-флаг в тело\n\
            (развилка А/Б + явная рекомендация), вопросов владельцу нет.\n\
         Ты — аналитик: не реализуй задачу, не пиши в Design Spec, не создавай задач."
    )
}
