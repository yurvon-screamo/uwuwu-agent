//! Интеграционные пины #1687: HTTP-контракт клиента Ollama, честная
//! деградация при мёртвом маршруте, снятие квотных реликтов Gemini и
//! событие полноты индекса. Только тест-код: поверх зеркала `src/search`
//! (крейт бинарный, lib-таргета нет), прода не тронута.
//! Один файл на предмет: каждое `mod search;` компилирует и гоняет зеркало.

mod search;

use std::collections::HashMap;
use std::fmt::Debug;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use search::semantic::{ClientTimeouts, Embedder, OllamaEmbeddings, EMBED_MODEL, EMBED_MODEL_DIMS};
use search::semantic_backend::LanceSemanticBackend;
use search::support::{lance_dir, FakeWiki, FixedEmbedder, TempDirGuard};
use serde_json::Value;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use uwuwu_usecase::{SearchUseCase, SemanticBackend, WikiRepo};

// ——— mock /api/embed ———

struct HttpRequest {
    line: String,
    body: String,
}

/// One-shot mock `/api/embed`: помнит request line и тело каждого вызова,
/// отвечает векторами по числу присланных текстов. Deadline не даёт потоку
/// ждать вечно, если клиент прислал меньше вызовов, чем ожидалось.
struct MockEmbed {
    url: String,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
    handle: Option<JoinHandle<()>>,
}

/// Читает запрос целиком: request line + тело по Content-Length.
fn read_request(sock: &mut std::net::TcpStream) -> Option<HttpRequest> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = match sock.read(&mut tmp) {
            Ok(0) | Err(_) => return None,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let len: usize = head
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .expect("Content-Length в запросе");
    while buf.len() < header_end + len {
        let n = match sock.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
    }
    Some(HttpRequest {
        line: head.lines().next().unwrap_or_default().to_string(),
        body: String::from_utf8_lossy(&buf[header_end..header_end + len]).into_owned(),
    })
}

