//! Харнес офлайн-пинов pre-image журнала (#1609): временный каталог,
//! ловушка tracing-событий и разбор записанного JSON. Без сети и досок.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use uwuwu_usecase::StatusPreImage;

pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("uwuwu-rename-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path)
    }
    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

pub(crate) fn image(board: &str) -> StatusPreImage {
    StatusPreImage {
        board: board.into(),
        from: "Draft".into(),
        to: "Todo".into(),
        created_ms: 42,
        options: Vec::new(),
        distribution: HashMap::new(),
    }
}

/// Ловушка tracing-событий: thread-local dispatcher, каждый sync-тест —
/// свой поток, соседние тесты события не перехватывают.
struct EventTrap(Arc<Mutex<Vec<String>>>);

struct TrapFields(Arc<Mutex<Vec<String>>>);

impl tracing::field::Visit for TrapFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "event" {
            self.0.lock().expect("trap lock").push(value.to_string());
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "event" {
            let name = format!("{value:?}");
            self.0
                .lock()
                .expect("trap lock")
                .push(name.trim_matches('"').to_string());
        }
    }
}

impl tracing::Subscriber for EventTrap {
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        matches!(*meta.level(), tracing::Level::WARN | tracing::Level::ERROR)
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

pub(crate) fn trap_events() -> (tracing::dispatcher::DefaultGuard, Arc<Mutex<Vec<String>>>) {
    let records = Arc::new(Mutex::new(Vec::new()));
    let guard =
        tracing::dispatcher::set_default(&tracing::Dispatch::new(EventTrap(Arc::clone(&records))));
    (guard, records)
}

pub(crate) fn assert_event(records: &[String], name: &str) {
    assert!(
        records.iter().any(|r| r == name),
        "нет события {name}: {records:?}"
    );
}

pub(crate) fn board_keys(path: &Path) -> Vec<String> {
    let raw = std::fs::read_to_string(path).expect("журнал прочитан");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("журнал — JSON");
    json["by_board"]
        .as_object()
        .expect("by_board — объект")
        .keys()
        .cloned()
        .collect()
}
