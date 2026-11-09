//! Hermetic-тесты semantic-модуля: wiremock-kernel (динамическая пагинация
//! по keyset-курсору), wiremock-ollama (счётчик инпутов), tmp-sqlite.
//! Покрывают корректностное ядро задачи 20261104072044: keyset через run'ы
//! одинаковых updated, overlap, бюджет, reconcile-diff, guard'ы, деградацию
//! семантической половины и stmt-контракт (явный ORDER BY + LIMIT).

use crate::semantic::ollama::{normalize, EmbedError, OllamaClient};
use crate::semantic::store::{ChunkMeta, ChunkRecord, Store};
use crate::semantic::{sync, SemanticConfig};
use crate::siyuan::SiYuan;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

// ——— моки ———

type BlocksHandle = Arc<Mutex<Vec<Value>>>;
type StmtsHandle = Arc<Mutex<Vec<String>>>;

/// Динамический kernel-SQL мок: разборывает курсор stmt'а и нарезает
/// страницы датасета — честная пагинация, как ядро.
struct KernelMock {
    blocks: BlocksHandle,
    stmts: StmtsHandle,
}

impl KernelMock {
    fn new(blocks: Vec<Value>) -> (Self, BlocksHandle, StmtsHandle) {
        let blocks = Arc::new(Mutex::new(blocks));
        let stmts = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                blocks: blocks.clone(),
                stmts: stmts.clone(),
            },
            blocks,
            stmts,
        )
    }

    fn handle(&self, stmt: &str) -> Vec<Value> {
        let blocks = self.blocks.lock().unwrap();
        if stmt.contains("length(content)") {
            // state-метрика status: блоки за верхней границей длины
            let max = stmt
                .split("length(content) > ")
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .and_then(|v| v.parse::<usize>().ok())
                .expect("граница длины парсится");
            let count = blocks
                .iter()
                .filter(|b| b["content"].as_str().unwrap_or("").chars().count() > max)
                .count();
            return vec![json!({ "c": count })];
        }
        if stmt.contains("count(*)") {
            // честный count: строки над watermark-парой
            // (updated > wu or (updated = wu and id > wi))
            let after = stmt
                .split("updated > '")
                .nth(1)
                .and_then(|rest| rest.split('\'').next())
                .unwrap_or("0")
                .to_string();
            let id_marker = "and id > '";
            let id_after = stmt
                .split(id_marker)
                .nth(1)
                .and_then(|rest| rest.split('\'').next())
                .unwrap_or("")
                .to_string();
            let count = blocks
                .iter()
                .filter(|b| {
                    let updated = b["updated"].as_str().unwrap_or("").as_bytes();
                    let id = b["id"].as_str().unwrap_or("").as_bytes();
                    updated > after.as_bytes()
                        || (updated == after.as_bytes() && id > id_after.as_bytes())
                })
                .count();
            return vec![json!({ "c": count })];
        }
        if stmt.contains("updated > '") {
            // дельта: keyset по (updated, id)
            let (u, i) = delta_cursor(stmt).expect("курсор дельты парсится");
            let mut page: Vec<Value> = blocks
                .iter()
                .filter(|b| {
                    let updated = b["updated"].as_str().unwrap_or("");
                    let id = b["id"].as_str().unwrap_or("");
                    updated.as_bytes() > u.as_bytes()
                        || (updated.as_bytes() == u.as_bytes() && id.as_bytes() > i.as_bytes())
                })
                .cloned()
                .collect();
            page.sort_by_key(|b| {
                (
                    b["updated"].as_str().unwrap_or("").to_string(),
                    b["id"].as_str().unwrap_or("").to_string(),
                )
            });
            page.truncate(sync::PAGE);
            return page;
        }
        if stmt.contains("id in (") {
            let ids = ids_in(stmt);
            return blocks
                .iter()
                .filter(|b| ids.contains(&b["id"].as_str().unwrap_or("").to_string()))
                .cloned()
                .collect();
        }
        if stmt.contains("order by id asc") {
            // лёгкий скан reconcile: keyset по id
            let i = id_after(stmt).expect("курсор скана парсится");
            let mut page: Vec<Value> = blocks
                .iter()
                .filter(|b| b["id"].as_str().unwrap_or("").as_bytes() > i.as_bytes())
                .cloned()
                .collect();
            page.sort_by_key(|b| b["id"].as_str().unwrap_or("").to_string());
            page.truncate(sync::PAGE);
            return page;
        }
        panic!("kernel mock: нераспознанный stmt: {stmt}");
    }
}

fn delta_cursor(stmt: &str) -> Option<(String, String)> {
    let updated_marker = "updated > '";
    let u_start = stmt.find(updated_marker)? + updated_marker.len();
    let u = &stmt[u_start..];
    let u = &u[..u.find('\'')?];
    let id_marker = "and id > '";
    let i_start = stmt.find(id_marker)? + id_marker.len();
    let i = &stmt[i_start..];
    let i = &i[..i.find('\'')?];
    Some((u.to_string(), i.to_string()))
}

fn id_after(stmt: &str) -> Option<String> {
    let marker = "and id > '";
    let start = stmt.find(marker)? + marker.len();
    let rest = &stmt[start..];
    Some(rest[..rest.find('\'')?].to_string())
}

fn ids_in(stmt: &str) -> Vec<String> {
    let start = stmt.find("id in (").expect("id in") + "id in (".len();
    let rest = &stmt[start..];
    let end = rest.find(')').expect(")");
    rest[..end]
        .split(',')
        .map(|literal| literal.trim().trim_matches('\'').to_string())
        .collect()
}

impl Respond for KernelMock {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).expect("json тело kernel-запроса");
        let stmt = body["stmt"].as_str().unwrap_or("").to_string();
        self.stmts.lock().unwrap().push(stmt.clone());
        ResponseTemplate::new(200).set_body_json(json!({ "code": 0, "data": self.handle(&stmt) }))
    }
}

/// Embed-мок: один вектор на каждый инпут; считает присланные инпуты.
struct EmbedMock {
    vector: Vec<f32>,
    inputs: Arc<Mutex<Vec<String>>>,
    /// Задержка ответа — тесты таймаутов бюджета (п.2 ревью PR #78).
    delay: Duration,
    /// Дискретный режим (детерминизм вместо плотных таймингов, флейк
    /// 09.11): первый вызов — с `delay`, последующие — со `slow`.
    /// Признак «первого» — пустой счётчик инпутов до extend (батч шлёт
    /// все инпуты одним вызовом, len() == 1 не случится никогда).
    slow_after_first: Option<Duration>,
}

impl Respond for EmbedMock {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).expect("json тело embed");
        let inputs = body["input"].as_array().expect("input-массив");
        let sent: Vec<String> = inputs
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();
        let first_call = self.inputs.lock().unwrap().is_empty();
        self.inputs.lock().unwrap().extend(sent.iter().cloned());
        let delay = match self.slow_after_first {
            Some(slow) if !first_call => slow,
            _ => self.delay,
        };
        let embeddings: Vec<Vec<f32>> = inputs.iter().map(|_| self.vector.clone()).collect();
        ResponseTemplate::new(200)
            .set_body_json(json!({ "embeddings": embeddings }))
            .set_delay(delay)
    }
}

async fn kernel_server(
    blocks: Vec<Value>,
) -> (MockServer, Arc<Mutex<Vec<Value>>>, Arc<Mutex<Vec<String>>>) {
    let server = MockServer::start().await;
    let (kernel, blocks_handle, stmts) = KernelMock::new(blocks);
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(kernel)
        .mount(&server)
        .await;
    (server, blocks_handle, stmts)
}

async fn embed_server(vector: Vec<f32>) -> (MockServer, Arc<Mutex<Vec<String>>>) {
    embed_server_with_delay(vector, Duration::ZERO).await
}

async fn embed_server_with_delay(
    vector: Vec<f32>,
    delay: Duration,
) -> (MockServer, Arc<Mutex<Vec<String>>>) {
    embed_server_custom(vector, delay, None).await
}

/// Дискретный мок вместо плотных таймингов (флейк 09.11): первый вызов
/// отвечает мгновенно, последующие — через `slow`, заведомо больший
/// таймаута клиента. Исход теста фиксирован конструкцией мока, а не
/// победой одного реального тайминга над другим.
async fn embed_server_slow_after_first(
    vector: Vec<f32>,
    slow: Duration,
) -> (MockServer, Arc<Mutex<Vec<String>>>) {
    embed_server_custom(vector, Duration::ZERO, Some(slow)).await
}

