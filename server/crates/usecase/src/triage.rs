//! Триаж черновиков: Draft с заполненным телом → один триаж-агент за тик
//! (методичка agents/triage: дедуп/редактура/вердикт Todo или флаг владельцу).

use crate::error::UsecaseResult;
use crate::ports::{AgentSessionGateway, SessionSpec, TaskBoardRepo};
use std::sync::Arc;
use uwuwu_domain::Status;
use uwuwu_domain::Task;

pub struct TriageUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    pub board_name: Arc<String>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub spawn_model: Option<(String, String)>,
    pub checkout: String,
}

impl TriageUseCase {
    /// Один проход триажа: не больше одного спавна за тик (мягкий гейт).
    pub async fn tick(&self) -> UsecaseResult<usize> {
        let rows = self.board.read_board(&self.board_name).await?;
        let candidate = rows
            .iter()
            .find(|t| t.status == Status::Draft && !t.title.is_empty())
            .cloned();
        let Some(task) = candidate else { return Ok(0) };
        let spec = SessionSpec {
            directory: self.checkout.clone(),
            title: format!("триаж #{}", task.number),
            agent: None,
            model: self.spawn_model.clone(),
        };
        let session = self.sessions.create_session(&spec).await?;
        let prompt = triage_prompt(&task);
        self.sessions.prompt(&session, &prompt).await?;
        tracing::info!(event = "triage.spawned", number = task.number, session = %session, "Triage agent spawned");
        Ok(1)
    }
}

fn triage_prompt(task: &Task) -> String {
    format!(
        "Триаж черновика #{number} «{title}» (доска «Задачи uwuwu», статус Draft).\n\n\
         Действуй по методичке репо agents/triage (канон триажа). Через MCP uwuwu:\n\
         1. uwuwu_task_get {{number: {number}}} — прочитай тело целиком\n\
         2. uwuwu_docs_ls / uwuwu_docs_read — дедуп по вики и опыту; uwuwu_task_list — доска\n\
         3. Вердикт: валидна → uwuwu_task_edit {{number: {number}, status: \"Todo\"}} + коммент-сводка;\n\
            дубль/мусор → uwuwu_task_close {{number: {number}, not_planned: true}} с комментом-обоснованием;\n\
            нужна правка — отредактируй тело uwuwu_task_body и переведи в Todo.\n\
         Ты — триаж-агент: не диагностируй баги, не чини код; только валидация заявки.",
        number = task.number,
        title = task.title,
    )
}
