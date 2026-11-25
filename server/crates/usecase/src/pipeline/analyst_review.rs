//! Ревьювер аналитики (#1135, мастер-сессия #1634): строки Review Analys
//! едут волной в живую мастер-сессию проекта (привязка роль x проект),
//! мертвая пересоздается, между волнами - нативный автокомпакт.
//! Отмена эфимерности #1588 старшим словом владельца 25.11.2026.

use crate::bots::{compact_verdict, CompactFacts, CompactPolicy};
use crate::error::UsecaseResult;
use crate::ports::{SessionInfo, SessionSpec, TaskBoardRepo, TickStateStore};
use crate::spawn_models::SpawnModels;
use std::sync::Arc;
use uwuwu_domain::{Agent, AgentSlot, LoopOutcome, SpawnLoop, SpawnModel, Status, Task};

use crate::pipeline::analyst::AnalystBoard;

/// Кулдаун ревью-волны по строке (ретрай умершей сессии - не чаще 15 мин).
const REVIEW_COOLDOWN_MS: u64 = 15 * 60_000;

/// Роль мастер-привязки ревьювера (ключ привязки - роль:проект).
fn reviewer_role() -> &'static str {
    crate::naming::agent_name(Agent::AnalystReviewer)
}

/// Канонический raw модели спавна: provider/id[#variant] - контракт
/// сравнения C1 (модель рождения против карты).
fn model_raw(sm: &SpawnModel) -> String {
    match &sm.variant {
        Some(v) => format!("{}/{}#{v}", sm.model.provider(), sm.model.id()),
        None => format!("{}/{}", sm.model.provider(), sm.model.id()),
    }
}

pub struct AnalystReviewUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    /// Доски: те же, что у аналитика (волна собирает со всех).
    pub boards: Vec<AnalystBoard>,
    /// Роутер проект -> executor: волна ревьювера - гейтвей проекта.
    pub sessions: Arc<crate::router::SessionRouter>,
    pub state: Arc<dyn TickStateStore>,
    pub clock: Arc<dyn crate::ports::Clock>,
    pub models: Arc<SpawnModels>,
    /// Единый кап активных ходов исполнителя (policy.cup): ревью-волна не
    /// спавнится при полном капе (#1231).
    pub cup: usize,
    /// Автокомпакт мастер-сессий между волнами (секция [compact];
    /// None - механика off).
    pub compact: Option<CompactPolicy>,
}

/// Дословный маркер волны ревью аналитики - контракт мок-стенда
/// (mock_opencode::stage_action_of_prompt); правка промпта через него.
pub const ANALYST_REVIEW_WAVE_MARKER: &str = "Analytics review (project {project})";

