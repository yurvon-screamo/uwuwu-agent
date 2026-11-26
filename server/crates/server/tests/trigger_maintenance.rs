//! Пин триггера обслуживания #1436: вызов maintain живёт в `sync_index`
//! (`changed > 0`), не у пользователя. Механические пины churn_maintenance.rs
//! зовут `maintain()` руками — если триггер выкинут из sync_index, прод
//! возвращается к линейному росту памяти, а они остаются зелёными.
//!
//! Отдельный тестовый таргет: cargo исполняет тестовые бинари последовательно,
//! здесь нет параллельных sync-тестов, эмитящих `semantic.maintain` — шпион
//! на глобальном дефолте ловит только события этого процесса-таргета.
//!
//! Крейт бинарный, lib-таргета нет: модули search подключаются #[path]-зеркалом
//! (паттерн tests/search/mod.rs).

pub mod search {
    #[allow(dead_code)]
    #[path = "../../src/search/fts.rs"]
    pub mod fts;

    #[allow(dead_code)]
    #[path = "../../src/search/query.rs"]
    pub mod query;

    #[allow(dead_code)]
    #[path = "../../src/search/semantic.rs"]
    pub mod semantic;

    #[path = "../../src/search/semantic_backend.rs"]
    pub mod semantic_backend;

    #[path = "../../src/search/maintenance.rs"]
    pub mod maintenance;

    #[path = "../search/support.rs"]
    pub mod support;
}

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use search::semantic::Embedder;
use search::semantic_backend::LanceSemanticBackend;
use search::support::{lance_dir, FixedEmbedder, TempDirGuard};
use uwuwu_usecase::{SemanticBackend, UsecaseResult, WikiRepo};

const DOCS: usize = 3;
const SLUGS: [&str; DOCS] = ["doc/a", "doc/b", "doc/c"];

/// Вики с мутабельным содержимым: churn двигает текст и sha всех доков —
/// sha-гейт синка пропускает их на реиндекс (прод-чурн «платформа сама
/// себя пишет»).
struct ChurnWiki {
    docs: Mutex<HashMap<String, (String, String)>>,
    round: Mutex<usize>,
}

impl ChurnWiki {
    fn new() -> Self {
        let docs = SLUGS
            .iter()
            .map(|s| (s.to_string(), (format!("# {s}\n\nраунд 0"), "sha-0".into())))
            .collect();
        Self {
            docs: Mutex::new(docs),
            round: Mutex::new(0),
        }
    }

    fn churn_all(&self) {
        let mut round = self.round.lock().expect("round lock");
        *round += 1;
        let n = *round;
        for (md, sha) in self.docs.lock().expect("docs lock").values_mut() {
            *md = format!("# док\n\nраунд {n} — новое содержимое");
            *sha = format!("sha-{n}");
        }
    }
}

#[async_trait]
impl WikiRepo for ChurnWiki {
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        Ok(self
            .docs
            .lock()
            .expect("docs lock")
            .get(slug)
            .map(|(md, _)| md.clone()))
    }

    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(self
            .docs
            .lock()
            .expect("docs lock")
            .keys()
            .filter(|s| s.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(self
            .docs
            .lock()
            .expect("docs lock")
            .iter()
            .map(|(s, (_, sha))| (s.clone(), sha.clone()))
            .collect())
    }

    async fn write_doc(&self, _slug: &str, _content: &str, _message: &str) -> UsecaseResult<()> {
        Ok(())
    }

    async fn append_doc(&self, _slug: &str, _line: &str, _message: &str) -> UsecaseResult<usize> {
        Ok(0)
    }
}

/// Шпион лог-событий: копит значения поля `event` этого таргета. Глобальный
/// дефолт (`set_global_default`) — события обслуживаются с любых тредов
/// рантайма/блокирующего пула; соседних эмитящих тестов в процессе нет
/// (отдельный таргет — см. шапку).
struct MaintainSpy {
    events: Mutex<Vec<String>>,
}

struct SpyVisit(Option<String>);

impl tracing::field::Visit for SpyVisit {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "event" {
            self.0 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "event" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

impl tracing::Subscriber for MaintainSpy {
    fn enabled(&self, _meta: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::Id {
        tracing::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut visit = SpyVisit(None);
        event.record(&mut visit);
        if let Some(name) = visit.0 {
            if name == "semantic.maintain" {
                self.events.lock().expect("spy lock").push(name);
            }
        }
    }

    fn enter(&self, _span: &tracing::Id) {}

    fn exit(&self, _span: &tracing::Id) {}

    fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

    fn register_callsite(
        &self,
        _meta: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }
}

/// Триггер: sync с чурном (changed > 0) эмитит `semantic.maintain` — по
/// событию на таблицу (chunks и doc_meta); стабильный конвергентный sync
/// (changed = 0) — ноль событий (условие триггера).
#[tokio::test]
async fn churn_sync_runs_maintenance_and_stable_sync_skips_it() {
    let guard = TempDirGuard(lance_dir("trigger"));
    let dir = guard.0.clone();
    let wiki = Arc::new(ChurnWiki::new());
    let embeddings: Arc<dyn Embedder> = Arc::new(FixedEmbedder);
    let backend = LanceSemanticBackend::open(dir, wiki.clone(), None, embeddings)
        .await
        .expect("open backend");

    let spy = Arc::new(MaintainSpy {
        events: Mutex::new(Vec::new()),
    });
    tracing::subscriber::set_global_default(spy.clone()).expect("global spy");

    // Чурн: changed > 0 → обслуживание обеих таблиц
    wiki.churn_all();
    let changed = backend.sync_index(DOCS).await.expect("churn sync");
    assert_eq!(changed, DOCS, "чурн реиндексит все доки");
    let events_after_churn = spy.events.lock().expect("spy lock").len();
    assert!(
        events_after_churn >= 1,
        "sync с изменениями запускает обслуживание (semantic.maintain): {events_after_churn}"
    );

    // Стабильный конвергентный sync: changed = 0 → обслуживания нет
    let before = spy.events.lock().expect("spy lock").len();
    let changed = backend.sync_index(DOCS).await.expect("stable sync");
    assert_eq!(changed, 0, "стабильные доки не реиндекссятся повторно");
    let events_after_stable = spy.events.lock().expect("spy lock").len();
    assert_eq!(
        events_after_stable, before,
        "sync без изменений не обслуживает (changed > 0 — условие триггера)"
    );
}
