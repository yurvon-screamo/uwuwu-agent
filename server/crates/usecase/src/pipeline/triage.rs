//! Триаж черновиков: Draft с заполненным телом → один триаж-агент за тик
//! (методичка agents/triage: дедуп/редактура/вердикт Todo или флаг владельцу).

use crate::error::UsecaseResult;
use crate::ports::{AgentSessionGateway, SessionSpec, TaskBoardRepo};
use std::sync::Arc;
use uwuwu_domain::{ModelRef, Status, Task};

pub struct TriageUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    /// Доски триажа: (проект, доска, чекаут) — канон: черновики всех досок.
    pub boards: Vec<TriageBoard>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub state: Arc<dyn crate::ports::TickStateStore>,
    pub clock: Arc<dyn crate::ports::Clock>,
    pub spawn_model: Option<ModelRef>,
}

/// Доска триажа: где искать черновики и где спавнить агента.
#[derive(Debug, Clone)]
pub struct TriageBoard {
    pub project: String,
    pub board: String,
    pub checkout: String,
}

impl TriageUseCase {
    /// Один проход триажа: не больше одного спавна за тик (мягкий гейт)
    /// по всем доскам проектов.
    pub async fn tick(&self) -> UsecaseResult<usize> {
        // cooldown: строку триажим не чаще раза в 15 минут (шторм-гард)
        let now = self.clock.now_ms();
        let mut candidate: Option<(Task, TriageBoard)> = None;
        for tb in &self.boards {
            let rows = match self.board.read_board(&tb.board).await {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(event = "triage.board_failed",
                        board = %tb.board, error = %e, "Триаж: доска не прочитана");
                    continue;
                }
            };
            let found = rows.into_iter().find(|t| {
                t.status() == Status::Draft
                    && !t.title().is_empty()
                    && self
                        .state
                        .last_triage_ms(&tb.board, t.number())
                        .map(|ts| now.saturating_sub(ts) >= 15 * 60_000)
                        .unwrap_or(true)
            });
            if let Some(task) = found {
                candidate = Some((task, tb.clone()));
                break;
            }
        }
        let Some((task, tb)) = candidate else {
            return Ok(0);
        };
        let spec = SessionSpec {
            directory: tb.checkout.clone(),
            title: format!("триаж #{}", task.number()),
            agent: uwuwu_domain::AgentSlot::Role(uwuwu_domain::Agent::Triage),
            model: self.spawn_model.clone(),
            variant: None,
            bootstrap: false,
        };
        self.state
            .mark_triage_spawn(&tb.board, task.number(), self.clock.now_ms());
        let session = self.sessions.create_session(&spec).await?;
        let prompt = triage_prompt(&tb.project, &task);
        self.sessions.prompt(&session, &prompt).await?;
        tracing::info!(event = "triage.spawned", number = task.number(), session = %session, "Triage agent spawned");
        Ok(1)
    }
}

fn triage_prompt(project: &str, task: &Task) -> String {
    format!(
        "Триаж черновика #{number} «{title}» (проект {project}, статус Draft).\n\n\
         Действуй по методичке репо agents/triage (канон триажа). Через MCP uwuwu:\n\
         1. uwuwu_task_get {{number: {number}}} — прочитай тело целиком\n\
         2. uwuwu_docs_ls / uwuwu_docs_read — дедуп по вики и опыту; uwuwu_task_list — доска\n\
         3. Вердикт: валидна → uwuwu_task_edit {{number: {number}, status: \"Todo\"}} + коммент-сводка;\n\
            дубль/мусор → uwuwu_task_close {{number: {number}, not_planned: true}} с комментом-обоснованием;\n\
            нужна правка — отредактируй тело uwuwu_task_body и переведи в Todo.\n\
         Ты — триаж-агент: не диагностируй баги, не чини код; только валидация заявки.",
        number = task.number(),
        title = task.title(),
    )
}
