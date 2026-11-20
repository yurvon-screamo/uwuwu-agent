//! Интеграционный пин #1392: слаг вики-дока с кавычками не рвёт
//! lance-предикат delete (`Unterminated string literal`) — semantic sync
//! индексирует такой док, док находится поиском, повторный sync — no-op.
//!
//! Крейт бинарный, lib-таргета нет: модули search подключаются
//! зеркалом tests/search/mod.rs (паттерн mock_route_coverage).

mod search;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use search::semantic::{Embedder, EMBED_MODEL_DIMS};
use search::semantic_backend::LanceSemanticBackend;
use uwuwu_usecase::{AppendDocOutcome, SemanticBackend, UsecaseResult, WikiRepo};

/// Слаг с обоими классами кавычек: апостроф рвёт одинарный SQL-литерал,
/// двойная кавычка — контроль (в одинарном литерале она безопасна).
const QUOTED_SLUG: &str = "Experience/platform/Фабрика uwuwu: хвосты amend'а \"merge\" коммита";
const DOC_MD: &str =
    "# Пин #1392\n\nТекст дока с amend'а и \"кавычками\" — чанки перезаливаются после delete.";

struct FakeWiki {
    docs: HashMap<String, (String, String)>,
}

#[async_trait]
impl WikiRepo for FakeWiki {
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        Ok(self.docs.get(slug).map(|(md, _)| md.clone()))
    }

    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(self
            .docs
            .keys()
            .filter(|slug| slug.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(self
            .docs
            .iter()
            .map(|(slug, (_, sha))| (slug.clone(), sha.clone()))
            .collect())
    }

    async fn write_doc(&self, _slug: &str, _content: &str, _message: &str) -> UsecaseResult<()> {
        Ok(())
    }

    async fn append_doc(
        &self,
        _slug: &str,
        _line: &str,
        _message: &str,
    ) -> UsecaseResult<AppendDocOutcome> {
        Ok(AppendDocOutcome {
            created: false,
            lines: 0,
        })
    }
}

/// Фиксированный мок-эмбеддер: нулевые векторы нужной размерности. Трейт
/// `Embedder` (#1363) — эмбеддинги внешний unmanaged-dep, мок трейта —
/// штатный приём тестов; ADR-110 не задет — её скоуп GitHub-транспорты.
struct FixedEmbedder;

#[async_trait]
impl Embedder for FixedEmbedder {
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>> {
        Ok(docs.iter().map(|_| vec![0.0f32; EMBED_MODEL_DIMS]).collect())
    }

    async fn embed_query(&self, _query: &str) -> UsecaseResult<Vec<f32>> {
        Ok(vec![0.0f32; EMBED_MODEL_DIMS])
    }
}

/// Lance-том в уникальной временной папке (без новой dev-dep tempfile).
fn lance_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "uwuwu-semantic-1392-{}-{nanos}",
        std::process::id()
    ))
}

/// Lance-том в уникальной временной папке (без новой dev-dep tempfile).
/// Guard чистит папку при любом исходе теста, паника включительно —
/// падающие прогоны не копят мусор в /tmp (замечание ревью #1392).
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn backend_with_quoted_doc() -> (LanceSemanticBackend, TempDirGuard) {
    let guard = TempDirGuard(lance_dir());
    let dir = guard.0.clone();
    let wiki = Arc::new(FakeWiki {
        docs: HashMap::from([(
            QUOTED_SLUG.to_string(),
            (DOC_MD.to_string(), "sha-1392".to_string()),
        )]),
    });
    let embeddings: Arc<dyn Embedder> = Arc::new(FixedEmbedder);
    let backend = LanceSemanticBackend::open(dir, wiki, None, embeddings)
        .await
        .expect("open lance backend");
    (backend, guard)
}

#[tokio::test]
async fn sync_indexes_doc_with_quotes_in_slug() {
    let (backend, _dir) = backend_with_quoted_doc().await;

    let indexed = backend.sync_index(10).await.expect("sync ok");

    assert_eq!(indexed, 1);
}

#[tokio::test]
async fn synced_doc_with_quotes_in_slug_is_found_by_search() {
    let (backend, _dir) = backend_with_quoted_doc().await;
    backend.sync_index(10).await.expect("sync ok");

    // Текст запроса произволен: стаб отдаёт константный нулевой вектор,
    // матчинг идёт по векторам, не по лексике (замечание ревью #1392).
    let hits = backend.search("аменд", 5).await.expect("search ok");

    assert!(hits.iter().any(|(_, slug)| slug == QUOTED_SLUG));
}

#[tokio::test]
async fn resync_of_unchanged_corpus_indexes_nothing() {
    let (backend, _dir) = backend_with_quoted_doc().await;
    backend.sync_index(10).await.expect("first sync ok");

    let indexed = backend.sync_index(10).await.expect("resync ok");

    assert_eq!(indexed, 0);
}
