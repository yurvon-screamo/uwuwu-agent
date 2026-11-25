//! Аналитика (замена триажа, #1135): все строки Analys досок образуют
//! одну волну → одна мастер-сессия аналитика на проект (методичка
//! agents/analyst: sourcing, соразмерность рамок, вердикты через MCP).

use crate::bots::{compact_verdict, CompactFacts, CompactPolicy};
use crate::error::UsecaseResult;
use crate::ports::{AnalystMaster, SessionSpec, TaskBoardRepo, TickStateStore};
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

/// Ключ привязки мастер-сессии: перечисление «роль:проект» (#1607) —
/// на ту же форму садится #1634 (analyst-reviewer) без второй карты.
pub fn analyst_master_key(role: &str, project: &str) -> String {
    format!("{role}:{project}")
}

/// Сырая строка модели карты («provider/id[#variant]») — значение поля
/// модели рождения в привязке (C1: сравнение — парсингом, не строками).
fn card_model_raw(model: &SpawnModel) -> String {
    match &model.variant {
        Some(v) => format!("{}/{}#{v}", model.model.provider(), model.model.id()),
        None => format!("{}/{}", model.model.provider(), model.model.id()),
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
            // Сырая строка модели карты — в привязку (spec ниже забирает
            // sm по частям, borrow после move невозможен).
            let sm_raw = card_model_raw(&sm);
            let spec = SessionSpec {
                directory: tb.checkout.clone(),
                title: format!("{} аналитика волны", Agent::Analyst.emoji()),
                agent: AgentSlot::Role(Agent::Analyst),
                custom_agent: None,
                model: sm.model.clone(),
                variant: sm.variant.clone(),
                bootstrap: false,
            };
            let sessions = self.sessions.project(&tb.project);
            // Мастер-сессия проекта (#1607, срез 2б): волна едет промптом
            // в живую привязанную сессию; мёртвая или разошедшаяся с
            // картой/конфигом — пересоздание с перезаписью привязки.
            let key = analyst_master_key("analyst", &tb.project);
            let mut reuse: Option<String> = None;
            if let Some(bound) = self.state.analyst_master(&key) {
                // Контракт расхождения (C1, прецедент ботов): модель
                // рождения ≠ карта или checkout ≠ конфигу — пересоздание.
                // Legacy-привязка (поля None) расхождением не считается.
                let model_ok = bound
                    .model
                    .as_deref()
                    .is_none_or(|raw| SpawnModel::parse(raw).ok().as_ref() == Some(&sm));
                let checkout_ok = bound.checkout.as_deref().is_none_or(|c| c == tb.checkout);
                if model_ok && checkout_ok {
                    match sessions.liveness(&bound.session).await {
                        crate::ports::Liveness::Alive => reuse = Some(bound.session),
                        // Second source (#1072→#1163): проба солгала,
                        // сессия жива в union-скане тика — респавн
                        // отменён, волна едет в неё же.
                        crate::ports::Liveness::Dead
                            if scan.iter().any(|s| s.id == bound.session) =>
                        {
                            tracing::warn!(
                                event = "analys.second_source_alive",
                                session = %bound.session,
                                "Проба Dead при живой сессии в union-скане — респавн отменён"
                            );
                            reuse = Some(bound.session);
                        }
                        // Dead без второго источника — пересоздание ниже.
                        crate::ports::Liveness::Dead => {}
                        // Unknown — доска откладывается на тик (канон
                        // stage-wakeup: без зрения вслепую не работаем).
                        crate::ports::Liveness::Unknown => {
                            tracing::info!(
                                event = "analys.deferred_unknown",
                                project = %tb.project,
                                session = %bound.session,
                                "Мастер-сессия: liveness неизвестен — доска ждёт тика"
                            );
                            continue;
                        }
                    }
                } else {
                    tracing::info!(
                        event = "analys.diverged",
                        project = %tb.project,
                        session = %bound.session,
                        "Привязка разошлась с картой/конфигом — пересоздание"
                    );
                }
            }
            if let Some(session) = reuse {
                let prompt = analyst_prompt(&tb.project, &wave);
                sessions.prompt(&session, &prompt).await?;
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
            let session = sessions.create_session(&spec).await?;
            let prompt = analyst_prompt(&tb.project, &wave);
            sessions.prompt(&session, &prompt).await?;
            // Привязка мастер-сессии: модель рождения — аргумент usage_pct
            // прохода (C1), checkout — контракт пересоздания (Low-1 р.4).
            self.state.set_analyst_master(
                &key,
                AnalystMaster {
                    session: session.clone(),
                    model: Some(sm_raw.clone()),
                    checkout: Some(tb.checkout.clone()),
                    last_compact_ms: 0,
                },
            );
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
        // гигиена состояния (R2-L3): счётчики обеих гвардов строк, ушедших
        // с досок (deleted/closed), из state.json вычищаются. Частичный сбой
        // чтения — prune откладывается: неполный live-набор стёр бы счётчики
        // живых строк непрочитанной доски
        if !read_failed {
            self.state.prune_analys_guards(&live_rows);
        }
        // Компакт-проход мастер-сессий (срез 4): после волн — волны в
        // приоритете за слоты капа.
        self.compact_pass(busy).await;
        Ok(spawned)
    }

    /// Компакт-проход мастер-сессий (#1607, срез 4): расширение матрицы
    /// вердикта ботов (`compact_verdict`, ADR-082) на привязки аналитики —
    /// та же политика `[compact]`, тот же нативный `compact_session`.
    /// Бот-ветка не затрагивается: гейт ниже — только для адмиссий
    /// аналитика (UC-05: «Ботов не гейти»). Секция отсутствует — no-op.
    /// Скан — по привязкам state (ключ «роль:проект»); занятость — от
    /// модели рождения привязки (C1), не от карты.
    async fn compact_pass(&self, busy: usize) {
        let Some(policy) = &self.compact else { return };
        // Пик-окно — как будильник: запуск хода откладывается целиком.
        if self.peak.holds(self.clock.utc_minute()) {
            tracing::info!(
                event = "analys.compact_deferred_peak",
                "Компакт-проход мастер-сессий отложен: пик-окно"
            );
            return;
        }
        let (scan, failed) = self.sessions.list_sessions_reachable().await;
        if !failed.is_empty() {
            tracing::warn!(
                event = "analys.compact_list_degraded",
                executors = ?failed,
                "Компакт-проход: часть исполнителей недоступна — факты по живому union"
            );
        }
        let now = self.clock.now_ms();
        let mut admitted = 0usize;
        for key in self.state.analyst_master_keys() {
            let Some(project) = key.strip_prefix("analyst:") else {
                continue;
            };
            let Some(bound) = self.state.analyst_master(&key) else {
                continue;
            };
            let sessions = self.sessions.project(project);
            let busy_flag = scan
                .iter()
                .find(|s| s.id == bound.session)
                .map(|s| s.busy)
                .unwrap_or(false);
            let idle_ms = match sessions.last_activity_ms(&bound.session).await {
                Ok(ms) => ms.map(|ms| now.saturating_sub(ms)),
                // Транспорт/потеря — пропуск до следующего тика (волна
                // подхватит живую сессию; мёртвую — пересоздаст).
                Err(_) => continue,
            };
            let model = bound
                .model
                .as_deref()
                .and_then(|raw| SpawnModel::parse(raw).ok())
                .unwrap_or_else(|| self.models.for_agent(Agent::Analyst));
            let usage_pct = match sessions
                .context_usage_pct(&bound.session, &model.model)
                .await
            {
                Ok(u) => u,
                Err(_) => continue,
            };
            let verdict = compact_verdict(
                &CompactFacts {
                    busy: busy_flag,
                    idle_ms,
                    usage_pct,
                    last_compact_ms: bound.last_compact_ms,
                    now_ms: now,
                },
                policy,
            );
            if !verdict.fire {
                tracing::debug!(
                    event = "analys.compact_skip",
                    key = %key,
                    reason = %verdict.reason,
                    "Компакт мастер-сессии: не стреляем"
                );
                continue;
            }
            // CUP-гейт адмиссий аналитика: admitted этого прохода занимают
            // слоты (волны тика — в счётчике busy вызывающего).
            if busy + admitted >= self.cup {
                tracing::info!(
                    event = "analys.compact_held_cup",
                    key = %key,
                    active = busy + admitted,
                    cap = self.cup,
                    "Компакт мастер-сессии отложен: CUP cap"
                );
                break;
            }
            // Метка ДО POST (ADR-082): коулдаун переживает рестарт сервера,
            // повторная адмиссия исключена.
            self.state.mark_analyst_compact(&key, now);
            match sessions.compact_session(&bound.session).await {
                Ok(()) => {
                    admitted += 1;
                    tracing::info!(
                        event = "analys.compacted",
                        key = %key,
                        session = %bound.session,
                        "Компакт мастер-сессии admitted"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        event = "analys.compact_failed",
                        key = %key,
                        error = %e,
                        "Адмиссия компакта не удалась — метка держит коулдаун"
                    );
                }
            }
        }
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

fn analyst_prompt(project: &str, rows: &[&Task]) -> String {
    let marker = ANALYST_WAVE_MARKER.replace("{project}", project);
    let mut list = String::new();
    for t in rows {
        list.push_str(&format!("- #{} «{}»\n", t.number(), t.title()));
    }
    format!(
        "{marker} — rows in Analys, the whole batch:\n\n{list}\n\
         Act per the repo methodology agents/analyst. Via uwuwu MCP, for each row:\n\
         1. uwuwu_task_get {{number}} — the full request; sources: the project Design Spec,\n\
            README/Domains, ADRs, verbatim owner quotes in the request/thread.\n\
         2. Elaboration by type (feature — US/UC/NFT/«How we test»; bug — symptom +\n\
            reproduction frame; spike/docs — hypothesis + «what counts as the answer»);\n\
            the artifact is the reworked body (uwuwu_task_body) per the type's TASK-TEMPLATE skeleton.\n\
         3. Verdicts: elaborated → uwuwu_task_edit {{number, status: \"Review Analys\"}};\n\
            duplicate/garbage → uwuwu_task_close {{number, not_planned: true}} with rationale;\n\
            a real blocker → Needs attention; a fork outside FT/NFT → a 🚩 flag in the body\n\
            (fork А/Б + an explicit recommendation), no questions to the owner.\n\
         You are an analyst: do not implement the task, do not write into the Design Spec, do not create tasks."
    )
}
