//! Ревьювер аналитики (#1135): строки Review Analys → одна fresh-сессия
//! на волну (парность «1 сессия аналитики = 1 сессия ревьювера»).
//! Оптики plan-reviewer'а: sourcing, форма/дедуп, соразмерность, readiness.

use crate::error::UsecaseResult;
use crate::ports::{AgentSessionGateway, SessionSpec, TaskBoardRepo, TickStateStore};
use std::sync::Arc;
use uwuwu_domain::{Agent, AgentSlot, Status, Task};

/// Кулдаун ревью-волны по строке (ретрай умершей сессии — не чаще 15 мин).
const REVIEW_COOLDOWN_MS: u64 = 15 * 60_000;

pub struct AnalystReviewUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    /// Доски: те же, что у аналитика (волна собирает со всех).
    pub boards: Vec<crate::pipeline::analyst::AnalystBoard>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub state: Arc<dyn TickStateStore>,
    pub clock: Arc<dyn crate::ports::Clock>,
}

impl AnalystReviewUseCase {
    /// Один проход: не больше одного спавна за тик — вся очередь
    /// Review Analys уходит одной сессии.
    pub async fn tick(&self) -> UsecaseResult<usize> {
        // парность: ≤1 живого ревьювера аналитики одновременно
        if self.sessions.list_sessions().await?.iter().any(|s| {
            s.outcome.is_empty()
                && crate::naming::parse_agent(&s.agent) == Some(Agent::AnalystReviewer)
        }) {
            tracing::debug!(
                event = "analys_review.alive_skip",
                "Живой ревьювер — волна ждёт"
            );
            return Ok(0);
        }
        let now = self.clock.now_ms();
        let mut wave: Vec<Task> = Vec::new();
        let mut first: Option<&crate::pipeline::analyst::AnalystBoard> = None;
        for tb in &self.boards {
            let rows = match self.board.read_board(&tb.board).await {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(event = "analys_review.board_failed",
                        board = %tb.board, error = %e, "Ревью аналитики: доска не прочитана");
                    continue;
                }
            };
            for t in rows {
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
                wave.push(t);
                if first.is_none() {
                    first = Some(tb);
                }
            }
        }
        let Some(tb) = first else {
            return Ok(0);
        };
        let review_board = tb.clone();
        for t in &wave {
            self.state
                .mark_analys_review_spawn(&review_board.board, t.number(), now);
        }
        let spec = SessionSpec {
            directory: review_board.checkout.clone(),
            title: "ревью аналитики".into(),
            agent: AgentSlot::Role(Agent::AnalystReviewer),
            model: None,
            variant: None,
            bootstrap: false,
        };
        let session = self.sessions.create_session(&spec).await?;
        let mut sorted: Vec<Task> = wave;
        sorted.sort_by_key(|t| t.priority().rank());
        let prompt = analyst_review_prompt(&review_board.project, &sorted);
        self.sessions.prompt(&session, &prompt).await?;
        tracing::info!(event = "analys_review.spawned", rows = sorted.len(), session = %session,
            "Analyst-review wave spawned");
        Ok(1)
    }
}

fn analyst_review_prompt(project: &str, rows: &[Task]) -> String {
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
         Отчёт волны — одним сводным комментом (uwuwu_task_comment по каждой строке кратко,\n\
         вердикт и причина)."
    )
}