fn accept_with_deadline(listener: &TcpListener, deadline: Instant) -> Option<std::net::TcpStream> {
    loop {
        match listener.accept() {
            Ok((sock, _)) => return Some(sock),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

impl MockEmbed {
    fn spawn(expected: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            for _ in 0..expected {
                let mut sock = match accept_with_deadline(&listener, deadline) {
                    Some(sock) => sock,
                    None => return,
                };
                let _ = sock.set_read_timeout(Some(Duration::from_secs(5)));
                let req = match read_request(&mut sock) {
                    Some(req) => req,
                    None => return,
                };
                let inputs = serde_json::from_str::<Value>(&req.body).expect("json body")["input"]
                    .as_array()
                    .expect("input — массив")
                    .len();
                sink.lock().expect("lock").push(req);
                let answer = serde_json::json!({
                    "model": EMBED_MODEL,
                    "embeddings": vec![vec![0.0f32; EMBED_MODEL_DIMS]; inputs],
                })
                .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    answer.len(),
                    answer
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            }
        });
        Self {
            url,
            requests,
            handle: Some(handle),
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .expect("lock")
            .iter()
            .map(|r| HttpRequest {
                line: r.line.clone(),
                body: r.body.clone(),
            })
            .collect()
    }
}

impl Drop for MockEmbed {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn inputs(body: &str) -> Vec<String> {
    serde_json::from_str::<Value>(body).expect("json body")["input"]
        .as_array()
        .expect("input — массив")
        .iter()
        .map(|v| v.as_str().expect("строка").to_string())
        .collect()
}

/// Пин контракта #1687: `POST /api/embed` с явной моделью `:740m`, query шлёт
/// ровно один текст с task-префиксом, документы — title/text-префиксы.
#[tokio::test]
async fn request_contract_pins_method_path_model_and_prefixes() {
    let mock = MockEmbed::spawn(2);
    let client = OllamaEmbeddings::new(mock.url.clone(), EMBED_MODEL.to_string());

    let qv = client
        .embed_query("лимиты эмбеддингов")
        .await
        .expect("живой query-контракт");
    assert_eq!(qv.len(), EMBED_MODEL_DIMS, "query-вектор 768-dim");
    let docs = vec![
        ("Projects/x".to_string(), "текст один".to_string()),
        (String::new(), "без тайтла".to_string()),
    ];
    let dv = client.embed_docs(&docs).await.expect("батч docs");
    assert_eq!(dv.len(), 2, "по вектору на документ");

    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "query и батч — два вызова /api/embed");
    for req in &requests {
        assert_eq!(req.line, "POST /api/embed HTTP/1.1", "метод и путь");
    }
    let query: Value = serde_json::from_str(&requests[0].body).expect("query json");
    assert_eq!(query["model"], Value::String(EMBED_MODEL.to_string()));
    assert_eq!(
        query["input"],
        serde_json::json!(["task: search result | query: лимиты эмбеддингов"]),
        "query — ровно один текст с task-префиксом"
    );
    let batch: Value = serde_json::from_str(&requests[1].body).expect("batch json");
    assert_eq!(batch["model"], Value::String(EMBED_MODEL.to_string()));
    assert_eq!(
        batch["input"],
        serde_json::json!([
            "title: Projects/x | text: текст один",
            "title: none | text: без тайтла",
        ]),
        "doc-контракт с непустым/пустым тайтлом"
    );
}

/// Размер батча и production-таймауты — часть контракта из описания PR.
#[tokio::test]
async fn batch_size_and_timeouts_match_the_contract() {
    let timeouts = ClientTimeouts::default();
    assert_eq!(timeouts.connect, Duration::from_secs(2));
    assert_eq!(timeouts.query_total, Duration::from_secs(15));
    assert_eq!(timeouts.batch_total, Duration::from_secs(60));

    let mock = MockEmbed::spawn(2);
    let client = OllamaEmbeddings::new(mock.url.clone(), EMBED_MODEL.to_string());
    // Литералы, не константа из прода: иначе мутация размера батча остаётся
    // невидимой — тест считал бы 40 как EMBED_BATCH+2.
    let docs: Vec<_> = (0..40)
        .map(|i| (format!("T{i}"), format!("текст {i}")))
        .collect();
    let vecs = client.embed_docs(&docs).await.expect("два батча");
    assert_eq!(vecs.len(), 40);

    let requests = mock.requests();
    assert_eq!(requests.len(), 2, "40 текстов — два вызова");
    assert_eq!(
        inputs(&requests[0].body).len(),
        38,
        "первый батч: 38 текстов"
    );
    assert_eq!(inputs(&requests[1].body).len(), 2, "второй батч: 2 текста");
}

/// NFT-1: URL задан, но Ollama недоступна — секция semantic честно пишет
/// причину, FTS продолжает выдавать попадания по уже накопленному индексу.
#[tokio::test]
async fn dead_ollama_renders_unavailable_while_fulltext_lives() {
    const SLUG: &str = "Experience/platform/Пин деградации Ollama";
    const DOC: &str = "# Пин\n\nRailway деплой идёт мержем в master.";
    let guard = TempDirGuard(lance_dir("semantic-contract-dead"));
    let dir = guard.0.clone();
    let wiki: Arc<dyn WikiRepo> = Arc::new(FakeWiki {
        docs: HashMap::from([(SLUG.to_string(), (DOC.to_string(), "sha-test".to_string()))]),
    });

    // Индекс сеется рабочим эмбеддером; затем бекенд переоткрывается с
    // клиентом на закрытом порту — то состояние, что даёт прод при битом релее.
    {
        let seed =
            LanceSemanticBackend::open(dir.clone(), wiki.clone(), None, Arc::new(FixedEmbedder))
                .await
                .expect("seed open");
        seed.sync_index(10).await.expect("seed sync");
    }
    let dead = Arc::new(OllamaEmbeddings::new(
        "http://127.0.0.1:1".to_string(),
        EMBED_MODEL.to_string(),
    ));
    let backend = LanceSemanticBackend::open(dir, wiki, None, dead)
        .await
        .expect("reopen с мертвым клиентом");
    let uc = SearchUseCase {
        semantic: Some(Arc::new(backend)),
    };

    let hits = uc.search("railway деплой", 5).await.expect("search ok");

    assert!(
        hits.iter()
            .any(|h| h.section == "fulltext" && h.slug == SLUG),
        "FTS жив при отказе Ollama, факт: {hits:?}"
    );
    let semantic = hits
        .iter()
        .find(|h| h.section == "semantic")
        .expect("секция semantic присутствует");
    assert!(
        semantic.slug.contains("unavailable")
            && semantic.slug.contains("ollama-embeddings")
            && semantic.slug.contains("transport"),
        "честная причина транспортного отказа, факт: {semantic:?}"
    );
}

/// Дата суточного окна старого капа: полночь Pacific, фиксированный UTC-8.
fn legacy_budget_day() -> String {
    let offset = chrono::FixedOffset::west_opt(8 * 3600).expect("UTC-8");
    chrono::Utc::now()
        .with_timezone(&offset)
        .format("%Y-%m-%d")
        .to_string()
}

/// Квотный реликт (#1687): файл в схеме настоящего ридера с капом 700
/// (`day`/`spent`) больше не останавливает синк и не переписывает файл.
#[tokio::test]
async fn exhausted_legacy_budget_does_not_stop_sync() {
    let budget = format!(r#"{{"day":"{}","spent":700}}"#, legacy_budget_day());
    let guard = TempDirGuard(lance_dir("semantic-contract-budget"));
    let dir = guard.0.clone();
    std::fs::create_dir_all(&dir).expect("volume dir");
    let budget_file = dir.join("semantic_budget.json");
    std::fs::write(&budget_file, &budget).expect("exhausted budget");

    let docs: HashMap<String, (String, String)> = (0..2)
        .map(|i| {
            (
                format!("Experience/platform/Пин бюджета {i}"),
                (
                    format!("# Пин {i}\n\nТекст документа номер {i}."),
                    format!("sha-{i}"),
                ),
            )
        })
        .collect();
    let wiki = Arc::new(FakeWiki { docs });
    let backend = LanceSemanticBackend::open(dir, wiki, None, Arc::new(FixedEmbedder))
        .await
        .expect("open");

    assert_eq!(
        backend.sync_index(19).await.expect("синк проходит кап"),
        2,
        "старый суточный кап не блокирует индексацию"
    );
    assert_eq!(
        std::fs::read_to_string(&budget_file).expect("budget file readable"),
        budget,
        "квотный файл не переписывается"
    );
}

/// Емкостный гард прод-тика (в проде — `SEMANTIC_SYNC_BUDGET = 512`) — не квота:
/// тест держит механизм на дешёвом явном значении 19,
/// остаток доезжает следующим тиком, третий тик — no-op.
#[tokio::test]
async fn sync_capacity_guard_limits_tick_to_19_docs() {
    let guard = TempDirGuard(lance_dir("semantic-contract-capacity"));
    let dir = guard.0.clone();
    let docs: HashMap<String, (String, String)> = (0..21)
        .map(|i| {
            (
                format!("Experience/platform/Пин гарда {i}"),
                (
                    format!("# Пин {i}\n\nТекст документа номер {i}."),
                    format!("sha-{i}"),
                ),
            )
        })
        .collect();
    let wiki = Arc::new(FakeWiki { docs });
    let backend = LanceSemanticBackend::open(dir, wiki, None, Arc::new(FixedEmbedder))
        .await
        .expect("open");

    assert_eq!(backend.sync_index(19).await.expect("тик 1"), 19);
    assert_eq!(backend.sync_index(19).await.expect("тик 2"), 2);
    assert_eq!(backend.sync_index(19).await.expect("тик 3"), 0);
}

// ——— событие полноты индекса ———

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
        // Зеркало `mod search;` тянет соседние тесты того же бинарника —
        // их события не должны попадать в счёт этого теста.
        if !ctx
            .event_span(event)
            .is_some_and(|span| span.metadata().name() == "semantic-contract-index-state")
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

    let guard = TempDirGuard(lance_dir("semantic-contract-index"));
    let dir = guard.0.clone();
    // Три дока с разным числом длинных абзацев: каждый абзац >1200 байт
    // (кириллица в String::len — два байта на букву), чанковка режет по
    // каждому. docs=3, chunks=2+3+4=9 — пары различают таблицы doc_meta и
    // chunks, а не только существование строк.
    let paragraph = "бит ".repeat(200);
    let docs: HashMap<String, (String, String)> = (1..=3)
        .map(|i| {
            let md = format!(
                "# Пин {i}\n\n{}",
                std::iter::repeat_n(paragraph.clone(), i)
                    .collect::<Vec<_>>()
                    .join("\n\n")
            );
            (
                format!("Experience/platform/Пин полноты {i}"),
                (md, format!("sha-{i}")),
            )
        })
        .collect();
    let wiki = Arc::new(FakeWiki { docs });
    let backend = LanceSemanticBackend::open(dir, wiki, None, Arc::new(FixedEmbedder))
        .await
        .expect("open");

    let span = tracing::info_span!("semantic-contract-index-state");
    let _entered = span.enter();
    assert_eq!(backend.sync_index(10).await.expect("тик с изменениями"), 3);
    assert_eq!(backend.sync_index(10).await.expect("тик без изменений"), 0);

    let events = capture().0.lock().expect("lock")[before..].to_vec();
    assert_eq!(events.len(), 2, "по событию на тик, факт: {events:?}");
    assert_eq!(events[0], (3, 9), "docs=doc_meta, chunks=chunks");
    assert_eq!(events[1], events[0], "счётчики неизменны на changed=0");
}