async fn embed_server_custom(
    vector: Vec<f32>,
    delay: Duration,
    slow_after_first: Option<Duration>,
) -> (MockServer, Arc<Mutex<Vec<String>>>) {
    let server = MockServer::start().await;
    let inputs = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/api/embed"))
        .respond_with(EmbedMock {
            vector,
            inputs: inputs.clone(),
            delay,
            slow_after_first,
        })
        .mount(&server)
        .await;
    (server, inputs)
}

fn block(id: &str, updated: &str, content: &str, hpath: &str) -> Value {
    json!({
        "id": id,
        "root_id": format!("{id}-root"),
        "box": "20261103215252-xlpd6kj",
        "hpath": hpath,
        "content": content,
        "updated": updated,
        "type": "p",
    })
}

fn seeded_store(dir: &std::path::Path) -> Store {
    Store::open(&dir.join("semantic.db")).expect("tmp store")
}

fn ollama(uri: &str) -> OllamaClient {
    OllamaClient::new(uri, "test-model", Duration::from_secs(2)).expect("test ollama")
}

fn opts_no_deadline() -> sync::SyncOptions {
    sync::SyncOptions {
        since: None,
        deadline: None,
        embed_timeout: Duration::from_secs(2),
        page_delay: None,
        on_batch: None,
    }
}

const T0: &str = "20261105000000";
const T1: &str = "20261105000001";

// ——— store / ollama unit ———

#[test]
fn normalize_l2() {
    let normalized = normalize(vec![3.0, 4.0]);
    assert!((normalized[0] - 0.6).abs() < 1e-6);
    assert!((normalized[1] - 0.8).abs() < 1e-6);
}

#[tokio::test]
async fn store_upsert_is_idempotent_and_meta_roundtrips() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());
    let record = ChunkRecord {
        meta: ChunkMeta {
            block_id: "b1".into(),
            root_id: "r".into(),
            box_id: "box".into(),
            hpath: "/h".into(),
            updated: T0.into(),
        },
        content: "контент чанка".into(),
        vec: normalize(vec![1.0, 0.0]),
    };
    for _ in 0..2 {
        let tx = store.begin().expect("tx");
        Store::upsert_batch(&tx, std::slice::from_ref(&record), Some(T0)).expect("upsert");
        tx.commit().expect("commit");
    }
    assert_eq!(store.chunk_count().unwrap(), 1);
    store.set_meta("model", "m").unwrap();
    assert_eq!(store.meta("model").unwrap().as_deref(), Some("m"));
    // WAL включён при открытии — параллельные читатели не блокируются.
    assert_eq!(store.journal_mode().unwrap(), "wal");
}

#[tokio::test]
async fn ollama_embed_guards_dim_and_reports_missing_model() {
    let (server, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let client = ollama(&server.uri());

    let vectors = client
        .embed(
            &["один".into(), "два".into()],
            Some(3),
            Duration::from_secs(2),
        )
        .await
        .expect("embed ок");
    assert_eq!(vectors.len(), 2);
    assert_eq!(inputs.lock().unwrap().len(), 2);

    let mismatch = client
        .embed(&["три".into()], Some(1024), Duration::from_secs(2))
        .await
        .expect_err("dim-guard");
    assert!(matches!(
        mismatch,
        EmbedError::DimMismatch {
            index_dim: 1024,
            got_dim: 3
        }
    ));
    assert!(mismatch.to_string().contains("semantic rebuild"));

    let dead = OllamaClient::new("http://127.0.0.1:1", "test-model", Duration::from_secs(1))
        .expect("client");
    let unreachable = dead
        .embed(&["q".into()], None, Duration::from_secs(1))
        .await
        .expect_err("route");
    assert!(matches!(unreachable, EmbedError::Unreachable { .. }));

    let missing_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/embed"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": "model \"test-model\" not found, try pulling it first"
        })))
        .mount(&missing_server)
        .await;
    let missing = ollama(&missing_server.uri())
        .embed(&["q".into()], None, Duration::from_secs(2))
        .await
        .expect_err("404");
    assert!(matches!(missing, EmbedError::ModelMissing { .. }));
    assert!(missing.to_string().contains("ollama pull test-model"));
}

// ——— дельта: keyset / overlap / бюджет / stmt-контракт ———

#[tokio::test]
async fn delta_pages_through_equal_timestamp_run_without_loss() {
    // 520 строк: 300 с одинаковым updated (run булк-импорта, разрезается
    // границей страниц), далее вторая секунда; каждая 10-я — короткая.
    let mut blocks = Vec::new();
    for i in 0..520 {
        let updated = if i < 300 { T0 } else { T1 };
        let content = if i % 10 == 0 {
            "коротко".to_string()
        } else {
            format!("чанк номер {i} с содержанием длиннее сорока символов")
        };
        blocks.push(block(
            &format!("20261105-{i:04}"),
            updated,
            &content,
            "/test/doc",
        ));
    }
    let (server, _, stmts) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());

    let report = sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("delta");

    // 520 строк, 52 короткие (i=0,10,…,510) — не индексируем, не теряем.
    assert_eq!(report.skipped_short, 52);
    assert_eq!(report.embedded, 468);
    assert_eq!(store.chunk_count().unwrap(), 468);
    let (mark_updated, mark_id) = sync::split_watermark(&store.meta("watermark").unwrap().unwrap());
    assert_eq!(mark_updated, T1);
    assert_eq!(mark_id, "20261105-0519", "clean-end пишет курсор-пару");
    assert_eq!(store.meta("dim").unwrap().as_deref(), Some("3"));
    // каждый чанк эмбедден ровно один раз — 0 потерь, 0 дублей
    assert_eq!(inputs.lock().unwrap().len(), 468);
    // stmt-контракт: каждая страница несёт keyset-курсор, ORDER BY и LIMIT
    let captured = stmts.lock().unwrap();
    assert!(
        captured.len() >= 3,
        "страниц минимум три: {:?}",
        captured.len()
    );
    for stmt in captured.iter() {
        if stmt.contains("(updated >") {
            // дельта-скан: keyset по (updated, id), без content в выборке
            assert!(
                stmt.contains("order by updated asc, id asc"),
                "ORDER BY: {stmt}"
            );
            assert!(
                stmt.contains(&format!("limit {}", sync::PAGE)),
                "LIMIT: {stmt}"
            );
            assert!(
                !stmt.contains(", content,"),
                "скан не таскает content: {stmt}"
            );
        } else {
            // точечный fetch по PK
            assert!(stmt.contains("order by"), "ORDER BY: {stmt}");
            assert!(stmt.contains("limit"), "LIMIT: {stmt}");
        }
    }
}

#[tokio::test]
async fn delta_overlap_reembeds_same_second_edit_after_watermark() {
    let blocks = vec![
        block(
            "a1",
            T0,
            "первый чанк с длинным содержанием для индексации",
            "/d",
        ),
        block(
            "a2",
            T0,
            "второй чанк с длинным содержанием для индексации",
            "/d",
        ),
        block(
            "a3",
            T0,
            "третий чанк с длинным содержанием для индексации",
            "/d",
        ),
    ];
    let (server, blocks_handle, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());

    sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("первая дельта");
    let (mark_updated, _) = sync::split_watermark(&store.meta("watermark").unwrap().unwrap());
    assert_eq!(mark_updated, T0);

    // Правка a2 в ту же секунду, что watermark — после прохода курсора.
    {
        let mut kernel_blocks = blocks_handle.lock().unwrap();
        kernel_blocks[1] = block(
            "a2",
            T0,
            "второй чанк ОТРЕДАКТИРОВАН, содержание всё ещё длинное",
            "/d",
        );
    }
    let report = sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("вторая дельта");
    // отредактированная a2 переэмбеддится (content разошёлся), нетронутые
    // a1/a3 — skip-unchanged без переэмбедда (пара в watermark различает)
    assert_eq!(
        report.embedded, 1,
        "overlap-окно переэмбеддит только правку"
    );
    assert_eq!(store.chunk_count().unwrap(), 3);
    let reembedded = inputs.lock().unwrap();
    assert!(reembedded.iter().any(|c| c.contains("ОТРЕДАКТИРОВАН")));
}

