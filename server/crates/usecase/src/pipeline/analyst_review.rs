//! Ревьювер аналитики (#1135): строки Review Analys → одна fresh-сессия
//! на волну доски (парность «1 сессия аналитики = 1 сессия ревьювера»).
//! Оптики plan-reviewer'а: sourcing, форма/дедуп, соразмерность, readiness.

use crate::error::UsecaseResult;
use crate::ports::{SessionSpec, TaskBoardRepo, TickStateStore};
use crate::spawn_models::SpawnModels;
use std::sync::Arc;
use uwuwu_domain::{Agent, AgentSlot, LoopOutcome, SpawnLoop, Status, Task};

use crate::pipeline::analyst::AnalystBoard;

/// Кулдаун ревью-волны по строке (ретрай умершей сессии — не чаще 15 мин).
const REVIEW_COOLDOWN_MS: u64 = 15 * 60_000;

pub struct AnalystReviewUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    /// Доски: те же, что у аналитика (волна собирает со всех).
    pub boards: Vec<AnalystBoard>,
    /// Роутер «проект → executor»: спавн ревьювера волны — гейтвей проекта.
    pub sessions: Arc<crate::router::SessionRouter>,
    pub state: Arc<dyn TickStateStore>,
    pub clock: Arc<dyn crate::ports::Clock>,
    pub models: Arc<SpawnModels>,
    /// Единый кап активных ходов исполнителя (policy.cup): ревью-волна не
    /// спавнится при полном капе (#1231).
    pub cup: usize,
}

impl AnalystReviewUseCase {
    /// Один проход: по каждой доске — не больше одного спавна за тик.
    /// Цикл-гвард ревью-контура (R2-C2): спавны ревьювера при неизменном
    /// статусе строки считаются SpawnLoop'ом (порог слотовых спавнов
    /// владельца 16.11, >4) — систематически умирающий ревьювер не ретраится
    /// вечно, строка уходит в Needs attention.
    pub async fn tick(&self) -> UsecaseResult<usize> {
        // парность: ≤1 живого ревьювера аналитики — busy-признак (ход в
        // полёте, #1231): outcome при новом ходе не очищается, дубль-волна
        // на 2+ ходе ревьювера была незаметна. Деградация по исполнителю
        // (#1450): как у аналитика — волна по живому union, отказ одного
        // исполнителя волну не останавливает
        let (scan, failed) = self.sessions.list_sessions_reachable().await;
        if !failed.is_empty() {
            tracing::warn!(
                event = "analys_review.list_degraded",
                executors = ?failed,
                "Ревью аналитики: часть исполнителей недоступна — волна по живому union"
            );
        }
        if crate::router::has_busy_agent(&scan, Agent::AnalystReviewer) {
            tracing::debug!(
                event = "analys_review.alive_skip",
                "Живой ревьювер — волна ждёт"
            );
            return Ok(0);
        }
        // CUP-гвард (#1231): ревью-волна — запуск хода, отложка при полном
        // капе; гарды строк не заряжаются (отложка — не спавн). Счётчик на
        // тик: спавны волн текущего тика занимают слоты (#1244-класс).
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
        let mut spawned = 0usize;
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
            // Сортировка ДО построения гвардов: пары (строка, счётчик)
            // собираются в одном порядке — zip не разъезжается (H1 ревью).
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
            // заняли слоты — при полном капе доска откладывается целиком
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
            // Спавн ДО зарядки счётчиков (канон «failed spawn не заряжает»):
            // сбой гейтвея не тратит попытку ревью и не заряжает кулдаун.
            let sm = self.models.for_agent(Agent::AnalystReviewer);
            let spec = SessionSpec {
                directory: tb.checkout.clone(),
                title: format!("{} ревью аналитики", Agent::AnalystReviewer.emoji()),
                agent: AgentSlot::Role(Agent::AnalystReviewer),
                custom_agent: None,
                model: sm.model,
                variant: sm.variant,
                bootstrap: false,
            };
            let sessions = self.sessions.project(&tb.project);
            let session = sessions.create_session(&spec).await?;
            // сортировка уже сделана на candidates (пары wave↔guards
            // построены в одном проходе) — здесь порядок не трогаем (L1)
            let prompt = analyst_review_prompt(&tb.project, &wave);
            sessions.prompt(&session, &prompt).await?;
            busy += 1; // спавн этого тика занимает слот капа
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
            spawned += 1;
        }
        Ok(spawned)
    }

    /// Петля ревью-спавнов: строка → Needs attention + коммент владельцу.
    /// Порядок: статус → сброс счётчика (сбой статуса счётчик сохраняет).
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
                    "⚠︎ Analytics review loop guard: {repeats} reviewer spawns without status \
                     progress (the row waits in Review Analys). The row moved to \
                     Needs attention — owner's triage."
                ),
            )
            .await
        {
            tracing::error!(event = "analys_review.escalate_comment_failed", number = task.number(), error = %e, "Коммент эскалации не ушёл");
        }
    }
}

fn analyst_review_prompt(project: &str, rows: &[&Task]) -> String {
    let mut list = String::new();
    for t in rows {
        list.push_str(&format!("- #{} «{}»\n", t.number(), t.title()));
    }
    format!(
        "Analytics review (project {project}) — rows in Review Analys, the whole wave:\n\n{list}\n\
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
