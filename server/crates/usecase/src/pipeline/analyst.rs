//! Аналитика (замена триажа, #1135): все строки Analys досок образуют
//! одну волну → одна мастер-сессия аналитика на проект (методичка
//! agents/analyst: sourcing, соразмерность рамок, вердикты через MCP).

use crate::bots::CompactPolicy;
use crate::error::UsecaseResult;
use crate::pipeline::compact_pass::CompactPass;
use crate::ports::{SessionInfo, SessionSpec, TaskBoardRepo, TickStateStore};
use crate::spawn_models::SpawnModels;
use std::sync::Arc;
use uwuwu_domain::{Agent, AgentSlot, PeakWindow, SpawnModel, Status, Task};

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
    pub models: Arc<SpawnModels>,
    /// Единый кап активных ходов исполнителя (policy.cup): волна не
    /// спавнится при полном капе (#1231).
    pub cup: usize,
    /// Пик-окно GLM: компакт-проход откладывается целиком (политика
    /// запуска будильника, UC-05). Волны пик-гейта не несут (канон #1135).
    pub peak: PeakWindow,
    /// Автокомпакт мастер-сессий (#1607, срез 4): расширение матрицы
    /// ботов на привязки аналитики. None — проход off (safe default).
    pub compact: Option<CompactPolicy>,
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

/// Дословный маркер волны в промпте — контракт мок-стенда
/// (mock_opencode::stage_action_of_prompt); правка промпта через него.
pub const ANALYST_WAVE_MARKER: &str = "Analytics wave (project {project})";

/// Роль мастер-привязки аналитика (#1607): та же форма «роль:проект»,
/// что у ревьювера (#1634) — одна коллекция, один компакт-примитив.
fn analyst_role() -> &'static str {
    crate::naming::agent_name(Agent::Analyst)
}

/// Канонический raw модели спавна: provider/id[#variant] — контракт
/// сравнения C1 (модель рождения против карты), как у ревьювера.
fn model_raw(sm: &SpawnModel) -> String {
    match &sm.variant {
        Some(v) => format!("{}/{}#{v}", sm.model.provider(), sm.model.id()),
        None => format!("{}/{}", sm.model.provider(), sm.model.id()),
    }
}

