//! Ревьювер аналитики (#1135): строки Review Analys → одна fresh-сессия
//! на волну доски (парность «1 сессия аналитики = 1 сессия ревьювера»).
//! Оптики plan-reviewer'а: sourcing, форма/дедуп, соразмерность, readiness.

use crate::error::UsecaseResult;
use crate::ports::{SessionSpec, TaskBoardRepo, TickStateStore};
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
        // на 2+ ходе ревьювера была незаметна. Strict union (#1203): как у
        // аналитика — живой ревьювер волны на офлайн-исполнителе не
        // порождает дубль-спавн на другом
        let scan = self.sessions.list_sessions_strict().await?;
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
            let spec = SessionSpec {
                directory: tb.checkout.clone(),
                title: format!("{} ревью аналитики", Agent::AnalystReviewer.emoji()),
                agent: AgentSlot::Role(Agent::AnalystReviewer),
                custom_agent: None,
                model: None,
                variant: None,
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
                    "⚠︎ Цикл-гвард ревью аналитики: {repeats} спавнов ревьювера без прогресса \
                     статуса (строка ждёт в Review Analys). Строка переведена в \
                     Needs attention — разбор владельца."
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
        "Ревью аналитики (проект {project}) — строки в Review Analys, волна целиком:\n\n{list}\n\
         Действуй по методичке репо agents/analyst-reviewer. По каждой строке:\n\
         1. uwuwu_task_get {{number}} — проработанное тело; сверяй с источниками заявки\n\
            (Design Spec/README/ADR/цитаты владельца): каждое требование имеет источник,\n\
            сочинённое = request_changes.\n\
         2. Оптики: sourcing; форма/дедуп (тайтл-конкретика, тело по TASK-TEMPLATE-костяку\n\
            типа); соразмерность (спайк не задавлен рамками фичи); readiness (постановку\n\
            можно брать в работу).\n\
         3. Вердикты: approve → uwuwu_task_edit {{number, status: \"Todo\"}};\n\
            request_changes → uwuwu_task_edit {{number, status: \"Analys\"}} + замечания комментами\n\
            (цикл-гвард считает круги); реальная проблема → Needs attention.\n\
         Отчёт — по одному комменту на строку (вердикт + причина кратко); никаких\n\
         отдельных сводных постов по волне сверх этих комментов."
    )
}
