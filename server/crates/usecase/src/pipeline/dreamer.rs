//! Дример: пост-обработка уснувших сессий (REST-only, без opencode.db —
//! решение #1133). Инвариант: 1 рабочая сессия = 1 дример-сессия;
//! маркер обработки — watermark (revival ниже делает сессию кандидатом
//! снова, #1306), вечный скип — только не-источники опыта (боты).
use crate::error::UsecaseResult;
use crate::naming;
use crate::ports::{Clock, DreamerMark, SessionInfo, SessionSpec, TickStateStore};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use std::sync::Arc;
use uwuwu_domain::{Agent, AgentSlot};

/// Idle-порог: уснувшая = нет активности 30 минут и есть финал.
const IDLE_MS: u64 = 30 * 60_000;
/// Дневной кап дример-спавнов (канон: ~раз в час).
const DAY_CAP: u32 = 24;

/// Агенты, чьи сессии не являются «рабочими» для дримера.
const SKIP_AGENTS: [Agent; 3] = [Agent::Dreamer, Agent::Analyst, Agent::AnalystReviewer];

pub struct DreamerUseCase {
    /// Роутер «проект → executor» (#1203): скан сессий — strict union
    /// (busy-учёт капа и alive-гвард — решения по агрегату, #1203/#1231),
    /// собственный спавн — дефолтный исполнитель.
    pub sessions: Arc<SessionRouter>,
    pub state: Arc<dyn TickStateStore>,
    pub clock: Arc<dyn Clock>,
    pub models: Arc<SpawnModels>,
    pub checkout: String,
    /// Единый кап активных ходов исполнителя (policy.cup): дример-спавн
    /// откладывается при полном капе (#1231).
    pub cup: usize,
}

