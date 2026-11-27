//! Интеграционный пин #1436: чурн semantic-sync (delete+add каждого тика
//! по chunks/doc_meta) не копит Lance-версии/фрагменты/deletion-файлы
//! бесконечно — обслуживание (компакция + prune) ограничивает. Прод-факт:
//! read_version до 3946, ~750 коммитов за 1.5 ч, линейный anon-рост кучи.
//!
//! Пины:
//!   1. probe — чурн-петля копит версии (механика дефекта, red-факт);
//!   2. файл-бандс — обслуживание ограничивает файлы таблицы (дискриминатор
//!      фикса: без обслуживания в sync_index тест красный);
//!   3. ретеншен — параметр пути обслуживания: ноль в тесте схлопывает
//!      версии (замечание ревью плана: прод-константа в тесте не работает);
//!   4. сходимость — стабильный док не реиндексится повторно (защита от
//!      ложного механизма «хэш-гейт врёт»).
//!
//! Крейт бинарный, lib-таргета нет: модули search подключаются
//! зеркалом tests/search/mod.rs (паттерн dim_migration.rs).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::search::semantic::Embedder;
use crate::search::semantic_backend::LanceSemanticBackend;
use async_trait::async_trait;
use lancedb::connection::ConnectBuilder;
use uwuwu_usecase::{SemanticBackend, UsecaseResult, WikiRepo};

const DOCS: usize = 3;
const CHURN_ROUNDS: usize = 6;
const SLUGS: [&str; DOCS] = ["doc/a", "doc/b", "doc/c"];

// ——— фикстуры (паттерн dim_migration.rs) ———

struct FixedEmbedder;

#[async_trait]
impl Embedder for FixedEmbedder {
    async fn embed_docs(&self, docs: &[(String, String)]) -> UsecaseResult<Vec<Vec<f32>>> {
        Ok(docs
            .iter()
            .map(|_| vec![0.0f32; crate::search::semantic::search_dims()])
            .collect())
    }

    async fn embed_query(&self, _query: &str) -> UsecaseResult<Vec<f32>> {
        Ok(vec![0.0f32; crate::search::semantic::search_dims()])
    }
}

/// Вики с мутабельным содержимым: churn_all двигает текст и sha всех
/// доков — sha-гейт синка пропускает их на реиндекс (прод-чурн «платформа
/// сама себя пишет»).
struct ChurnWiki {
    docs: Mutex<HashMap<String, (String, String)>>,
    round: Mutex<usize>,
}

impl ChurnWiki {
    fn new() -> Self {
        let docs = SLUGS
            .iter()
            .map(|s| {
                (
                    s.to_string(),
                    (format!("# {s}\n\nраунд 0"), "sha-0".to_string()),
                )
            })
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
            .filter(|slug| slug.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(self
            .docs
            .lock()
            .expect("docs lock")
            .iter()
            .map(|(slug, (_, sha))| (slug.clone(), sha.clone()))
            .collect())
    }

    async fn write_doc(&self, _slug: &str, _content: &str, _message: &str) -> UsecaseResult<()> {
        Ok(())
    }

    async fn append_doc(&self, _slug: &str, _line: &str, _message: &str) -> UsecaseResult<usize> {
        Ok(0)
    }
}

fn lance_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("uwuwu-churn-{tag}-{}-{nanos}", std::process::id()))
}

/// Чистит папку при любом исходе теста, паника включительно.
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn open_backend(dir: PathBuf, wiki: Arc<ChurnWiki>) -> LanceSemanticBackend {
    let embeddings: Arc<dyn Embedder> = Arc::new(FixedEmbedder);
    LanceSemanticBackend::open(dir, wiki, None, embeddings)
        .await
        .expect("open backend")
}

async fn versions_of(dir: &Path, table: &str) -> usize {
    let db = ConnectBuilder::new(dir.to_str().expect("utf8 path"))
        .execute()
        .await
        .expect("connect");
    db.open_table(table)
        .execute()
        .await
        .expect("open table")
        .list_versions()
        .await
        .expect("list versions")
        .len()
}

fn count_files(dir: &Path) -> usize {
    let mut total = 0usize;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += count_files(&path);
            } else {
                total += 1;
            }
        }
    }
    total
}

/// Файлы данных таблицы: фрагменты + deletion-векторы (data/, _deletes/).
/// Manifest/_versions/_tx исключены — их по одному на коммит чистит prune
/// по ретеншену, а семейство файлов данных держит малым компакция (#1436).
fn count_data_files(dir: &Path) -> usize {
    const LEDGER_DIRS: [&str; 4] = ["_manifests", "_versions", "_tx", "_indices"];
    let mut total = 0usize;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !LEDGER_DIRS.contains(&name.as_ref()) {
                total += count_data_files(&path);
            }
        } else {
            total += 1;
        }
    }
    total
}

/// Чурн-петля: каждый раунд двигает все доки → полный реиндекс
/// (delete+add × chunks/doc_meta = 4 коммита на док).
async fn churn(backend: &LanceSemanticBackend, wiki: &ChurnWiki) {
    for _ in 0..CHURN_ROUNDS {
        wiki.churn_all();
        backend.sync_index(DOCS).await.expect("sync");
    }
}

