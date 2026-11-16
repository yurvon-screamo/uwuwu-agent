//! Дример: пост-обработка уснувших сессий (REST-only, без opencode.db —
//! решение #1133). Инвариант: 1 рабочая сессия = 1 дример-сессия;
/// обработанные помечаются в состоянии тика.
use crate::error::UsecaseResult;
use crate::naming;
use crate::ports::{Clock, SessionSpec, TickStateStore};
use crate::router::SessionRouter;
use std::sync::Arc;
use uwuwu_domain::{Agent, AgentSlot, ModelRef};

/// Idle-порог: уснувшая = нет активности 30 минут и есть финал.
const IDLE_MS: u64 = 30 * 60_000;
/// Дневной кап дример-спавнов (канон: ~раз в час).
const DAY_CAP: u32 = 24;

/// Агенты, чьи сессии не являются «рабочими» для дримера.
const SKIP_AGENTS: [Agent; 3] = [Agent::Dreamer, Agent::Analyst, Agent::AnalystReviewer];

pub struct DreamerUseCase {
    /// Роутер «проект → executor» (#1203): скан сессий — best-effort union
    /// по всем исполнителям (офлайн одного не останавливает пост-обработку),
    /// собственный спавн — дефолтный исполнитель.
    pub sessions: Arc<SessionRouter>,
    pub state: Arc<dyn TickStateStore>,
    pub clock: Arc<dyn Clock>,
    pub spawn_model: Option<ModelRef>,
    pub checkout: String,
}

impl DreamerUseCase {
    /// Один проход: до одного дрим-спавна за тик. Капы (директива
    /// владельца 15.11): ≤1 живого дримера, ≤24/день, цели — только
    /// сессии с финалом И реальной работой (токены > 0, не пустышки).
    pub async fn tick(&self) -> UsecaseResult<usize> {
        let now = self.clock.now_ms();
        let list = self.sessions.list_sessions_best_effort().await?;
        // ≤1 живого дримера одновременно
        if list
            .iter()
            .any(|s| s.outcome.is_empty() && naming::parse_agent(&s.agent) == Some(Agent::Dreamer))
        {
            tracing::debug!(event = "dreamer.alive_skip", "Живой дример — спавна нет");
            return Ok(0);
        }
        // дневной кап (сутки JST — канон расписаний)
        let day_key = jst_day_key(now);
        let today = self.state.dreamer_day_spawns(&day_key);
        if today >= DAY_CAP {
            tracing::debug!(
                event = "dreamer.day_cap",
                today,
                cap = DAY_CAP,
                "Дневной кап"
            );
            return Ok(0);
        }
        let candidate = list.into_iter().find(|s| {
            !s.outcome.is_empty()
                && s.total_tokens > 0
                && now.saturating_sub(s.updated_ms) > IDLE_MS
                && naming::parse_agent(&s.agent)
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
            custom_agent: None,
            agent: AgentSlot::Role(Agent::Dreamer),
            model: self.spawn_model.clone(),
            variant: None,
            bootstrap: false,
        };
        let sessions = self.sessions.default();
        let session = sessions.create_session(&spec).await?;
        let prompt = dreamer_prompt(&target.id);
        sessions.prompt(&session, &prompt).await?;
        self.state.mark_dreamer_done(&target.id);
        self.state.mark_dreamer_spawn(&day_key);
        tracing::info!(event = "dreamer.spawned", target = %target.id, session = %session, "Dreamer spawned");
        Ok(1)
    }
}

/// Ключ суток JST (YYYY-MM-DD) для дневного капа.
fn jst_day_key(ms: u64) -> String {
    let secs = ms / 1000 + 9 * 3600;
    let (y, mo, da) = uwuwu_domain::civil_from_days((secs / 86400) as i64);
    format!("{y:04}-{mo:02}-{da:02}")
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
