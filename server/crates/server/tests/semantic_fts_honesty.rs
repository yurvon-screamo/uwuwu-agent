//! Интеграционный пин #1473 (срез 1): отказ FTS-запроса не молчит —
//! fulltext() пробрасывает Err в usecase, где секция рендерит
//! «— недоступен: …». Мастер до фикса глотал Err в Ok(vec![])
//! («честная пустота») — секция полнотекста исчезала из выдачи,
//! неотличимая от «совпадений нет» (ложно-негативный дедуп).
//!
//! Индукция отказа: data-файлы таблицы сносятся при живом манифесте —
//! open_table читает манифест, execute() падает на отсутствующих файлах.
//!
//! Крейт бинарный, lib-таргета нет: модули search подключаются
//! зеркалом tests/search/mod.rs (паттерн semantic_slug_escape).

mod search;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use search::semantic::{Embedder, EMBED_MODEL_DIMS};
use search::semantic_backend::LanceSemanticBackend;
use uwuwu_usecase::{AppendDocOutcome, SemanticBackend, UsecaseResult, WikiRepo};

const SLUG: &str = "Experience/platform/Пин честного полнотекста";
const DOC_MD: &str = "# Пин #1473\n\nRailway деплой монолита идёт мержем в master.";

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

/// Фиксированный мок-эмбеддер: нулевые векторы нужной размерности.
struct FixedEmbedder;

#[async_trait]
impl Embedder for FixedEmbedder {
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>> {
        Ok(docs
            .iter()
            .map(|_| vec![0.0f32; EMBED_MODEL_DIMS])
            .collect())
    }

    async fn embed_query(&self, _query: &str) -> UsecaseResult<Vec<f32>> {
        Ok(vec![0.0f32; EMBED_MODEL_DIMS])
    }
}

fn lance_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "uwuwu-semantic-1473-{}-{nanos}",
        std::process::id()
    ))
}

/// Guard чистит папку при любом исходе теста, паника включительно.
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn seeded_backend() -> (LanceSemanticBackend, TempDirGuard) {
    let guard = TempDirGuard(lance_dir());
    let dir = guard.0.clone();
    let wiki = Arc::new(FakeWiki {
        docs: HashMap::from([(SLUG.to_string(), (DOC_MD.to_string(), "sha-1473".to_string()))]),
    });
    let embeddings: Arc<dyn Embedder> = Arc::new(FixedEmbedder);
    let backend = LanceSemanticBackend::open(dir, wiki, None, embeddings)
        .await
        .expect("open lance backend");
    backend.sync_index(10).await.expect("seed sync ok");
    (backend, guard)
}

/// Снос data-файлов таблицы при живом манифесте: open_table succeeds,
/// execute() падает. Возвращает Table-директорию для диагностики.
fn break_data_files(volume: &std::path::Path) {
    let table_dir = volume.join("chunks.lance");
    let data_dir = table_dir.join("data");
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
        cyrillic
            .as_ref()
            .expect("fts ok")
            .iter()
            .any(|s| s == SLUG),
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
