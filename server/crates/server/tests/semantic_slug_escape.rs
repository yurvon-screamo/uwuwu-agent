//! Интеграционный пин #1392: слаг вики-дока с кавычками не рвёт
//! lance-предикат delete (`Unterminated string literal`) — semantic sync
//! индексирует такой док, док находится поиском, повторный sync — no-op.
//!
//! Крейт бинарный, lib-таргета нет: модули search подключаются
//! зеркалом tests/search/mod.rs (паттерн mock_route_coverage).

mod search;

use std::collections::HashMap;
use std::sync::Arc;

use search::semantic::Embedder;
use search::semantic_backend::LanceSemanticBackend;
use search::support::{lance_dir, FakeWiki, FixedEmbedder, TempDirGuard};
use uwuwu_usecase::SemanticBackend;

/// Слаг с обоими классами кавычек: апостроф рвёт одинарный SQL-литерал,
/// двойная кавычка — контроль (в одинарном литерале она безопасна).
const QUOTED_SLUG: &str = "Experience/platform/Фабрика uwuwu: хвосты amend'а \"merge\" коммита";
const DOC_MD: &str =
    "# Пин #1392\n\nТекст дока с amend'а и \"кавычками\" — чанки перезаливаются после delete.";

/// Гейт-фикстура: бекенд с доком-кавычкой, временный том под гардом уборки.
async fn backend_with_quoted_doc() -> (LanceSemanticBackend, TempDirGuard) {
    let guard = TempDirGuard(lance_dir("1392"));
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
