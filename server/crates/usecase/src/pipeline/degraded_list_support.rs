//! Харнес ролевых пинов деградации (#1609): живой/отказной гейтвеи,
//! ловушка tracing-событий и кандидат дримера. Только in-memory, без сети.

use crate::error::UsecaseResult;
use crate::ports::{
    AgentSessionGateway, AssistantReply, Clock, Liveness, SessionInfo, SessionSpec,
};
use crate::router::SessionRouter;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::dispatcher::DefaultGuard;
use uwuwu_domain::UtcMinute;

/// Гейтвей с памятью зовов: отказ list_sessions у одного исполнителя,
/// живой спавн/delivery у другого.
pub(crate) struct RecordingGw {
    fail_list: bool,
    sessions: Vec<SessionInfo>,
    pub(crate) created: Arc<Mutex<Vec<String>>>,
    pub(crate) prompted: Arc<Mutex<Vec<String>>>,
}

impl RecordingGw {
    pub(crate) fn new(fail_list: bool, sessions: Vec<SessionInfo>) -> Arc<Self> {
        Arc::new(Self {
            fail_list,
            sessions,
            created: Arc::new(Mutex::new(Vec::new())),
            prompted: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

#[async_trait]
impl AgentSessionGateway for RecordingGw {
    async fn create_session(&self, _spec: &SessionSpec) -> UsecaseResult<String> {
        self.created
            .lock()
            .expect("created lock")
            .push("new".into());
        Ok("ses_new".into())
    }
    async fn prompt(&self, session: &str, _text: &str) -> UsecaseResult<()> {
        self.prompted
            .lock()
            .expect("prompted lock")
            .push(session.into());
        Ok(())
    }
    async fn run_command(&self, _s: &str, _n: &str, _a: &str, _b: u64) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, _s: &str) -> Liveness {
        Liveness::Alive
    }
    async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
        Ok(None)
    }
    async fn last_assistant_reply(&self, _s: &str) -> UsecaseResult<Option<AssistantReply>> {
        Ok(None)
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        if self.fail_list {
            Err(crate::error::UsecaseError::Session(
                "исполнитель down недоступен".into(),
            ))
        } else {
            Ok(self.sessions.clone())
        }
    }
    async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
}

/// Дефолт — живой (спавн дримера и фолбэк проектов), именованный down — отказной.
pub(crate) fn router(live: Arc<RecordingGw>, down: Arc<RecordingGw>) -> Arc<SessionRouter> {
    let live: Arc<dyn AgentSessionGateway> = live;
    let down: Arc<dyn AgentSessionGateway> = down;
    Arc::new(SessionRouter::new(
        "live",
        live,
        vec![("down".into(), down)],
        HashMap::new(),
    ))
}

/// Часы дримера: idle-порог 30 мин, now = 100_000_000.
pub(crate) struct DreamClock;
impl Clock for DreamClock {
    fn utc_minute(&self) -> UtcMinute {
        UtcMinute {
            minute_of_day: 100,
            weekday: 3,
        }
    }
    fn now_ms(&self) -> u64 {
        100_000_000
    }
}

/// Idle-кандидат дримера: финал есть, токены > 0, активность старше порога.
pub(crate) fn dreamer_target() -> SessionInfo {
    SessionInfo {
        id: "ses_target".into(),
        title: String::new(),
        agent: String::new(),
        directory: String::new(),
        updated_ms: 100_000_000 - 31 * 60_000,
        outcome: "succeeded".into(),
        total_tokens: 700,
        busy: false,
    }
}

/// Ловушка tracing: thread-local dispatcher, каждый tokio::test — свой поток.
struct EventTrap(Arc<Mutex<Vec<String>>>);

struct TrapFields(Arc<Mutex<Vec<String>>>);

impl tracing::field::Visit for TrapFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0
            .lock()
            .expect("trap lock")
            .push(format!("{}={}", field.name(), value));
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .lock()
            .expect("trap lock")
            .push(format!("{}={:?}", field.name(), value));
    }
}

impl tracing::Subscriber for EventTrap {
    fn enabled(&self, _meta: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _follows: &tracing::Id, _span: &tracing::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut TrapFields(Arc::clone(&self.0)));
    }
    fn enter(&self, _span: &tracing::Id) {}
    fn exit(&self, _span: &tracing::Id) {}
}

pub(crate) fn trap_events() -> (DefaultGuard, Arc<Mutex<Vec<String>>>) {
    let records = Arc::new(Mutex::new(Vec::new()));
    let guard =
        tracing::dispatcher::set_default(&tracing::Dispatch::new(EventTrap(Arc::clone(&records))));
    (guard, records)
}

pub(crate) fn assert_degraded(records: &[String], event: &str) {
    let plain = format!("event={event}");
    let quoted = format!("event=\"{event}\"");
    let has_event = records
        .iter()
        .any(|r| r.as_str() == plain.as_str() || r.as_str() == quoted.as_str());
    let has_executor = records.iter().any(|r| r.contains("down"));
    assert!(
        has_event && has_executor,
        "нет {event} с именем отказа: {records:?}"
    );
}