// ——— пины ———

#[tokio::test]
async fn churn_accumulates_versions_without_bounds() {
    let guard = TempDirGuard(lance_dir("probe"));
    let dir = guard.0.clone();
    let wiki = Arc::new(ChurnWiki::new());
    let backend = open_backend(dir.clone(), wiki.clone()).await;

    churn(&backend, &wiki).await;

    let versions = versions_of(&dir, "chunks").await;
    // floor = половина теоретических коммитов (2 на док каждого раунда:
    // delete+add) — гард от слияния парных коммитов лансом
    let expected_floor = CHURN_ROUNDS * DOCS;
    assert!(
        versions >= expected_floor,
        "чурн копит версии (механика #1436): {versions} < {expected_floor}"
    );
}

#[tokio::test]
async fn churn_files_are_collapsed_by_zero_retention_maintain() {
    let guard = TempDirGuard(lance_dir("files"));
    let dir = guard.0.clone();
    let wiki = Arc::new(ChurnWiki::new());
    let backend = open_backend(dir.clone(), wiki.clone()).await;

    churn(&backend, &wiki).await;
    let data_files_before = count_data_files(&dir.join("chunks.lance"));
    let files_before = count_files(&dir.join("chunks.lance"));
    // красный факт дефекта: чурн разрос семейство файлов данных
    assert!(
        data_files_before >= CHURN_ROUNDS * DOCS,
        "чурн копит файлы данных (механика #1436): {data_files_before}"
    );

    backend.maintain(Duration::ZERO).await.expect("maintain");

    let versions = versions_of(&dir, "chunks").await;
    assert!(
        versions <= 3,
        "prune с нулевым ретеншеном чистит версии: {versions}"
    );
    let files_after = count_files(&dir.join("chunks.lance"));
    assert!(
        files_after < files_before,
        "prune чистит семейство файлов: {files_before} → {files_after}"
    );
    let data_files_after = count_data_files(&dir.join("chunks.lance"));
    assert!(
        data_files_after <= 8,
        "компакция сливает фрагменты: {data_files_before} → {data_files_after}"
    );
    let hits = backend.search("раунд", 5).await.expect("search");
    assert!(
        hits.iter().any(|(_, slug)| SLUGS.contains(&slug.as_str())),
        "данные пережили обслуживание"
    );
}

#[tokio::test]
async fn stable_docs_do_not_reindex_on_repeat_syncs() {
    let guard = TempDirGuard(lance_dir("converge"));
    let dir = guard.0.clone();
    let wiki = Arc::new(ChurnWiki::new());
    let backend = open_backend(dir.clone(), wiki.clone()).await;

    let changed_first = backend.sync_index(DOCS).await.expect("first sync");
    assert_eq!(changed_first, DOCS, "первый sync реиндексит все доки");
    let versions_first = versions_of(&dir, "chunks").await;

    let changed_second = backend.sync_index(DOCS).await.expect("second sync");
    assert_eq!(
        changed_second, 0,
        "стабильные доки не реиндексятся повторно (сходимость)"
    );
    let versions_second = versions_of(&dir, "chunks").await;
    assert_eq!(
        versions_first, versions_second,
        "повторный sync не создаёт коммитов"
    );
}

/// Soak (#1436, вне CI): долгий чурн с логом RSS и моделью прод-таймингов
/// (prune стреляет каждый «тик»). Семейство версий/файлов ограничено
/// (пин ниже); кривая RSS — в лог для оценки плато, плато на окне соука
/// не ассертится (флак): судья — прод server.memory (срез D) либо
/// heaptrack на стенде (fallback плана #1436).
/// Opt-in: UWUWU_SOAK=1 cargo test soak_ — иначе skip (гейт UWUWU_E2E #1363).
#[tokio::test]
async fn soak_churn_rss_plateaus_under_maintenance() {
    let Ok("1") = std::env::var("UWUWU_SOAK").as_deref() else {
        println!("soak skipped: UWUWU_SOAK=1 не задан");
        return;
    };
    let guard = TempDirGuard(lance_dir("soak"));
    let dir = guard.0.clone();
    let wiki = Arc::new(ChurnWiki::new());
    let backend = open_backend(dir.clone(), wiki.clone()).await;

    let rounds = 150;
    for r in 1..=rounds {
        wiki.churn_all();
        backend.sync_index(DOCS).await.expect("sync");
        // Модель прод-тайминга в сжатом времени: тик прода = 180с, к
        // следующему тику файлы старше ретеншена и prune чистит их.
        // Раунд соука ~0.7с — ретеншен 2с даёт ту же семантику
        // «семейство файлов ограничено окном ретеншена».
        backend
            .maintain(Duration::from_secs(2))
            .await
            .expect("maintain");
        if r % 15 == 0 {
            let rss_kb = std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("VmRSS:"))
                        .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
                })
                .unwrap_or(0);
            println!("soak round {r}: VmRSS {rss_kb} KB");
        }
    }
    let versions = versions_of(&dir, "chunks").await;
    println!("soak final: versions {versions}");
    assert!(
        versions <= 100,
        "soak: обслуживание ограничивает версии под чурном: {versions}"
    );
}
