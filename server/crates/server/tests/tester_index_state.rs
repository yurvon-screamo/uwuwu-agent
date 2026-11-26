//! Тест тестировщика #1687: событие `semantic.index_state` пишется каждым
//! тиком синка, включая тик без изменений — иначе stops growing неотличим
//! от часового backoff. Ни одного изменения прода.

mod search;

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex, Once, OnceLock};

use search::semantic_backend::LanceSemanticBackend;
use search::support::{lance_dir, FakeWiki, FixedEmbedder, TempDirGuard};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use uwuwu_usecase::SemanticBackend;

/// (docs, chunks) из каждого события semantic.index_state.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<(u64, u64)>>>);
static CAPTURE: OnceLock<Capture> = OnceLock::new();

fn capture() -> &'static Capture {
    CAPTURE.get_or_init(Capture::default)
}

fn init_subscriber() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(capture().clone()),
        )
        .expect("один глобальный subscriber на бинарник теста");
    });
}

fn number(debug: &str) -> Option<u64> {
    let s = debug.trim();
    let s = s
        .strip_prefix("Some(")
        .and_then(|v| v.strip_suffix(')'))
        .unwrap_or(s);
    s.parse().ok()
}

#[derive(Default)]
struct StateVisitor {
    is_state: bool,
    docs: Option<u64>,
    chunks: Option<u64>,
}

impl Visit for StateVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "event" && value == "semantic.index_state" {
            self.is_state = true;
        }
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "docs" => self.docs = Some(value),
            "chunks" => self.chunks = Some(value),
            _ => {}
        }
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        match field.name() {
            "docs" => self.docs = Some(value as u64),
            "chunks" => self.chunks = Some(value as u64),
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        let s = format!("{value:?}");
        match field.name() {
            "event" if s.contains("semantic.index_state") => self.is_state = true,
            "docs" => self.docs = number(&s),
            "chunks" => self.chunks = number(&s),
            _ => {}
        }
    }
}

impl<S> Layer<S> for Capture
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        // Зеркало `mod search;` тянет чужие тесты того же бинарника — их
        // события не должны попадать в счёт этого теста. Отсекаем по спану.
        if !ctx
            .event_span(event)
            .is_some_and(|span| span.metadata().name() == "tester-index-state")
        {
            return;
        }
        let mut visitor = StateVisitor::default();
        event.record(&mut visitor);
        if visitor.is_state {
            self.0
                .lock()
                .expect("lock")
                .push((visitor.docs.unwrap_or(0), visitor.chunks.unwrap_or(0)));
        }
    }
}

#[tokio::test]
async fn index_state_is_logged_on_tick_with_changes_and_on_empty_tick() {
    init_subscriber();
    let before = capture().0.lock().expect("lock").len();

    let guard = TempDirGuard(lance_dir("tester-index-state"));
    let dir = guard.0.clone();
    let wiki = Arc::new(FakeWiki {
        docs: HashMap::from([(
            "Experience/platform/Пин полноты индекса".to_string(),
            (
                "# Пин\n\nОдин документ, один чанк.".to_string(),
                "sha-state".to_string(),
            ),
        )]),
    });
    let backend = LanceSemanticBackend::open(dir, wiki, None, Arc::new(FixedEmbedder))
        .await
        .expect("open");

    let span = tracing::info_span!("tester-index-state");
    let _entered = span.enter();
    assert_eq!(backend.sync_index(10).await.expect("тик с изменениями"), 1);
    assert_eq!(backend.sync_index(10).await.expect("тик без изменений"), 0);

    let events = capture().0.lock().expect("lock")[before..].to_vec();
    assert_eq!(events.len(), 2, "по событию на тик, факт: {events:?}");
    assert_eq!(
        events[0], events[1],
        "счётчики(full) одинаковы на changed=0"
    );
    assert_eq!(events[0].0, 1, "docs в событии — число строк doc_meta");
    assert_eq!(events[0].1, 1, "chunks в событии — число строк chunks");
}
