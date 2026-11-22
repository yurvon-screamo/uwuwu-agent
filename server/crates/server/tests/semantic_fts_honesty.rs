//! Интеграционный пин #1473 (срез 1): отказ FTS-запроса не молчит —
//! fulltext() пробрасывает Err в usecase, где секция рендерит
//! «— недоступен: …». Мастер до фикса глотал Err в Ok(vec![])
//! («честная пустота») — секция полнотекста исчезала из выдачи,
//! неотличимая от «совпадений нет» (ложно-негативный дедуп).
//!
//! Индукция отказа: data-файлы таблицы сносятся при живом манифесте —
//! open_table читает манифест, execute() падает на отсутствующих файлах.
//! Санити заодно пиннит корневой фикс: FTS-индекс создаётся при открытии
//! бекенда, полнотекст ищет латиницу и кириллицу (simple-токенизатор).
//!
//! Крейт бинарный, lib-таргета нет: модули search подключаются
//! зеркалом tests/search/mod.rs (паттерн semantic_slug_escape).

mod search;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use search::semantic_backend::LanceSemanticBackend;
use search::support::{lance_dir, FakeWiki, FixedEmbedder, TempDirGuard};
use uwuwu_usecase::SemanticBackend;

const SLUG: &str = "Experience/platform/Пин честного полнотекста";
const DOC_MD: &str = "# Пин #1473\n\nRailway деплой монолита идёт мержем в master.";

async fn seeded_backend() -> (LanceSemanticBackend, TempDirGuard) {
    let guard = TempDirGuard(lance_dir("fts-honesty"));
    let dir = guard.0.clone();
    let wiki = Arc::new(FakeWiki {
        docs: HashMap::from([(
            SLUG.to_string(),
            (DOC_MD.to_string(), "sha-1473".to_string()),
        )]),
    });
    let backend = LanceSemanticBackend::open(dir, wiki, None, Arc::new(FixedEmbedder))
        .await
        .expect("open lance backend");
    backend.sync_index(10).await.expect("seed sync ok");
    (backend, guard)
}

/// Снос data-файлов таблицы при живом манифесте: open_table succeeds,
/// execute() падает на отсутствующих файлах.
fn break_data_files(volume: &Path) {
    let data_dir = volume.join("chunks.lance").join("data");
    std::fs::remove_dir_all(&data_dir).expect("drop data dir");
}

#[tokio::test]
async fn fts_finds_seeded_doc_before_breakage() {
    let (backend, _dir) = seeded_backend().await;

    let latin = backend.fulltext(&["railway".to_string()], 5).await;
    assert!(
        latin.as_ref().expect("fts ok").iter().any(|s| s == SLUG),
        "сеяный док находится FTS (латиница), факт: {latin:?}"
    );

    let cyrillic = backend.fulltext(&["монолита".to_string()], 5).await;
    assert!(
        cyrillic.as_ref().expect("fts ok").iter().any(|s| s == SLUG),
        "simple-токенизатор индекса покрывает кириллицу, факт: {cyrillic:?}"
    );
}

#[tokio::test]
async fn fts_query_failure_is_honest_error_not_silent_empty() {
    let (backend, dir) = seeded_backend().await;
    break_data_files(&dir.0);

    let hits = backend.fulltext(&["railway".to_string()], 5).await;

    // Мастер до фикса: Ok(vec![]) — «честная пустота», секция исчезает
    // из выдачи. Фикс: Err пробрасывается в usecase.
    assert!(
        hits.is_err(),
        "отказ FTS должен быть честным Err, факт: {hits:?}"
    );
}