impl AnalystUseCase {
    /// Один проход: по каждой доске — не больше одного спавна за тик,
    /// очередь доски летит в одну сессию пачкой (мультиборд: чекаут и
    /// проект — своей доски, C3 ревью).
    pub async fn tick(&self) -> UsecaseResult<usize> {
        // ≤1 живого аналитика: busy-признак («ход в полёте», #1231) —
        // outcome атрибут последнего финала и при новом ходе не
        // очищается, дубль-волна на 2+ ходе аналитика была незаметна.
        // Деградация по исполнителю (#1450): волна считается по живому
        // union — отказ одного исполнителя не останавливает волну
        let (scan, failed) = self.sessions.list_sessions_reachable().await;
        if !failed.is_empty() {
            tracing::warn!(
                event = "analys.list_degraded",
                executors = ?failed,
                "Аналитика: часть исполнителей недоступна — волна по живому union"
            );
        }
        // Гигиена привязок (#1607, как у ревьювера #1634): проекты, ушедшие
        // из конфига, не держат сирот в state.json. Вне компакт-прохода:
        // работает и при выключенной секции [compact].
        let live_projects: std::collections::HashSet<String> =
            self.boards.iter().map(|b| b.project.clone()).collect();
        if self.state.prune_master_role(analyst_role(), &live_projects) {
            if let Err(e) = self.state.save() {
                tracing::warn!(event = "analys.prune_save_failed", error = %e, "Гигиена привязок не персистится");
            }
        }
        // Компакт-проход мастер-сессий (срез 4) — до busy/CUP-гардов:
        // idle-привязки других проектов компактируются, даже когда
        // волна ждёт (как у ревьювера #1634).
        if let Some(policy) = &self.compact {
            self.compact_pass(policy, &scan).await;
        }
        if crate::router::has_busy_agent(&scan, Agent::Analyst) {
            tracing::debug!(event = "analys.alive_skip", "Живой аналитик — волна ждёт");
            return Ok(0);
        }
        // CUP-гвард (#1231): волна — запуск хода, ей нужен свободный слот
        // единого капа. Отложка — не спавн: кулдауны/цикл-гарды строк не
        // заряжаются (канон «failed spawn не заряжает»). Счётчик на тик:
        // спавны волн текущего тика занимают слоты — иначе мультиборд при
        // busy == cap−1 оверселлит до cap+N−1 (#1244-класс, ревью #1231).
        let mut busy = crate::router::busy_count(&scan);
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
            // CUP-гвард на доску (#1231): волны предыдущих досок этого тика
            // заняли слоты — при полном капе доска откладывается целиком
            if busy >= self.cup {
                tracing::info!(
                    event = "analys.held_cup",
                    board = %tb.board,
                    active = busy,
                    cap = self.cup,
                    "Волна доски отложена: CUP cap"
                );
                continue;
            }
            // порядок пачки — приоритет очереди (high → … → пусто)
            wave.sort_by_key(|t| t.priority().rank());
            // Спавн ДО зарядки счётчиков (канон «failed spawn не заряжает»,
            // tick_tests failed_spawn_does_not_charge_loop_or_cooldown):
            // сбой гейтвея не сжигает круг волны и не заряжает кулдаун.
            let sm = self.models.for_agent(Agent::Analyst);
            let expected = model_raw(&sm);
            let spec = SessionSpec {
                directory: tb.checkout.clone(),
                title: format!("{} аналитика волны", Agent::Analyst.emoji()),
                agent: AgentSlot::Role(Agent::Analyst),
                custom_agent: None,
                model: sm.model,
                variant: sm.variant,
                bootstrap: false,
            };
            let role = analyst_role();
            let gateway = self.sessions.project(&tb.project);
            let prompt = analyst_prompt(&tb.project, &wave);
            enum WaveAct {
                Reuse(String),
                Recreate,
                Defer,
            }
            // Мастер-сессия проекта (#1607, срез 2б; форма — #1634):
            // привязка жива — волна едет в неё же; мёртвая или
            // разошедшаяся с картой/конфигом — пересоздание.
            let act = match self.state.master_binding(role, &tb.project) {
                Some((session, bound_model, bound_checkout))
                    if bound_model == expected && bound_checkout == tb.checkout =>
                {
                    match gateway.liveness(&session).await {
                        crate::ports::Liveness::Alive => WaveAct::Reuse(session),
                        crate::ports::Liveness::Dead => WaveAct::Recreate,
                        crate::ports::Liveness::Unknown => WaveAct::Defer,
                    }
                }
                Some((session, _, _)) => {
                    tracing::info!(
                        event = "analys.diverged",
                        project = %tb.project,
                        session = %session,
                        "Привязка разошлась с картой/конфигом — пересоздание"
                    );
                    WaveAct::Recreate
                }
                None => WaveAct::Recreate,
            };
            match act {
                WaveAct::Defer => {
                    tracing::info!(
                        event = "analys.deferred_unknown",
                        project = %tb.project,
                        "Волна отложена: состояние привязки неизвестно"
                    );
                    continue;
                }
                WaveAct::Reuse(session) => {
                    if let Err(e) = gateway.prompt(&session, &prompt).await {
                        tracing::warn!(
                            event = "analys.prompt_failed",
                            board = %tb.board,
                            session = %session,
                            error = %e,
                            "Промпт в живую сессию не доставлен — ретрай следующим тиком"
                        );
                        continue;
                    }
                    busy += 1; // ход этого тика занимает слот капа
                    for t in &wave {
                        let rounds = self
                            .state
                            .analys_loop(&tb.board, t.number())
                            .map(|(_, r)| r)
                            .unwrap_or(0);
                        self.state
                            .set_analys_loop(&tb.board, t.number(), Status::Analys, rounds);
                        self.state.mark_analys_spawn(&tb.board, t.number(), now);
                    }
                    tracing::info!(event = "analys.reused", board = %tb.board, rows = wave.len(),
                        session = %session, "Analyst wave reused master session");
                    continue;
                }
                WaveAct::Recreate => {
                    let session = gateway.create_session(&spec).await?;
                    // Привязка — до промпта: сбой промпта не теряет волну,
                    // следующий тик reuse'ит свежую сессию и ретраит.
                    self.state.set_master_binding(
                        role,
                        &tb.project,
                        &session,
                        &expected,
                        &tb.checkout,
                    );
                    if let Err(e) = self.state.save() {
                        tracing::warn!(
                            event = "analys.state_save_failed",
                            board = %tb.board,
                            error = %e,
                            "Привязка не персистится — рестарт может дать дубль"
                        );
                    }
                    if let Err(e) = gateway.prompt(&session, &prompt).await {
                        tracing::warn!(
                            event = "analys.prompt_failed",
                            board = %tb.board,
                            session = %session,
                            error = %e,
                            "Промпт в новую сессию не доставлен — ретрай следующим тиком"
                        );
                        continue;
                    }
                    busy += 1; // спавн этого тика занимает слот капа
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
            }
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

    /// Компакт-проход аналитика (#1607, срез 4): общая механика — в
    /// `pipeline::compact_pass`; роль задаёт префикс событий и пик-гейт
    /// целевого провайдера (#1680; у ревьювера пика нет — осознанное
    /// различие, не дрейф).
    async fn compact_pass(&self, policy: &CompactPolicy, scan: &[SessionInfo]) {
        CompactPass {
            role: analyst_role(),
            event_prefix: "analys",
            projects: &self.boards,
            sessions: &self.sessions,
            state: self.state.as_ref(),
            clock: self.clock.as_ref(),
            scan,
            policy,
            cup: self.cup,
            peak: Some(&self.peak),
        }
        .run()
        .await;
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
                    "⚠︎ Analytics loop guard: {rounds} full rounds (Review Analys → Analys) \
                     without reaching Todo — the row went through review and came back. The row moved \
                     to Needs attention — owner's triage."
                ),
            )
            .await
        {
            tracing::error!(event = "analys.escalate_comment_failed", number = task.number(), error = %e, "Коммент эскалации не ушёл");
        }
    }
}

pub(crate) fn analyst_prompt(project: &str, rows: &[&Task]) -> String {
    let marker = ANALYST_WAVE_MARKER.replace("{project}", project);
    let mut list = String::new();
    for t in rows {
        list.push_str(&format!("- #{} «{}»\n", t.number(), t.title()));
    }
    format!(
        "{marker} — rows in Analys, the whole batch:\n\n{list}\n\
         Methodology: agents/analyst.md (loaded by opencode as your agent definition).\n\
         Act per that methodology for every row.\n\
         After an owner reply in chat, fix it verbatim in the body/thread and pass\n\
         owner_answer with the source status when clearing Needs attention."
    )
}
