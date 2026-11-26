//! Харнес ролевых пинов деградации (#1609): живой/отказной гейтвеи,
//! ловушка tracing-событий и кандидат дримера. Только in-memory, без сети.

use crate::error::UsecaseResult;
use crate::pipeline::cup_pins_tests::DreamerClock;
use crate::ports::{
    AgentSessionGateway, AssistantReply, Clock, Liveness, SessionInfo, SessionSpec,
};
use crate::router::SessionRouter;
use async_trait::async_trait;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

/// Idle-кандидат дримера: финал есть, токены > 0, активность старше порога.
/// Часы — общие с cup_pins_tests (дубля фикстуры нет).
pub(crate) fn dreamer_target() -> SessionInfo {
    let now = DreamerClock.now_ms();
    SessionInfo {
        id: "ses_target".into(),
        title: String::new(),
        agent: String::new(),
        directory: String::new(),
        updated_ms: now - 31 * 60_000,
        outcome: "succeeded".into(),
        total_tokens: 700,
        busy: false,
    }
}

/// Ловушка tracing: процесс-глобальный роутер с `Interest::always()` плюс
/// поток-локальный приёмник. `dispatcher::set_default` в tracing-core 0.1.36
/// кэширует `Interest::never()` на callsite, если первый заход на него случился
/// на потоке без диспетчера, — после этого событие ловящего потока молча
/// теряется (tokio-rs/tracing#3611; гонка в тестах #1661: 83/500 красных).
/// Роутер регистрирует все callsite'ы как always и сам решает, отдать ли
/// событие активной ловушке этого потока.
/// Поля одного события склеиваются в одну запись: имя отказа должно лежать
/// в том же событии, что и `event=<role>.list_degraded`, и ровно такой
/// список: иначе ассерт ловит чужой `executor=down` из роутера или superset.
struct EventTrap(Arc<Mutex<Vec<String>>>);

struct TrapFields<'a>(&'a mut Vec<String>);

impl tracing::field::Visit for TrapFields<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push(format!("{}={}", field.name(), value));
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{}={:?}", field.name(), value));
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
        let mut fields = Vec::new();
        event.record(&mut TrapFields(&mut fields));
        self.0.lock().expect("trap lock").push(fields.join(" | "));
    }
    fn enter(&self, _span: &tracing::Id) {}
    fn exit(&self, _span: &tracing::Id) {}
}

thread_local! {
    static TRAP: RefCell<Option<Arc<EventTrap>>> = const { RefCell::new(None) };
}

struct EventRouter;

impl tracing::Subscriber for EventRouter {
    fn enabled(&self, _meta: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn register_callsite(&self, _meta: &tracing::Metadata<'_>) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }
    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if let Some(trap) = TRAP.with(|t| t.borrow().clone()) {
            trap.event(event);
        }
    }
    fn enter(&self, _span: &tracing::Id) {}
    fn exit(&self, _span: &tracing::Id) {}
}

pub(crate) struct TrapGuard(Option<Arc<EventTrap>>);

impl Drop for TrapGuard {
    fn drop(&mut self) {
        TRAP.with(|t| *t.borrow_mut() = self.0.take());
    }
}

static ROUTER: std::sync::OnceLock<()> = std::sync::OnceLock::new();

pub(crate) fn trap_events() -> (TrapGuard, Arc<Mutex<Vec<String>>>) {
    ROUTER.get_or_init(|| {
        let _ = tracing::subscriber::set_global_default(EventRouter);
    });
    let records = Arc::new(Mutex::new(Vec::new()));
    let prev = TRAP.with(|t| {
        t.borrow_mut()
            .replace(Arc::new(EventTrap(Arc::clone(&records))))
    });
    (TrapGuard(prev), records)
}

/// Точное поле события: имя сравнивается целиком, а не по подстроке —
/// иначе префиксный вариант (`...list_degraded_v2`) проходил бы ассерт и
/// сдвиг имени события не ловился ни одним из ролевых пинов (проверено
/// мутацией в раунде тестирования).
fn exact_field(rec: &str, field: &str, value: &str) -> bool {
    let plain = format!("{field}={value}");
    let quoted = format!("{field}=\"{value}\"");
    rec.split(" | ").any(|f| f == plain || f == quoted)
}

pub(crate) fn has_exact_field(records: &[String], field: &str, value: &str) -> bool {
    records.iter().any(|r| exact_field(r, field, value))
}

pub(crate) fn is_exact_degraded(records: &[String], event: &str) -> bool {
    const FAILED: &str = "executors=[\"down\"]";
    records
        .iter()
        .any(|r| r.contains(FAILED) && exact_field(r, "event", event))
}

pub(crate) fn assert_degraded(records: &[String], event: &str) {
    const FAILED: &str = "executors=[\"down\"]";
    assert!(
        is_exact_degraded(records, event),
        "нет {event} с точным полем {FAILED} в той же записи: {records:?}"
    );
}