impl AnalystReviewUseCase {
    /// Один проход: по каждой доске - не больше одной волны за тик.
    /// Цикл-гвард ревью-контура (R2-C2): спавны ревьювера при неизменном
    /// статусе строки считаются SpawnLoop'ом - систематически умирающий
    /// ревьювер не ретраится вечно, строка уходит в Needs attention.
    pub async fn tick(&self) -> UsecaseResult<usize> {
        // Деградация по исполнителю (#1450): волна по живому union.
        let (scan, failed) = self.sessions.list_sessions_reachable().await;
        if !failed.is_empty() {
            tracing::warn!(
                event = "analys_review.list_degraded",
                executors = ?failed,
                "Ревью аналитики: часть исполнителей недоступна - волна по живому union"
            );
        }
        // Компакт-проход мастер-сессий - до busy/CUP-гардов: idle-привязки
        // других проектов компактируются даже когда волна ждет.
        if let Some(policy) = self.compact {
            self.compact_pass(&policy, &scan).await;
        }
        // Парность: живой ревьювер - ход в полете (#1231, сходимость
        // с #1640: повторный тик при живом аналитике/ревьювере не дает
        // дубль, волна ждет).
        if crate::router::has_busy_agent(&scan, Agent::AnalystReviewer) {
            tracing::debug!(
                event = "analys_review.alive_skip",
                "Живой ревьювер - волна ждёт"
            );
            return Ok(0);
        }
        // CUP-гвард (#1231): волна - запуск хода, отложка при полном
        // капе; гарды строк не заряжаются (отложка - не спавн). Счетчик на
        // тик: волны текущего тика занимают слоты (#1244-класс).
        let mut busy = crate::router::busy_count(&scan);
        if busy >= self.cup {
            tracing::info!(
                event = "analys_review.held_cup",
                active = busy,
                cap = self.cup,
                "Ревью-волна отложена: CUP cap"
            );
            return Ok(0);
        }
        let now = self.clock.now_ms();
        let mut delivered = 0usize;
        for tb in &self.boards {
            let rows = match self.board.read_board(&tb.board).await {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(event = "analys_review.board_failed",
                        board = %tb.board, error = %e, "Ревью аналитики: доска не прочитана");
                    continue;
                }
            };
            let mut candidates: Vec<&Task> = Vec::new();
            for t in &rows {
                if t.status() != Status::ReviewAnalys || t.title().is_empty() {
                    continue;
                }
                let fresh = self
                    .state
                    .last_analys_review_ms(&tb.board, t.number())
                    .map(|ts| now.saturating_sub(ts) >= REVIEW_COOLDOWN_MS)
                    .unwrap_or(true);
                if !fresh {
                    continue;
                }
                candidates.push(t);
            }
            // Сортировка ДО построения гвардов: пары собираются в одном
            // порядке - zip не разъезжается (H1 ревью).
            candidates.sort_by_key(|t| t.priority().rank());
            let mut wave: Vec<&Task> = Vec::new();
            let mut guards: Vec<SpawnLoop> = Vec::new();
            for t in candidates {
                let mut guard = self
                    .state
                    .analys_review_loop(&tb.board, t.number())
                    .map(|(status, repeats)| {
                        let mut g = SpawnLoop::new(status);
                        for _ in 0..repeats {
                            g.register_spawn(status);
                        }
                        g
                    })
                    .unwrap_or_else(|| SpawnLoop::new(t.status()));
                if guard.register_spawn(t.status()) == LoopOutcome::Escalate {
                    self.escalate(tb, t, guard.repeats()).await;
                    continue;
                }
                wave.push(t);
                guards.push(guard);
            }
            if wave.is_empty() {
                continue;
            }
            // CUP-гвард на доску (#1231): волны предыдущих досок этого тика
            // заняли слоты - при полном капе доска откладывается целиком.
            if busy >= self.cup {
                tracing::info!(
                    event = "analys_review.held_cup",
                    board = %tb.board,
                    active = busy,
                    cap = self.cup,
                    "Ревью-волна доски отложена: CUP cap"
                );
                continue;
            }
            // Матрица привязки (#1634 срез 2): живая - reuse, мертвая -
            // пересоздание, Unknown - отложенный ход без порчи привязки,
            // нет записи - спавн. Расхождение модели/чекаута (C1) -
            // пересоздание. Зарядка гвардов - только после успешной
            // доставки (канон failed spawn не заряжает).
            let sm = self.models.for_agent(Agent::AnalystReviewer);
            let expected = model_raw(&sm);
            let role = reviewer_role();
            let gateway = self.sessions.project(&tb.project);
            let prompt = analyst_review_prompt(&tb.project, &wave);
            enum WaveAct {
                Reuse(String),
                Recreate,
                Defer,
            }
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
                Some(_) => WaveAct::Recreate,
                None => WaveAct::Recreate,
            };
            match act {
                WaveAct::Defer => {
                    tracing::info!(
                        event = "analys_review.defer_unknown",
                        board = %tb.board,
                        "Волна отложена: состояние привязки неизвестно"
                    );
                    continue;
                }
                WaveAct::Reuse(session) => {
                    if let Err(e) = gateway.prompt(&session, &prompt).await {
                        tracing::warn!(
                            event = "analys_review.prompt_failed",
                            board = %tb.board,
                            session = %session,
                            error = %e,
                            "Промпт в живую сессию не доставлен - ретрай следующим тиком"
                        );
                        continue;
                    }
                    busy += 1;
                    for (t, guard) in wave.iter().zip(guards.iter()) {
                        self.state.set_analys_review_loop(
                            &tb.board,
                            t.number(),
                            guard.status(),
                            guard.repeats(),
                        );
                        self.state
                            .mark_analys_review_spawn(&tb.board, t.number(), now);
                    }
                    tracing::info!(event = "analys_review.reused", board = %tb.board,
                        rows = wave.len(), session = %session, "Review wave reused master session");
                    delivered += 1;
                }
                WaveAct::Recreate => {
                    let spec = SessionSpec {
                        directory: tb.checkout.clone(),
                        title: format!("{} ревью аналитики", Agent::AnalystReviewer.emoji()),
                        agent: AgentSlot::Role(Agent::AnalystReviewer),
                        custom_agent: None,
                        model: sm.model,
                        variant: sm.variant,
                        bootstrap: false,
                    };
                    let session = gateway.create_session(&spec).await?;
                    // Привязка - до промпта: сбой промпта не теряет волну,
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
                            event = "analys_review.state_save_failed",
                            board = %tb.board,
                            error = %e,
                            "Привязка не персистится - рестарт может дать дубль"
                        );
                    }
                    if let Err(e) = gateway.prompt(&session, &prompt).await {
                        tracing::warn!(
                            event = "analys_review.prompt_failed",
                            board = %tb.board,
                            session = %session,
                            error = %e,
                            "Промпт в новую сессию не доставлен - ретрай следующим тиком"
                        );
                        continue;
                    }
                    busy += 1;
                    for (t, guard) in wave.iter().zip(guards.iter()) {
                        self.state.set_analys_review_loop(
                            &tb.board,
                            t.number(),
                            guard.status(),
                            guard.repeats(),
                        );
                        self.state
                            .mark_analys_review_spawn(&tb.board, t.number(), now);
                    }
                    tracing::info!(event = "analys_review.spawned", board = %tb.board,
                        rows = wave.len(), session = %session, "Analyst-review wave spawned");
                    delivered += 1;
                }
            }
        }
        Ok(delivered)
    }

    /// Компакт-проход мастер-сессий ревьювера (#1634 срез 3, матрица
    /// bots/compact): idle + занятость окна модели рождения + коулдаун.
    /// Потерянная сессия (Dead) - мимо, транспорт Unknown - пропуск
    /// привязки, соседи продолжаются. CUP-гейт: при полном капе проход
    /// откладывается целиком (как у мастер-аналитика #1607).
    async fn compact_pass(&self, policy: &CompactPolicy, scan: &[SessionInfo]) {
        if crate::router::busy_count(scan) >= self.cup {
            tracing::info!(
                event = "analys_review.compact_held_cup",
                "Компакт-проход отложен: CUP cap"
            );
            return;
        }
        let now = self.clock.now_ms();
        let role = reviewer_role();
        let mut projects: Vec<&String> = self.boards.iter().map(|b| &b.project).collect();
        projects.sort();
        projects.dedup();
        for project in projects {
            let Some((session, bound_model, _)) = self.state.master_binding(role, project) else {
                continue;
            };
            let gateway = self.sessions.project(project);
            match gateway.liveness(&session).await {
                crate::ports::Liveness::Alive => {}
                crate::ports::Liveness::Dead => continue,
                crate::ports::Liveness::Unknown => continue,
            }
            let Ok(model) = SpawnModel::parse(&bound_model) else {
                tracing::warn!(
                    event = "analys_review.compact_model_unparseable",
                    project = %project,
                    "Привязка с непарсящейся моделью - компакт пропущен"
                );
                continue;
            };
            let busy = scan
                .iter()
                .find(|s| s.id == session)
                .map(|s| s.busy)
                .unwrap_or(false);
            let idle_ms = match gateway.last_activity_ms(&session).await {
                Ok(ms) => ms.map(|ms| now.saturating_sub(ms)),
                Err(_) => continue,
            };
            let usage_pct = match gateway.context_usage_pct(&session, &model.model).await {
                Ok(u) => u,
                Err(_) => continue,
            };
            let facts = CompactFacts {
                busy,
                idle_ms,
                usage_pct,
                last_compact_ms: self.state.master_compact_ms(role, project),
                now_ms: now,
            };
            let verdict = compact_verdict(&facts, policy);
            if !verdict.fire {
                tracing::debug!(
                    event = "analys_review.compact_skip",
                    project = %project,
                    reason = %verdict.reason,
                    "Компакт: не стреляем"
                );
                continue;
            }
            self.state.set_master_compact_ms(role, project, now);
            if let Err(e) = self.state.save() {
                tracing::warn!(event = "analys_review.compact_save_failed",
                    project = %project, error = %e, "Метка компакта не персистится");
            }
            match gateway.compact_session(&session).await {
                Ok(()) => {
                    tracing::info!(event = "analys_review.compacted",
                        project = %project, session = %session,
                        reason = %verdict.reason, "Нативный compact admitted");
                }
                Err(e) => {
                    tracing::warn!(event = "analys_review.compact_failed",
                        project = %project, session = %session,
                        error = %e, "Адмиссия компакта не удалась - ретрай по коулдауну");
                }
            }
        }
    }

    /// Петля ревью-спавнов: строка -> Needs attention + коммент владельцу.
    /// Порядок: статус -> сброс счётчика (сбой статуса счётчик сохраняет).
    async fn escalate(&self, tb: &AnalystBoard, task: &Task, repeats: u32) {
        tracing::warn!(
            event = "analys_review.loop_escalated",
            number = task.number(),
            repeats,
            "Ревью-спавны без прогресса статуса"
        );
        if let Err(e) = self
            .board
            .set_status(&tb.board, task.number(), Status::NeedsAttention)
            .await
        {
            tracing::error!(event = "analys_review.escalate_failed", number = task.number(), error = %e, "Эскалация не применилась");
            return;
        }
        self.state
            .clear_analys_review_loop(&tb.board, task.number());
        if let Err(e) = self
            .board
            .post_comment(
                task.number(),
                &format!(
                    "Analytics review loop guard: {repeats} reviewer spawns without status \
                     progress (the row waits in Review Analys). The row moved to \
                     Needs attention - owner's triage."
                ),
            )
            .await
        {
            tracing::error!(event = "analys_review.escalate_comment_failed", number = task.number(), error = %e, "Коммент эскалации не ушёл");
        }
    }
}

