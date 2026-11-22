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

const SLUG_A: &str = "Experience/platform/Пин полнотекста: база";
const SLUG_B: &str = "Experience/platform/Пин полнотекста: объём";
const SLUG_C: &str = "Experience/platform/Пин полнотекста: дельта";
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

/// Прод-форма первой развёртки (#1473, Low ревью кода): том с уже
/// наполненным корпусом без индекса (эпоха до фикса), бут нового кода
/// строит FTS-индекс по нему, дописанные после бута строки идут дельтой.
/// Пин: полный текст находит и индексированную базу, и дельту.
#[tokio::test]
async fn fulltext_covers_prefilled_corpus_and_post_boot_delta() {
    let guard = TempDirGuard(lance_dir("fts-prefill"));
    let dir = guard.0.clone();

    // Наполняем том и сносим индекс, созданный при open: в лансе чанки
    // без инвертированного индекса — состояние тома на момент бута фикса.
    {
        let wiki = Arc::new(FakeWiki {
            docs: HashMap::from([
                (
                    SLUG_A.to_string(),
                    (
                        "# A\n\nRailway деплой монолита идёт мержем.".to_string(),
                        "sha-a".into(),
                    ),
                ),
                (
                    SLUG_B.to_string(),
                    (
                        "# B\n\nМонолит живёт на Railway, объём /data.".to_string(),
                        "sha-b".into(),
                    ),
                ),
            ]),
        });
        let backend = LanceSemanticBackend::open(dir.clone(), wiki, None, Arc::new(FixedEmbedder))
            .await
            .expect("open #1");
        backend.sync_index(10).await.expect("corpus sync");
        drop(backend);
        drop_fts_index(&dir).await;
    }

    // Бут с фиксом по наполненному тому: индекс строится по существующему
    // корпусу; дельта — новый док, старые не реиндексируются (sha-гейт).
    let wiki = Arc::new(FakeWiki {
        docs: HashMap::from([
            (
                SLUG_A.to_string(),
                (
                    "# A\n\nRailway деплой монолита идёт мержем.".to_string(),
                    "sha-a".into(),
                ),
            ),
            (
                SLUG_B.to_string(),
                (
                    "# B\n\nМонолит живёт на Railway, объём /data.".to_string(),
                    "sha-b".into(),
                ),
            ),
            (
                SLUG_C.to_string(),
                (
                    "# C\n\nПреемпшен гасит low-приоритетные ходы по капу.".to_string(),
                    "sha-c".into(),
                ),
            ),
        ]),
    });
    let backend = LanceSemanticBackend::open(dir, wiki, None, Arc::new(FixedEmbedder))
        .await
        .expect("open #2 — бут по наполненному тому");
    let changed = backend.sync_index(10).await.expect("delta sync");
    assert_eq!(changed, 1, "дельта — только новый док");

    for (token, slug) in [
        ("railway", SLUG_A),
        ("объём", SLUG_B),
        ("преемпшен", SLUG_C),
    ] {
        let hits = backend.fulltext(&[token.to_string()], 5).await;
        assert!(
            hits.as_ref().expect("fts ok").iter().any(|s| s == slug),
            "полнотекст покрывает «{token}» (индексированная база и дельта), факт: {hits:?}"
        );
    }
}

/// Снос FTS-индексов таблицы чанков: из теста нужен отдельный коннект —
/// chunks() бекенда не виден (pub(super)).
async fn drop_fts_index(volume: &std::path::Path) {
    use lancedb::index::IndexType;
    let db = lancedb::connection::ConnectBuilder::new(volume.to_str().expect("utf8 path"))
        .execute()
        .await
        .expect("connect");
    let table = db
        .open_table("chunks")
        .execute()
        .await
        .expect("open chunks");
    for idx in table.list_indices().await.expect("indices") {
        if matches!(idx.index_type, IndexType::FTS) {
            table.drop_index(&idx.name).await.expect("drop fts index");
        }
    }
}