#[tokio::test]
async fn delta_deadline_exhausted_embeds_nothing() {
    let blocks = vec![block(
        "a1",
        T0,
        "чанк с длинным содержанием для эмбеддинга",
        "/d",
    )];
    let (server, _) = kernel_server_blocks_only(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());

    let report = sync::delta_sync(
        &sy,
        &ollama(&embed.uri()),
        &mut store,
        &sync::SyncOptions {
            since: None,
            deadline: Some(Instant::now() - Duration::from_millis(1)),
            embed_timeout: Duration::from_secs(2),
            page_delay: None,
            on_batch: None,
        },
    )
    .await
    .expect("дельта не падает от бюджета");

    assert!(report.budget_exhausted);
    assert_eq!(report.embedded, 0);
    assert_eq!(store.chunk_count().unwrap(), 0);
    assert!(inputs.lock().unwrap().is_empty());
}

// ——— skip-unchanged: идемпотентность дельты без переэмбедда ———

/// Строка, уже проиндексированная с теми же (updated, content), не
/// переэмбеддится: overlap-перескан проходит её бесплатно, курсор двигается.
#[tokio::test]
async fn delta_skips_reembed_of_identical_rows() {
    let content = "чанк с длинным содержанием, уже в индексе без изменений";
    let blocks = vec![block("c1", T1, content, "/d")];
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());
    seed_indexed_chunk_at(&mut store, "c1", T1, content, "/d");
    store
        .set_meta("watermark", &sync::watermark_composite(T0, "seed"))
        .unwrap();

    let report = sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("дельта");

    assert_eq!(
        inputs.lock().unwrap().len(),
        0,
        "идентичная строка не переэмбеддится"
    );
    assert_eq!(report.embedded, 0);
    let mark = store.meta("watermark").unwrap().unwrap();
    assert_eq!(
        sync::split_watermark(&mark),
        (T1.to_string(), "c1".to_string()),
        "курсор прошёл пропущенную строку: {mark}"
    );
}

/// Дрейф путей (hpath изменился при равных updated и content) чинится
/// метаданными без переэмбедда — свежесть hpath не деградирует до суток.
#[tokio::test]
async fn delta_refreshes_path_drift_without_reembed() {
    let content = "чанк с длинным содержанием, переехавший между доками";
    let blocks = vec![block("c1", T1, content, "/new-place")];
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());
    seed_indexed_chunk_at(&mut store, "c1", T1, content, "/old-place");
    store
        .set_meta("watermark", &sync::watermark_composite(T0, "seed"))
        .unwrap();

    let report = sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("дельта");

    assert_eq!(inputs.lock().unwrap().len(), 0, "переэмбедда нет");
    assert_eq!(report.paths_refreshed, 1, "дрейф путей освежён");
    let hpath = store
        .chunk_metas()
        .expect("меты чанков")
        .into_iter()
        .find(|m| m.block_id == "c1")
        .expect("чанк на месте")
        .hpath;
    assert_eq!(hpath, "/new-place");
}

/// Анти-карусельный сценарий (диагноз 20261106182055): лавина одной
/// секунды, префикс закоммичен прошлым прогоном (пара T1|0031) —
/// следующий прогон переэмбеддит только незакрытый хвост, курсор доходит
/// до конца лавины.
#[tokio::test]
async fn delta_mountain_run2_does_not_reembed_committed_prefix() {
    let blocks: Vec<Value> = (0..300)
        .map(|i| {
            block(
                &format!("20261105-{i:04}"),
                T1,
                &format!("чанк лавины номер {i} с содержанием длиннее сорока символов"),
                "/d",
            )
        })
        .collect();
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());
    // «Прогон 1»: префикс 0000..0031 закоммичен, курсор стоит на 0031
    for i in 0..32 {
        seed_indexed_chunk_at(
            &mut store,
            &format!("20261105-{i:04}"),
            T1,
            &format!("чанк лавины номер {i} с содержанием длиннее сорока символов"),
            "/d",
        );
    }
    store
        .set_meta("watermark", &sync::watermark_composite(T1, "20261105-0031"))
        .unwrap();

    let report = sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("дельта");

    {
        let embedded = inputs.lock().unwrap();
        assert_eq!(
            embedded.len(),
            268,
            "переэмбедден только хвост 0032..0299, не префикс: {}",
            embedded.len()
        );
        assert!(!embedded.iter().any(|c| c.contains("номер 0 ")));
    }
    assert_eq!(report.embedded, 268);
    let mark = store.meta("watermark").unwrap().unwrap();
    assert_eq!(
        sync::split_watermark(&mark),
        (T1.to_string(), "20261105-0299".to_string()),
        "курсор дошёл до конца лавины: {mark}"
    );
    let backlog = crate::semantic::status_backlog_for_tests(&sy, &store).await;
    assert_eq!(backlog, 0, "очередь закрыта");
}

async fn kernel_server_blocks_only(blocks: Vec<Value>) -> (MockServer, Arc<Mutex<Vec<Value>>>) {
    let (server, blocks_handle, _) = kernel_server(blocks).await;
    (server, blocks_handle)
}

// ——— pair-watermark: лавина одной секунды (диагноз 20261106182055) ———

/// Водяной знак несёт пару (updated, id): курсор дельты не теряет id-компоненту
/// при записи — иначе лавина блоков одной секунды (булк-реимпорт тела задачи)
/// замораживает прогресс: per-batch watermark равен текущему, MAX-гвард
/// отвергает равенство, очередь «висит» вечно (факт: 267 блоков секунды
/// 20261106223122, watermark стоял на ней же).
#[tokio::test]
async fn delta_watermark_pair_persists_after_full_mountain_pass() {
    // 300 блоков одной секунды T1; watermark стоит на T1 (как оставил бы
    // clean-end, пишущий голый updated без id-компоненты курсора).
    let blocks: Vec<Value> = (0..300)
        .map(|i| {
            block(
                &format!("20261105-{i:04}"),
                T1,
                &format!("чанк лавины номер {i} с содержанием длиннее сорока символов"),
                "/d",
            )
        })
        .collect();
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, _) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());
    store.set_meta("watermark", T1).unwrap();

    sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("дельта");

    let mark = store.meta("watermark").unwrap().unwrap();
    let (updated, id) = sync::split_watermark(&mark);
    assert_eq!(updated, T1, "секунда курсора — секунда лавины");
    assert_eq!(
        id, "20261105-0299",
        "id-компонента курсора сохранена: watermark = {mark}"
    );
}

/// Смерть посреди лавины (embed-таймаут второго батча — детерминированный
/// дискретный мок вместо плотных таймингов, флейк 09.11) — закоммиченный
/// батч двигает пару: watermark указывает на последнюю строку
/// закоммиченного батча, backlog считает незакрытый хвост.
#[tokio::test]
async fn delta_mid_mountain_embed_death_keeps_pair_progress() {
    let blocks: Vec<Value> = (0..300)
        .map(|i| {
            block(
                &format!("20261105-{i:04}"),
                T1,
                &format!("чанк лавины номер {i} с содержанием длиннее сорока символов"),
                "/d",
            )
        })
        .collect();
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    // батч 1 (32 строки) — мгновенный ответ, батчи 2+ — через 5 s при
    // таймауте 500 ms: Err(EmbedError::Timeout) гарантирован конструкцией
    // мока, а не гонкой «успеет ли батч в бюджет» (бывшие 25 ms vs 100 ms).
    let (embed, _inputs) =
        embed_server_slow_after_first(vec![1.0, 0.0, 0.0], Duration::from_secs(5)).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());
    store.set_meta("watermark", T1).unwrap();

    let outcome = sync::delta_sync(
        &sy,
        &ollama(&embed.uri()),
        &mut store,
        &sync::SyncOptions {
            since: None,
            deadline: None,
            embed_timeout: Duration::from_millis(500),
            page_delay: None,
            on_batch: None,
        },
    )
    .await;
    let Err(err) = outcome else {
        panic!("смерть посреди лавины: ожидали Err от таймаута второго батча");
    };

    let is_timeout = err
        .downcast_ref::<EmbedError>()
        .is_some_and(|e| matches!(e, EmbedError::Timeout { .. }));
    assert!(is_timeout, "ожидали EmbedError::Timeout: {err:#}");

    // ровно первый батч закоммичен — счётчик инпутов мока здесь не нужен:
    // смерть детерминирована, коммит только у батча 1
    let committed = store.chunk_count().unwrap() as usize;
    assert_eq!(
        committed,
        sync::EMBED_BATCH,
        "ровно первый батч: {committed}"
    );
    let mark = store.meta("watermark").unwrap().unwrap();
    let (updated, id) = sync::split_watermark(&mark);
    assert_eq!(updated, T1);
    assert_eq!(
        id, "20261105-0031",
        "пара указывает на последнюю закоммиченную строку: {mark}"
    );

    // backlog над парой — незакрытый хвост лавины, а не вся секунда
    let backlog = crate::semantic::status_backlog_for_tests(&sy, &store).await;
    assert_eq!(
        backlog,
        (300 - sync::EMBED_BATCH) as u64,
        "очередь над парой: {backlog}"
    );
}