fn analyst_review_prompt(project: &str, rows: &[&Task]) -> String {
    let marker = ANALYST_REVIEW_WAVE_MARKER.replace("{project}", project);
    let mut list = String::new();
    for t in rows {
        list.push_str(&format!("- #{} «{}»\n", t.number(), t.title()));
    }
    format!(
        "{marker} — rows in Review Analys, the whole wave:\n\n{list}\n\
         Act per the repo methodology agents/analyst-reviewer. For each row:\n\
         1. uwuwu_task_get {{number}} — the reworked body; verify against the request sources\n\
            (Design Spec/README/ADRs/owner quotes): every requirement has a source;\n\
            invented = request_changes.\n\
         2. Optics: sourcing; form/dedup (title specificity, body per the type's TASK-TEMPLATE\n\
            skeleton); proportionality (a spike not crushed by feature frames); readiness (the\n\
            statement can be taken into work).\n\
         3. Verdicts: approve → uwuwu_task_edit {{number, status: \"Todo\"}};\n\
            request_changes → uwuwu_task_edit {{number, status: \"Analys\"}} + remarks in comments\n\
            (the round guard counts the rounds); a real problem → Needs attention.\n\
         The report — one comment per row (verdict + brief reason); no\n\
         separate wave summary posts beyond those comments."
    )
}