impl DreamerUseCase {
    /// Один проход: до одного дрим-действия за тик (спавн нового носителя
    /// или wakeup существующего — ADR-005 1:1). Капы (директива
    /// владельца 15.11): ≤1 живого дримера, ≤24/день, цели — только
    /// сессии с финалом И реальной работой (токены > 0, не пустышки).
    pub async fn tick(&self) -> UsecaseResult<usize> {
        let now = self.clock.now_ms();
        // Strict union (#1203/#1231): busy-учёт капа и alive-гвард —
        // решения по агрегату, частичный список искажает подсчёт.
        // Офлайн-исполнитель останавливает тик дримера до возврата
        // (пост-обработка догоняет на следующем тике).
        let list = self.sessions.list_sessions_strict().await?;
        // Гигиена state сразу после живого скана и ДО гвардов (#1306):
        // день с исчерпанным капом гигиену не пропускает — записи
        // умерших сессий не копятся.
        let live: std::collections::HashSet<String> = list.iter().map(|s| s.id.clone()).collect();
        self.state.prune_dreamer(&live);
        // ≤1 живого дримера — busy-признак (ход в полёте, #1231): outcome
        // при новом ходе не очищается, дубль-дример на 2+ ходе был незаметен
        if crate::router::has_busy_agent(&list, Agent::Dreamer) {
            tracing::debug!(event = "dreamer.alive_skip", "Живой дример — спавна нет");
            return Ok(0);
        }
        // CUP-гвард (#1231): дример-спавн откладывается при полном капе.
        let busy = crate::router::busy_count(&list);
        if busy >= self.cup {
            tracing::info!(
                event = "dreamer.held_cup",
                active = busy,
                cap = self.cup,
                "Дример-спавн отложен: CUP cap"
            );
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
        let candidate = list.iter().find(|s| {
            !s.outcome.is_empty()
                && s.total_tokens > 0
                && now.saturating_sub(s.updated_ms) > IDLE_MS
                && naming::parse_agent(&s.agent)
                    .map(|a| !SKIP_AGENTS.contains(&a))
                    .unwrap_or(true)
                && match self.state.dreamer_mark(&s.id) {
                    None => true,
                    Some(DreamerMark::Forever) => false,
                    Some(DreamerMark::UpTo(pos)) => s.updated_ms > pos,
                }
        });
        let Some(target) = candidate else {
            return Ok(0);
        };
        let sessions = self.sessions.default();
        let carrier = self.dreamer_carrier(target, &live).await?;
        sessions
            .prompt(carrier.session(), &carrier.prompt(&target.id))
            .await?;
        self.state
            .mark_dreamer(&target.id, DreamerMark::UpTo(target.updated_ms));
        self.state.mark_dreamer_spawn(&day_key);
        match &carrier {
            DreamerCarrier::Woke(session) => {
                tracing::info!(event = "dreamer.wakeup", target = %target.id, session = %session, "Повторный прогон — wakeup дример-сессии");
            }
            DreamerCarrier::Spawned(session) => {
                tracing::info!(event = "dreamer.spawned", target = %target.id, session = %session, "Dreamer spawned");
            }
        }
        Ok(1)
    }

    /// Носитель прогона (ADR-005 1:1): живой линк — wakeup существующей
    /// дример-сессии; линк мёртв или отсутствует — спавн нового носителя
    /// и связка в state (#1306).
    async fn dreamer_carrier(
        &self,
        target: &SessionInfo,
        live: &std::collections::HashSet<String>,
    ) -> UsecaseResult<DreamerCarrier> {
        if let Some(id) = self
            .state
            .dreamer_session(&target.id)
            .filter(|id| live.contains(id))
        {
            return Ok(DreamerCarrier::Woke(id));
        }
        let sm = self.models.for_agent(Agent::Dreamer);
        let spec = SessionSpec {
            directory: self.checkout.clone(),
            title: format!(
                "{} дример {}",
                Agent::Dreamer.emoji(),
                &target.id[..12.min(target.id.len())]
            ),
            custom_agent: None,
            agent: AgentSlot::Role(Agent::Dreamer),
            model: sm.model,
            variant: sm.variant,
            bootstrap: false,
        };
        let session = self.sessions.default().create_session(&spec).await?;
        self.state.link_dreamer_session(&target.id, &session);
        Ok(DreamerCarrier::Spawned(session))
    }
}

/// Носитель дрим-прогона: существующая дример-сессия или новая.
enum DreamerCarrier {
    /// Линк жив — wakeup повторным прогоном (контекст дедупа сохранён).
    Woke(String),
    /// Первый прогон или прежний носитель умер — новый спавн.
    Spawned(String),
}

impl DreamerCarrier {
    fn session(&self) -> &str {
        match self {
            DreamerCarrier::Woke(id) | DreamerCarrier::Spawned(id) => id,
        }
    }

    /// Wakeup-промпт «повторный прогон» для существующей сессии несёт
    /// только дельту (методичка и контекст дедупа уже в ней, ADR-005);
    /// новый носитель получает полный промпт пост-обработки.
    fn prompt(&self, target: &str) -> String {
        match self {
            DreamerCarrier::Woke(_) => repeat_prompt(target),
            DreamerCarrier::Spawned(_) => dreamer_prompt(target),
        }
    }
}

/// Ключ суток JST (YYYY-MM-DD) для дневного капа (pub(crate) — тесты капа).
pub(crate) fn jst_day_key(ms: u64) -> String {
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

/// Wakeup-промпт повторного прогона (ADR-005): сессия дополнилась после
/// позиции прошлого дрима — дример-сессия уже несёт методичку и контекст
/// дедупа, промпт несёт только факт дельты.
fn repeat_prompt(target: &str) -> String {
    format!(
        "Повторный прогон: сессия {target} дополнилась новыми сообщениями после прошлого дрима.\n\
         Перечитай экспорт (uwuwu_session_export {{session: \"{target}\"}}) и дообработай:\n\
         дедуп с уже записанным, приписки классов, гигиена доков. Итог — краткой строкой.",
    )
}