/// Пара монотонна и по id внутри секунды: откат id-компоненты не двигает
/// курсор назад (resume с overlap-окном переезает секунды повторно).
#[test]
fn advance_watermark_pair_is_monotonic_within_second() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = seeded_store(dir.path());

    store
        .advance_watermark(&sync::watermark_composite(T1, "20261105-0100"))
        .unwrap();
    store
        .advance_watermark(&sync::watermark_composite(T1, "20261105-0001"))
        .unwrap();
    assert_eq!(
        store.meta("watermark").unwrap().as_deref(),
        Some("20261105000001|20261105-0100"),
        "откат id внутри секунды отвергнут"
    );
    store
        .advance_watermark(&sync::watermark_composite(T1, "20261105-9999"))
        .unwrap();
    assert_eq!(
        store.meta("watermark").unwrap().as_deref(),
        Some("20261105000001|20261105-9999"),
        "id вперёд внутри той же секунды принят"
    );
}

/// Repair (reconcile) пишет watermark той же парой, но назад её двигать
/// нельзя: stale-строка с updated ниже курсора отвергается MAX-гвардом,
/// дельта не теряет хвост над парой (примечание ревью плана).
#[tokio::test]
async fn repair_does_not_rewind_watermark_pair() {
    let blocks = vec![block(
        "old-row",
        T0,
        "старый чанк с длинным содержанием для ремонта",
        "/d",
    )];
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, _) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());
    let ahead = sync::watermark_composite(T1, "20261105-5000");
    store.set_meta("watermark", &ahead).unwrap();

    let outcome = sync::repair_missing(
        &sy,
        &ollama(&embed.uri()),
        &mut store,
        &["old-row".into()],
        None,
    )
    .await
    .expect("repair");

    assert_eq!(outcome.repaired, 1, "строка закоммичена в индекс");
    assert_eq!(
        store.meta("watermark").unwrap().as_deref(),
        Some(ahead.as_str()),
        "пара не откатилась stale-строкой"
    );
}

// ——— reconcile: backfill / prune / дрейф ———

#[tokio::test]
async fn reconcile_backfills_prunes_and_refreshes_paths() {
    let blocks = vec![
        block(
            "b1",
            T0,
            "первый чанк с длинным содержанием для индексации",
            "/old",
        ),
        block(
            "b2",
            T0,
            "второй чанк с длинным содержанием для индексации",
            "/old",
        ),
        block(
            "b3",
            T0,
            "третий чанк с длинным содержанием для индексации",
            "/old",
        ),
        block(
            "b4",
            T0,
            "четвёртый чанк с длинным содержанием для индексации",
            "/old",
        ),
    ];
    let (server, blocks_handle, stmts) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());

    sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("seed");
    inputs.lock().unwrap().clear();
    // reconcile только что прошёл внутри дельты? нет — дельта его не звала;
    // помечаем прошлый reconcile просроченным.
    store.set_meta("last_reconcile_at", "1000").unwrap();

    // Ядро изменилось: b4 исчез, b5 добавлен, b2 переехал (hpath), b3 изменён.
    {
        let mut kernel = blocks_handle.lock().unwrap();
        kernel.retain(|b| b["id"].as_str() != Some("b4"));
        kernel.push(block(
            "b5",
            T1,
            "новый чанк с длинным содержанием для индексации",
            "/new",
        ));
        let moved = kernel
            .iter_mut()
            .find(|b| b["id"].as_str() == Some("b2"))
            .unwrap();
        moved["hpath"] = json!("/moved");
        let edited = kernel
            .iter_mut()
            .find(|b| b["id"].as_str() == Some("b3"))
            .unwrap();
        *edited = block(
            "b3",
            T1,
            "третий чанк ИЗМЕНЁН с длинным содержанием",
            "/old",
        );
    }

    let report = sync::reconcile(&sy, &ollama(&embed.uri()), &mut store, None)
        .await
        .expect("reconcile");

    assert_eq!(report.pruned, 1, "b4 исчез из ядра");
    assert_eq!(report.paths_refreshed, 1, "b2 переехал без эмбеддинга");
    assert_eq!(report.repaired, 2, "b5 backfill + b3 переэмбедд");
    assert_eq!(store.chunk_count().unwrap(), 4, "b1..b3 + b5");
    // эмбедд зазван только для ремонта, не для дрейфа путей
    let embedded = inputs.lock().unwrap();
    assert_eq!(embedded.len(), 2);
    assert!(embedded.iter().any(|c| c.contains("новый чанк")));
    assert!(embedded.iter().any(|c| c.contains("ИЗМЕНЁН")));
    assert!(!embedded.iter().any(|c| c.contains("второй чанк")));
    // stmt-контракт: LIMIT — в каждом stmt; ORDER BY — в каждом
    // пагинационном (count-агрегат не пагинируется)
    for stmt in stmts.lock().unwrap().iter() {
        assert!(stmt.contains("limit"), "LIMIT: {stmt}");
        if !stmt.contains("count(*)") {
            assert!(stmt.contains("order by"), "ORDER BY: {stmt}");
        }
    }
    assert!(store.meta("last_reconcile_at").unwrap().is_some());
}

