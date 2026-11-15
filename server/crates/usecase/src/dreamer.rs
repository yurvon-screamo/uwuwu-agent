//! Дример: пост-обработка уснувших сессий (REST-only, без opencode.db —
//! решение #1133). Инвариант: 1 рабочая сессия = 1 дример-сессия;
/// обработанные помечаются в состоянии тика.
use crate::error::UsecaseResult;
use crate::ports::{AgentSessionGateway, Clock, SessionSpec, TickStateStore};
use std::sync::Arc;
use uwuwu_domain::{Agent, ModelRef};

/// Idle-порог: уснувшая = нет активности 30 минут и есть финал.
const IDLE_MS: u64 = 30 * 60_000;

/// Агенты, чьи сессии не являются «рабочими» для дримера.
const SKIP_AGENTS: [Agent; 3] = [Agent::Dreamer, Agent::Triage, Agent::PlanReviewer];

pub struct DreamerUseCase {
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub state: Arc<dyn TickStateStore>,
    pub clock: Arc<dyn Clock>,
    pub spawn_model: Option<ModelRef>,
    pub checkout: String,
}

impl DreamerUseCase {
    /// Один проход: до одного дрим-спавна за тик.
    pub async fn tick(&self) -> UsecaseResult<usize> {
        let now = self.clock.now_ms();
        let list = self.sessions.list_sessions().await?;
        let candidate = list.into_iter().find(|s| {
            !s.outcome.is_empty()
                && now.saturating_sub(s.updated_ms) > IDLE_MS
                && Agent::parse(&s.agent)
                    .map(|a| !SKIP_AGENTS.contains(&a))
                    .unwrap_or(true)
                && !self.state.dreamer_done(&s.id)
        });
        let Some(target) = candidate else {
            return Ok(0);
        };
        let spec = SessionSpec {
            directory: self.checkout.clone(),
            title: format!("дример {}", &target.id[..12.min(target.id.len())]),
            agent: Some(Agent::Dreamer),
            model: self.spawn_model.clone(),
        };
        let session = self.sessions.create_session(&spec).await?;
        let prompt = dreamer_prompt(&target.id);
        self.sessions.prompt(&session, &prompt).await?;
        self.state.mark_dreamer_done(&target.id);
        tracing::info!(event = "dreamer.spawned", target = %target.id, session = %session, "Dreamer spawned");
        Ok(1)
    }
}

fn dreamer_prompt(target: &str) -> String {
    format!(
        "Пост-обработка завершённой сессии {target} (агент dreamer, методичка agents/dreamer).\n\n\
         1. Прочитай сессию: uwuwu_session_export {{session: \"{target}\"}}\n\
         2. Дедуп: uwuwu_docs_ls / docs_read по Experience — «нечего взять» — нормальный исход,\n\
            тогда просто зафиксируй это в своём итоге.\n\
         3. Ценное: класс-приписка в существующую статью опыта (uwuwu_docs_append),\n\
            новые статьи — умеренно; ADR — только если принято архитектурное решение.\n\
         4. Гигиена доков: устаревшее/дыры — точечные правки (uwuwu_docs_write).\n\
         Действуй автономно; итог — краткой строкой.",
    )
}