#[tokio::test]
async fn reconcile_skips_within_daily_interval() {
    let (server, _) = kernel_server_blocks_only(vec![]).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());
    let fresh = chrono::Utc::now().timestamp();
    store
        .set_meta("last_reconcile_at", &fresh.to_string())
        .unwrap();

    let report = sync::reconcile(&sy, &ollama(&embed.uri()), &mut store, None)
        .await
        .expect("reconcile");

    assert!(report.skipped);
    assert!(inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reconcile_interrupted_scan_is_noop_not_mass_prune() {
    // High ревью PR #96: прерванный бюджетом скан — рутинное событие
    // поискового пути (бюджет 10s, живой бэклог). Частичный kernel против
    // индекса даёт ложные vanished (весь несосканированный хвост) —
    // прерванный прогон обязан быть no-op: без prune, без last_reconcile_at.
    let blocks = vec![block(
        "b1",
        T0,
        "первый чанк с длинным содержанием для индексации",
        "/d",
    )];
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());

    sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("seed");
    assert_eq!(store.chunk_count().unwrap(), 1);
    inputs.lock().unwrap().clear();
    store.set_meta("last_reconcile_at", "1000").unwrap();

    let report = sync::reconcile(
        &sy,
        &ollama(&embed.uri()),
        &mut store,
        Some(Instant::now() - Duration::from_millis(1)),
    )
    .await
    .expect("reconcile");

    assert_eq!(report.pruned, 0, "частичный скан не prune'ит индекс");
    assert_eq!(report.repaired, 0, "частичный скан не ремонтирует");
    assert_eq!(
        store.chunk_count().unwrap(),
        1,
        "чанк пережил прерванный прогон"
    );
    assert_eq!(
        store.meta("last_reconcile_at").unwrap().as_deref(),
        Some("1000"),
        "прерванный прогон не засчитывается"
    );
    assert!(inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reconcile_interrupted_repair_does_not_count_run() {
    // Common ревью PR #96: частичный backfill (бюджет умер посреди repair)
    // не должен проставлять last_reconcile_at — дыры ретраятся следующим
    // поиском, а не через сутки.
    let (server, _) = kernel_server_blocks_only(vec![block(
        "b1",
        T0,
        "первый чанк с длинным содержанием для индексации",
        "/d",
    )])
    .await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());
    store.set_meta("last_reconcile_at", "1000").unwrap();

    let outcome = sync::repair_missing(
        &sy,
        &ollama(&embed.uri()),
        &mut store,
        &["b1".to_string()],
        Some(Instant::now() - Duration::from_millis(1)),
    )
    .await
    .expect("repair_missing");

    assert!(!outcome.completed, "частичный outcome помечен");
    assert_eq!(outcome.repaired, 0);
    assert!(inputs.lock().unwrap().is_empty());
    assert_eq!(
        store.meta("last_reconcile_at").unwrap().as_deref(),
        Some("1000")
    );
}

// ——— п.1 stale-чанки: классификация по длине (мин./макс. граница) ———

#[tokio::test]
async fn reconcile_prunes_block_shrunk_below_min_chars() {
    // Ревьюверский сценарий п.1: длинный блок проиндексирован, затем усох
    // до ≤ MIN — reconcile обязан вычистить его из индекса, а не гонять
    // холостой repair→drop ежедневно.
    let blocks = vec![block(
        "b1",
        T0,
        "первый чанк с длинным содержанием для индексации",
        "/d",
    )];
    let (server, blocks_handle, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());

    sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("seed");
    assert_eq!(store.chunk_count().unwrap(), 1);
    inputs.lock().unwrap().clear();

    {
        let mut kernel = blocks_handle.lock().unwrap();
        *kernel
            .iter_mut()
            .find(|b| b["id"].as_str() == Some("b1"))
            .unwrap() = block("b1", T1, "короткий", "/d");
    }
    store.set_meta("last_reconcile_at", "1000").unwrap();

    let report = sync::reconcile(&sy, &ollama(&embed.uri()), &mut store, None)
        .await
        .expect("reconcile");

    assert_eq!(
        store.chunk_count().unwrap(),
        0,
        "усохший чанк удалён из индекса"
    );
    assert_eq!(report.pruned, 1, "усохший блок учтён в prune");
    assert!(
        inputs.lock().unwrap().is_empty(),
        "усохший блок не переэмбеддится"
    );
}

#[tokio::test]
async fn delta_prunes_block_shrunk_below_min_chars() {
    // Тот же усохший блок в overlap-окне дельты: старый чанк уходит
    // из индекса сразу, не дожидаясь суточного reconcile.
    let blocks = vec![block(
        "b1",
        T0,
        "первый чанк с длинным содержанием для индексации",
        "/d",
    )];
    let (server, blocks_handle, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());

    sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("seed");
    assert_eq!(store.chunk_count().unwrap(), 1);
    inputs.lock().unwrap().clear();

    {
        let mut kernel = blocks_handle.lock().unwrap();
        *kernel
            .iter_mut()
            .find(|b| b["id"].as_str() == Some("b1"))
            .unwrap() = block("b1", T0, "короткий", "/d");
    }

    let report = sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("delta");

    assert_eq!(
        store.chunk_count().unwrap(),
        0,
        "усохший чанк удалён в той же дельте"
    );
    assert_eq!(report.skipped_short, 1);
    assert_eq!(report.pruned, 1);
    assert!(inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn delta_skips_oversize_block_without_indexing() {
    // Блок за верхней границей (html_embed-стена): не индексируется,
    // счётчик per-run растёт, эмбедд-маршрут не тратится.
    let oversize = "х".repeat(sync::MAX_CONTENT_CHARS + 1);
    let blocks = vec![block("big", T0, &oversize, "/d")];
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());

    let report = sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("delta");

    assert_eq!(store.chunk_count().unwrap(), 0, "оверсайз не в индексе");
    assert_eq!(report.skipped_long, 1);
    assert!(inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reconcile_skips_oversize_backfill() {
    // Backfill оверсайза не происходит: блок в ядре, в индексе его нет —
    // и это финальное состояние, а не вечный repair-кандидат.
    let oversize = "х".repeat(sync::MAX_CONTENT_CHARS + 1);
    let blocks = vec![block("big", T0, &oversize, "/d")];
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());

    let report = sync::reconcile(&sy, &ollama(&embed.uri()), &mut store, None)
        .await
        .expect("reconcile");

    assert_eq!(report.repaired, 0, "оверсайз не попадает в backfill");
    assert_eq!(store.chunk_count().unwrap(), 0);
    assert!(inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reconcile_prunes_block_grown_beyond_max_chars() {
    // Проиндексированный блок вырос за MAX: старый вектор больше не
    // представляет содержимое — prune, не переэмбедд головы.
    let medium = "х".repeat(5000);
    let blocks = vec![block("b1", T0, &medium, "/d")];
    let (server, blocks_handle, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = seeded_store(dir.path());

    sync::delta_sync(&sy, &ollama(&embed.uri()), &mut store, &opts_no_deadline())
        .await
        .expect("seed");
    assert_eq!(store.chunk_count().unwrap(), 1);
    inputs.lock().unwrap().clear();

    {
        let oversize = "х".repeat(sync::MAX_CONTENT_CHARS + 1);
        let mut kernel = blocks_handle.lock().unwrap();
        *kernel
            .iter_mut()
            .find(|b| b["id"].as_str() == Some("b1"))
            .unwrap() = block("b1", T1, &oversize, "/d");
    }
    store.set_meta("last_reconcile_at", "1000").unwrap();

    let report = sync::reconcile(&sy, &ollama(&embed.uri()), &mut store, None)
        .await
        .expect("reconcile");

    assert_eq!(
        store.chunk_count().unwrap(),
        0,
        "переросший MAX чанк удалён"
    );
    assert_eq!(report.pruned, 1);
    assert!(inputs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn status_reports_oversize_blocks_by_target_types() {
    // State-метрика status: оверсайз-блоки корпуса, stmt несёт фильтр
    // целевых типов (тот же, что у индексации) и явный LIMIT.
    let oversize = "х".repeat(sync::MAX_CONTENT_CHARS + 1);
    let blocks = vec![
        block("big", T0, &oversize, "/d"),
        block(
            "b1",
            T0,
            "обычный чанк с длинным содержанием для индексации",
            "/d",
        ),
    ];
    let (server, _, stmts) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = SemanticConfig {
        embed_url: "http://127.0.0.1:1".into(),
        embed_model: "test-model".into(),
        db_path: dir.path().join("semantic.db"),
    };
    Store::open(&cfg.db_path).expect("пустой индекс для status");

    let lines = crate::semantic::status(&sy, &cfg).await.expect("status");

    let line = lines
        .iter()
        .find(|l| l.contains("не индексируются"))
        .expect("строка метрики оверсайза");
    assert!(line.contains('1'), "оверсайз-блок один: {line}");
    let captured = stmts.lock().unwrap();
    let metric_stmt = captured
        .iter()
        .find(|s| s.contains("length(content)"))
        .expect("stmt метрики ушёл в ядро");
    assert!(
        metric_stmt.contains("type in"),
        "фильтр целевых типов в метрике: {metric_stmt}"
    );
    assert!(metric_stmt.contains("limit"), "явный LIMIT: {metric_stmt}");
}

// ——— search_half: матрица деградации и happy path ———

fn config(dir: &std::path::Path, embed_url: &str) -> SemanticConfig {
    SemanticConfig {
        embed_url: embed_url.to_string(),
        embed_model: "test-model".into(),
        db_path: dir.join("semantic.db"),
    }
}

fn index_ready_store(dir: &std::path::Path) -> Store {
    let store = seeded_store(dir);
    store.set_meta("model", "test-model").unwrap();
    store.set_meta("dim", "3").unwrap();
    let fresh = chrono::Utc::now().timestamp();
    store
        .set_meta("last_reconcile_at", &fresh.to_string())
        .unwrap();
    store.set_meta("watermark", T1).unwrap();
    store
}

#[tokio::test]
async fn search_half_reports_missing_index_with_rebuild_hint() {
    let (server, _) = kernel_server_blocks_only(vec![]).await;
    let sy = SiYuan::for_tests(server.uri());
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = config(dir.path(), "http://127.0.0.1:1");

    let lines = crate::semantic::search_half(&sy, &cfg, "запрос", 10, 0.35).await;

    assert!(lines[0].starts_with("Семантический (test-model, cos ≥ 0.35):"));
    assert!(lines[1].contains("индекс пуст"));
    assert!(lines[1].contains("semantic rebuild"));
}

#[tokio::test]
async fn search_half_reports_model_mismatch() {
    let (server, _) = kernel_server_blocks_only(vec![]).await;
    let sy = SiYuan::for_tests(server.uri());
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = index_ready_store(dir.path());
    store.set_meta("model", "другая-модель").unwrap();
    let chunk = ChunkRecord {
        meta: ChunkMeta {
            block_id: "c1".into(),
            root_id: "r".into(),
            box_id: "b".into(),
            hpath: "/h".into(),
            updated: T0.into(),
        },
        content: "контент".into(),
        vec: normalize(vec![1.0, 0.0, 0.0]),
    };
    let tx = store.begin().unwrap();
    Store::upsert_batch(&tx, &[chunk], Some(T0)).unwrap();
    tx.commit().unwrap();

    let lines = crate::semantic::search_half(
        &sy,
        &config(dir.path(), "http://127.0.0.1:1"),
        "q",
        10,
        0.35,
    )
    .await;

    assert!(lines[1].contains("другой моделью"));
    assert!(lines[1].contains("semantic rebuild"));
}

// ——— п.2 бюджет поиска: wall-clock ≤ 10s, честный таймаут ———

fn seed_indexed_chunk(dir: &std::path::Path) {
    let mut store = index_ready_store(dir);
    let chunk = ChunkRecord {
        meta: ChunkMeta {
            block_id: "c1".into(),
            root_id: "r".into(),
            box_id: "b".into(),
            hpath: "/h".into(),
            updated: T0.into(),
        },
        content: "чанк с длинным содержанием для индексации".into(),
        vec: normalize(vec![1.0, 0.0, 0.0]),
    };
    let tx = store.begin().unwrap();
    Store::upsert_batch(&tx, &[chunk], Some(T0)).unwrap();
    tx.commit().unwrap();
}

/// Положить чанк в индекс от руки (срезы skip-unchanged/дрейфа).
fn seed_indexed_chunk_at(
    store: &mut Store,
    block_id: &str,
    updated: &str,
    content: &str,
    hpath: &str,
) {
    let chunk = ChunkRecord {
        meta: ChunkMeta {
            block_id: block_id.into(),
            root_id: "r".into(),
            box_id: "b".into(),
            hpath: hpath.into(),
            updated: updated.into(),
        },
        content: content.into(),
        vec: normalize(vec![1.0, 0.0, 0.0]),
    };
    let tx = store.begin().unwrap();
    Store::upsert_batch(&tx, &[chunk], None).unwrap();
    tx.commit().unwrap();
}

#[tokio::test]
async fn sync_command_drains_queue_and_reports() {
    // Ручная дотяжка вне бюджета поиска: очередь из трёх свежих строк
    // закоммичена, отчёт содержит счётчики и нулевой backlog.
    let blocks = vec![
        block(
            "s1",
            "20261105000010",
            "первый свежий чанк с длинным содержанием для индексации",
            "/d",
        ),
        block(
            "s2",
            "20261105000011",
            "второй свежий чанк с длинным содержанием для индексации",
            "/d",
        ),
        block(
            "s3",
            "20261105000012",
            "третий свежий чанк с длинным содержанием для индексации",
            "/d",
        ),
    ];
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, _) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = config(dir.path(), &embed.uri());
    let mut store = index_ready_store(dir.path());
    seed_indexed_chunk_at(
        &mut store,
        "seed",
        T0,
        "чанк с длинным содержанием для индексации",
        "/d",
    );
    drop(store);

    let lines = crate::semantic::sync_once(&sy, &cfg).await.expect("sync");

    assert!(
        lines.iter().any(|l| l.contains("3 чанков")),
        "отчёт дельты: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("очередь дельты: 0")),
        "очередь закрыта: {lines:?}"
    );
    let store = seeded_store(dir.path());
    let (updated, id) = sync::split_watermark(&store.meta("watermark").unwrap().unwrap());
    assert_eq!(updated, "20261105000012");
    assert_eq!(id, "s3", "курсор-пара на последней строке: {id}");
}

#[tokio::test]
async fn sync_command_refuses_while_rebuild_lock_held() {
    // Advisory-лок: синк на сотни секунд записей не должен пересекаться
    // с rebuild'ом — громкий отказ, а не взаимная порча индекса.
    let (server, _) = kernel_server_blocks_only(vec![]).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, _) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = config(dir.path(), &embed.uri());
    let mut store = index_ready_store(dir.path());
    seed_indexed_chunk_at(
        &mut store,
        "seed",
        T0,
        "чанк с длинным содержанием для индексации",
        "/d",
    );
    store.acquire_rebuild_lock().expect("чужой прогон");
    drop(store);

    let err = crate::semantic::sync_once(&sy, &cfg)
        .await
        .expect_err("лок держит живой прогон");
    assert!(err.to_string().contains("уже идёт"), "{err}");
}

#[tokio::test]
async fn sync_command_refuses_empty_index() {
    let (server, _) = kernel_server_blocks_only(vec![]).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, _) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = config(dir.path(), &embed.uri());

    let err = crate::semantic::sync_once(&sy, &cfg)
        .await
        .expect_err("пустой индекс");
    assert!(err.to_string().contains("индекс пуст"), "{err}");
}

#[tokio::test]
async fn search_half_returns_results_when_sync_budget_dies() {
    // Query-first: эмбедд запроса — первым (полный бюджет), sync — в
    // остатке. Бюджет умер посреди синка — секция отдаёт РЕЗУЛЬТАТЫ и
    // примечание о дельте, а не «таймаут: синхронизация не уложилась»
    // (пункт «Что сделать» задачи 20261106182055: результаты не заложники
    // синхронизации).
    let mut blocks = Vec::new();
    for i in 0..64 {
        blocks.push(block(
            &format!("fresh-{i:02}"),
            "20261105000010",
            &format!("свежий чанк очереди номер {i} с содержанием длиннее сорока символов"),
            "/queue",
        ));
    }
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    // query (вызов 1) — мгновенный: query-first успевает из полного
    // бюджета с огромным запасом; sync-батчи — через 2 s при остатке
    // 150 ms: таймаут синка детерминирован конструкцией мока, а не гонкой
    // «успеет ли query за бюджет» (бывшие 60 ms vs 150 ms, флейк 09.11).
    let (embed, _) =
        embed_server_slow_after_first(vec![1.0, 0.0, 0.0], Duration::from_secs(2)).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = config(dir.path(), &embed.uri());
    let mut store = index_ready_store(dir.path());
    let hit = ChunkRecord {
        meta: ChunkMeta {
            block_id: "hit-doc".into(),
            root_id: "r".into(),
            box_id: "b".into(),
            hpath: "/важный/док".into(),
            updated: T0.into(),
        },
        content: "важный документ с ответом на запрос".into(),
        vec: normalize(vec![1.0, 0.0, 0.0]),
    };
    let tx = store.begin().unwrap();
    Store::upsert_batch(&tx, &[hit], None).unwrap();
    tx.commit().unwrap();
    drop(store);

    let section = crate::semantic::search_half_at(
        &sy,
        &cfg,
        &crate::semantic::SearchHalfRequest {
            query: "важный запрос",
            limit: 10,
            threshold: 0.35,
        },
        Instant::now() + Duration::from_millis(150),
    )
    .await;

    assert!(
        section.iter().any(|l| l.starts_with("1. /важный/док")),
        "результаты в секции несмотря на смерть синка: {section:?}"
    );
    assert_eq!(
        section.last().map(String::as_str),
        Some("дельта не дотянулась в остатке бюджета — очередь дотянется следующими поисками"),
        "примечание о дельте — последняя строка секции: {section:?}"
    );
    assert!(!section.iter().any(|l| l.contains("недоступен")));
}

#[tokio::test]
async fn search_half_bounds_query_embed_timeout_by_remaining_budget() {
    // Дельта пуста, на эмбедд запроса осталось ~200ms, embed-мок отвечает
    // через 2s: Timeout, wall-clock ≈ остаток бюджета — не полный таймаут.
    let (server, _) = kernel_server_blocks_only(vec![]).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, _) = embed_server_with_delay(vec![1.0, 0.0, 0.0], Duration::from_secs(2)).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = config(dir.path(), &embed.uri());
    seed_indexed_chunk(dir.path());

    let started = Instant::now();
    let section = crate::semantic::search_half_at(
        &sy,
        &cfg,
        &crate::semantic::SearchHalfRequest {
            query: "запрос",
            limit: 10,
            threshold: 0.35,
        },
        started + Duration::from_millis(200),
    )
    .await;
    let elapsed = started.elapsed();

    assert!(
        section.iter().any(|l| l.contains("таймаут (бюджет 10s)")),
        "секция: {section:?}"
    );
    assert!(
        section.iter().any(|l| l.contains("не уложился в остаток")),
        "честный факт остатка: {section:?}"
    );
    assert!(!section.iter().any(|l| l.contains("недоступен")));
    assert!(
        elapsed < Duration::from_secs(1),
        "wall-clock ≈ остаток бюджета, не 2s/10s: {elapsed:?}"
    );
}

#[tokio::test]
async fn embed_timeout_is_distinct_from_unreachable() {
    // Классификация: reqwest-таймаут → Timeout (медленный ответ/холодный
    // старт), не Unreachable (лежащая Ollama) — статус не вводит в
    // заблуждение (п.2 ревью PR #78).
    let (server, _) = embed_server_with_delay(vec![1.0, 0.0, 0.0], Duration::from_secs(2)).await;
    let client =
        OllamaClient::new(&server.uri(), "test-model", Duration::from_secs(5)).expect("client");

    let err = client
        .embed(&["запрос".into()], None, Duration::from_millis(100))
        .await
        .expect_err("таймаут");

    assert!(matches!(err, EmbedError::Timeout { .. }), "{err}");
    assert!(err.to_string().contains("таймаут"));
}

// ——— п.3 resume: model-guard ———

#[tokio::test]
async fn rebuild_resume_refuses_model_mismatch() {
    // Прерванный билд моделью A + resume при env-модели B молча дособрал бы
    // индекс смешанными эмбеддингами (п.3 ревью PR #78) — громкий отказ.
    let blocks = vec![block(
        "a2",
        T1,
        "второй чанк с длинным содержанием для индексации",
        "/d",
    )];
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = SemanticConfig {
        embed_url: embed.uri(),
        embed_model: "model-b".into(),
        db_path: dir.path().join("semantic.db"),
    };
    let mut store = seeded_store(dir.path());
    store.set_meta("model", "model-a").unwrap();
    let partial = ChunkRecord {
        meta: ChunkMeta {
            block_id: "a0".into(),
            root_id: "r".into(),
            box_id: "b".into(),
            hpath: "/d".into(),
            updated: T0.into(),
        },
        content: "первый чанк с длинным содержанием для индексации".into(),
        vec: normalize(vec![1.0, 0.0, 0.0]),
    };
    let tx = store.begin().unwrap();
    Store::upsert_batch(&tx, &[partial], Some(T0)).unwrap();
    tx.commit().unwrap();
    drop(store);

    let err = match crate::semantic::rebuild(&sy, &cfg, None, true).await {
        Ok(_) => panic!("model mismatch на resume — ожидание отказа"),
        Err(e) => e,
    };

    let message = err.to_string();
    assert!(message.contains("model-a"), "индексная модель: {message}");
    assert!(message.contains("model-b"), "env-модель: {message}");
    assert!(inputs.lock().unwrap().is_empty(), "смешение не началось");
    let store = seeded_store(dir.path());
    assert_eq!(store.chunk_count().unwrap(), 1, "чанки не тронуты");
}

#[tokio::test]
async fn rebuild_resume_adopts_missing_model_meta() {
    // Сбой до первого батча: чанки есть, meta.model нет — resume adopt'ит
    // env-модель (семантика свежей БД) и продолжает.
    let blocks = vec![block(
        "a2",
        T1,
        "второй чанк с длинным содержанием для индексации",
        "/d",
    )];
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, _) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = SemanticConfig {
        embed_url: embed.uri(),
        embed_model: "test-model".into(),
        db_path: dir.path().join("semantic.db"),
    };
    let mut store = seeded_store(dir.path());
    let partial = ChunkRecord {
        meta: ChunkMeta {
            block_id: "a0".into(),
            root_id: "r".into(),
            box_id: "b".into(),
            hpath: "/d".into(),
            updated: T0.into(),
        },
        content: "первый чанк с длинным содержанием для индексации".into(),
        vec: normalize(vec![1.0, 0.0, 0.0]),
    };
    let tx = store.begin().unwrap();
    Store::upsert_batch(&tx, &[partial], Some(T0)).unwrap();
    tx.commit().unwrap();
    drop(store);
    assert_eq!(seeded_store(dir.path()).meta("model").unwrap(), None);

    crate::semantic::rebuild(&sy, &cfg, None, true)
        .await
        .expect("resume после adopt");

    let store = seeded_store(dir.path());
    assert_eq!(
        store.meta("model").unwrap().as_deref(),
        Some("test-model"),
        "env-модель adopt'нута в meta"
    );
}

// ——— п.4 rebuild-lock: advisory-лок в таблице locks ———

#[test]
fn rebuild_lock_refuses_second_acquisition_for_live_pid() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = seeded_store(dir.path());

    store.acquire_rebuild_lock().expect("первый acquire");
    let err = store
        .acquire_rebuild_lock()
        .expect_err("второй acquire при живом прогоне");

    assert!(err.to_string().contains("уже идёт"), "{err}");
}

#[test]
fn rebuild_lock_steals_from_dead_pid() {
    // Крэш прежнего прогона: устаревший lock снимается следующим запуском.
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = seeded_store(dir.path());
    store
        .place_lock("99999999:1700000000")
        .expect("мёртвый pid");

    store.acquire_rebuild_lock().expect("steal от мёртвого pid");

    let payload = store
        .lock_payload()
        .expect("payload")
        .expect("лок на месте");
    let pid = payload.split(':').next().expect("pid в payload");
    assert_eq!(
        pid,
        std::process::id().to_string(),
        "лок перезаписан текущим процессом"
    );
}

#[test]
fn rebuild_lock_release_allows_fresh_acquisition() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = seeded_store(dir.path());

    store.acquire_rebuild_lock().expect("acquire");
    store.release_rebuild_lock().expect("release");
    store.acquire_rebuild_lock().expect("acquire после release");
}

#[test]
fn rebuild_lock_survives_clear_for_rebuild() {
    // Пиннящий тест дыры (ревью плана v1, High): лок в стираемой meta
    // переживал clear только до первого fresh-прогона — два параллельных
    // fresh-запуска оба проходили. Лок живёт в таблице locks вне меты.
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = seeded_store(dir.path());

    store.acquire_rebuild_lock().expect("acquire");
    store.clear_for_rebuild().expect("clear");
    let err = store
        .acquire_rebuild_lock()
        .expect_err("второй fresh-прогон после clear");

    assert!(err.to_string().contains("уже идёт"), "{err}");
}

#[tokio::test]
async fn search_half_degrades_loudly_when_ollama_down() {
    let (server, _) = kernel_server_blocks_only(vec![]).await;
    let sy = SiYuan::for_tests(server.uri());
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = index_ready_store(dir.path());
    let chunk = ChunkRecord {
        meta: ChunkMeta {
            block_id: "c1".into(),
            root_id: "r".into(),
            box_id: "b".into(),
            hpath: "/h".into(),
            updated: T0.into(),
        },
        content: "контент".into(),
        vec: normalize(vec![1.0, 0.0, 0.0]),
    };
    let tx = store.begin().unwrap();
    Store::upsert_batch(&tx, &[chunk], Some(T0)).unwrap();
    tx.commit().unwrap();
    drop(store);

    // Порт 1 — connection refused, полнотекст бы ответил (ядро-мок жив).
    let lines = crate::semantic::search_half(
        &sy,
        &config(dir.path(), "http://127.0.0.1:1"),
        "q",
        10,
        0.35,
    )
    .await;

    assert!(lines[1].contains("недоступен"));
    assert!(lines[1].contains("Ollama"));
}

#[tokio::test]
async fn search_half_ranks_aligned_chunk_and_reports_empty_above_threshold() {
    let (server, blocks) = kernel_server_blocks_only(vec![]).await;
    blocks.lock().unwrap().clear();
    let sy = SiYuan::for_tests(server.uri());
    let dir = tempfile::tempdir().expect("tmpdir");
    let mut store = index_ready_store(dir.path());
    let aligned = ChunkRecord {
        meta: ChunkMeta {
            block_id: "aligned".into(),
            root_id: "r".into(),
            box_id: "b".into(),
            hpath: "/rathole/туннель".into(),
            updated: T0.into(),
        },
        content: "туннель до ноутбука через релей".into(),
        vec: normalize(vec![0.0, 2.0, 0.0]),
    };
    let orthogonal = ChunkRecord {
        meta: ChunkMeta {
            block_id: "orthogonal".into(),
            root_id: "r".into(),
            box_id: "b".into(),
            hpath: "/noise".into(),
            updated: T0.into(),
        },
        content: "совсем про другое содержание чанка".into(),
        vec: normalize(vec![2.0, 0.0, 0.0]),
    };
    let tx = store.begin().unwrap();
    Store::upsert_batch(&tx, &[aligned, orthogonal], Some(T0)).unwrap();
    tx.commit().unwrap();
    drop(store);

    // Запрос эмбеддится в [0, 2, 0] → косинус 1.0 с aligned, 0 с orthogonal.
    let (embed, _) = embed_server(vec![0.0, 2.0, 0.0]).await;
    let lines =
        crate::semantic::search_half(&sy, &config(dir.path(), &embed.uri()), "туннель", 10, 0.35)
            .await;

    assert!(
        lines[1].starts_with("1. /rathole/туннель  (1.00)"),
        "строки: {lines:?}"
    );
    assert!(lines[2].contains("туннель"));
    assert_eq!(
        lines.len(),
        3,
        "orthogonal ниже порога и не попадает в выдачу"
    );

    let (other_embed, _) = embed_server(vec![0.0, 0.0, 2.0]).await;
    let empty = crate::semantic::search_half(
        &sy,
        &config(dir.path(), &other_embed.uri()),
        "мимо",
        10,
        0.35,
    )
    .await;
    assert!(empty[1].contains("ничего выше порога 0.35"));
}

// ——— Tier-L: реальная Ollama (opt-in, канон /testing/Component Testing:
// gate APP_TEST_LLM=1, pre-flight /api/show, никогда auto-pull) ———

#[tokio::test]
async fn tier_l_real_ollama_ranks_relevant_chunk_higher() {
    if std::env::var("APP_TEST_LLM").ok().as_deref() != Some("1") {
        eprintln!("skip: APP_TEST_LLM=1 не задан (opt-in Tier-L)");
        return;
    }
    let base =
        std::env::var("UWUWU_EMBED_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let model =
        std::env::var("APP_TEST_LLM_MODEL").unwrap_or_else(|_| "qwen3-embedding:0.6b".into());

    let preflight = reqwest::Client::new()
        .post(format!("{base}/api/show"))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .expect("pre-flight /api/show");
    assert!(
        preflight.status().is_success(),
        "модели «{model}» нет на сервере — ollama pull {model} (auto-pull запрещён каноном)"
    );

    let client = OllamaClient::new(&base, &model, Duration::from_secs(60)).expect("client");
    let query = "как протянуть защищённый туннель до ноутбука через публичный релей".to_string();
    let relevant = "rathole-клиент устанавливает туннель до сервера через TCP-релей, пробрасывая локальный порт ноутбука в публичный endpoint".to_string();
    let irrelevant =
        "канбан-доска задач хранит строки с колонками Title, Status, Branch и PR в SiYuan database"
            .to_string();
    let vectors = client
        .embed(
            &[query, relevant, irrelevant],
            None,
            Duration::from_secs(60),
        )
        .await
        .expect("живой эмбеддинг");
    let query_vec = &vectors[0];
    let cosine = |v: &[f32]| v.iter().zip(query_vec).map(|(a, b)| a * b).sum::<f32>();
    assert!(
        cosine(&vectors[1]) > cosine(&vectors[2]),
        "релевантный абзац (rathole) должен ранжироваться выше нерелевантного (канбан): {} vs {}",
        cosine(&vectors[1]),
        cosine(&vectors[2])
    );
}

#[tokio::test]
async fn rebuild_resume_continues_from_watermark_without_reembedding() {
    // Частичный индекс после сбоя: a0 записан давно (вне overlap-окна),
    // a1 в секунду сбоя, a2/a3 не доезжали. Watermark = T0.
    let t_old = "20261104235800"; // T0 − 120 s: за границей overlap
    let blocks = vec![
        block(
            "a0",
            t_old,
            "нулевой чанк с длинным содержанием для индексации",
            "/d",
        ),
        block(
            "a1",
            T0,
            "первый чанк с длинным содержанием для индексации",
            "/d",
        ),
        block(
            "a2",
            T1,
            "второй чанк с длинным содержанием для индексации",
            "/d",
        ),
        block(
            "a3",
            T1,
            "третий чанк с длинным содержанием для индексации",
            "/d",
        ),
    ];
    let (server, _, _) = kernel_server(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let (embed, inputs) = embed_server(vec![1.0, 0.0, 0.0]).await;
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = SemanticConfig {
        embed_url: embed.uri(),
        embed_model: "test-model".into(),
        db_path: dir.path().join("semantic.db"),
    };

    // «Сбой» в секунду T0: записаны a0 и a1, watermark = T0.
    {
        let mut store = seeded_store(dir.path());
        store.set_meta("model", "test-model").unwrap();
        let partial = ChunkRecord {
            meta: ChunkMeta {
                block_id: "a0".into(),
                root_id: "r".into(),
                box_id: "b".into(),
                hpath: "/d".into(),
                updated: T0.into(),
            },
            content: "первый чанк с длинным содержанием для индексации".into(),
            vec: normalize(vec![1.0, 0.0, 0.0]),
        };
        let tx = store.begin().unwrap();
        Store::upsert_batch(&tx, std::slice::from_ref(&partial), Some(T0)).unwrap();
        tx.commit().unwrap();
    }

    crate::semantic::rebuild(&sy, &cfg, None, true)
        .await
        .expect("resume");

    let embedded = inputs.lock().unwrap();
    // a0 вне overlap-окна — не переэмбедден; хвост секунды сбоя (a1,
    // at-least-once) и незаписанные a2/a3 доброшены: 3 инпута, не 4.
    assert_eq!(embedded.len(), 3, "инпуты: {embedded:?}");
    assert!(!embedded.iter().any(|c| c.contains("нулевой")));
}

#[tokio::test]
async fn rebuild_resume_refuses_empty_index() {
    let (server, _) = kernel_server_blocks_only(vec![]).await;
    let sy = SiYuan::for_tests(server.uri());
    let dir = tempfile::tempdir().expect("tmpdir");
    let cfg = SemanticConfig {
        embed_url: server.uri(),
        embed_model: "test-model".into(),
        db_path: dir.path().join("semantic.db"),
    };
    let err = match crate::semantic::rebuild(&sy, &cfg, None, true).await {
        Ok(_) => panic!("пустой индекс — ожидание отказа resume"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("resume"));
}

#[test]
fn advance_watermark_is_monotonic() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = seeded_store(dir.path());

    store.advance_watermark(T0).unwrap();
    store.advance_watermark(T1).unwrap();
    assert_eq!(store.meta("watermark").unwrap().as_deref(), Some(T1));

    // «Откат» из overlap-окна resume не двигает знак назад.
    store.advance_watermark(T0).unwrap();
    assert_eq!(
        store.meta("watermark").unwrap().as_deref(),
        Some(T1),
        "watermark должен расти только вперёд"
    );
}

#[tokio::test]
async fn backlog_size_counts_only_rows_above_watermark() {
    // Регрессия: литерал watermark без кавычек делал строковое updated
    // числом и count отдавал «все строки ядра».
    let blocks = vec![
        block(
            "a1",
            "20261103230000",
            "первый чанк с длинным содержанием для индексации",
            "/d",
        ),
        block(
            "a2",
            "20261104000000",
            "второй чанк с длинным содержанием для индексации",
            "/d",
        ),
    ];
    let (server, _) = kernel_server_blocks_only(blocks).await;
    let sy = SiYuan::for_tests(server.uri());
    let dir = tempfile::tempdir().expect("tmpdir");
    let store = seeded_store(dir.path());
    store.set_meta("watermark", "20261103235900").unwrap();

    let backlog = crate::semantic::status_backlog_for_tests(&sy, &store).await;

    assert_eq!(backlog, 1, "над watermark только вторая строка");
}
