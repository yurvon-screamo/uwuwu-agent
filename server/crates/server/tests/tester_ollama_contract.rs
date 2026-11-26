//! Тесты тестировщика #1687: контракт живого запроса к Ollama, честная
//! деградация при мёртвом маршруте и снятие квотных реликтов Gemini.
//! Ни одного изменения прода — только интеграционные пины поверх зеркала
//! `src/search` (крейт бинарный, lib-таргета нет).

mod search;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use search::semantic::{Embedder, OllamaEmbeddings, EMBED_MODEL, EMBED_MODEL_DIMS};
use search::semantic_backend::LanceSemanticBackend;
use search::support::{lance_dir, FakeWiki, FixedEmbedder, TempDirGuard};
use serde_json::Value;
use uwuwu_usecase::{SearchUseCase, SemanticBackend, WikiRepo};

/// One-shot mock `/api/embed`: считает запросы и отвечает векторами нужной
/// размерности по числу присланных текстов. Таймауты сокета не дают тесту
/// зависнуть, если клиент прислал меньше вызовов, чем ожидалось.
struct MockEmbed {
    url: String,
    bodies: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

/// Читает один HTTP-запрос до конца заголовков и тела по Content-Length.
fn read_request(sock: &mut std::net::TcpStream) -> Option<String> {
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
    Some(String::from_utf8_lossy(&buf[header_end..header_end + len]).into_owned())
}

/// Принимает соединение, не дожидаясь его вечно: mock живёт в потоке теста.
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
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&bodies);
        let handle = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            for _ in 0..expected {
                let mut sock = match accept_with_deadline(&listener, deadline) {
                    Some(sock) => sock,
                    None => return,
                };
                let _ = sock.set_read_timeout(Some(Duration::from_secs(5)));
                let body = match read_request(&mut sock) {
                    Some(body) => body,
                    None => return,
                };
                sink.lock().expect("lock").push(body.clone());
                let inputs = serde_json::from_str::<Value>(&body).expect("json body")["input"]
                    .as_array()
                    .expect("input — массив")
                    .len();
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
            bodies,
            handle: Some(handle),
        }
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().expect("lock").clone()
    }
}

impl Drop for MockEmbed {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Пин контракта #1687: POST /api/embed с явной моделью `:740m`, query шлёт
/// ровно один текст с task-префиксом, документы — title/text-префиксы.
#[tokio::test]
async fn request_contract_pins_model_and_prefixes() {
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

    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 2, "query и батч — два вызова /api/embed");
    let query: Value = serde_json::from_str(&bodies[0]).expect("query json");
    assert_eq!(query["model"], Value::String(EMBED_MODEL.to_string()));
    assert_eq!(
        query["input"],
        serde_json::json!(["task: search result | query: лимиты эмбеддингов"]),
        "query — ровно один текст с task-префиксом"
    );
    let batch: Value = serde_json::from_str(&bodies[1]).expect("batch json");
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

/// NFT-1: URL задан, но Ollama недоступна — секция semantic честно пишет
/// причину, FTS продолжает выдавать попадания по уже накопленному индексу.
#[tokio::test]
async fn dead_ollama_renders_unavailable_while_fulltext_lives() {
    const SLUG: &str = "Experience/platform/Пин деградации Ollama";
    const DOC: &str = "# Пин\n\nRailway деплой идёт мержем в master.";
    let guard = TempDirGuard(lance_dir("tester-dead-ollama"));
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
        semantic.slug.contains("unavailable") && semantic.slug.contains("ollama-embeddings"),
        "честная причина отказа, факт: {semantic:?}"
    );
}

/// Снятие квотных реликтов (#1687): старый `semantic_budget.json` с исчерпанным
/// суточным капом больше не останавливает синок; емкостный гард 19 работает,
/// повторный тик идемпотентен, файл бюджета не переписывается.
#[tokio::test]
async fn stale_gemini_budget_does_not_stop_sync() {
    const BUDGET: &str = r#"{"date":"2026-11-27","spent":100000}"#;
    let guard = TempDirGuard(lance_dir("tester-no-budget"));
    let dir = guard.0.clone();
    std::fs::create_dir_all(&dir).expect("volume dir");
    let budget_file = dir.join("semantic_budget.json");
    std::fs::write(&budget_file, BUDGET).expect("stale budget");

    let docs: HashMap<String, (String, String)> = (0..21)
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

    assert_eq!(backend.sync_index(19).await.expect("тик 1"), 19);
    assert_eq!(backend.sync_index(19).await.expect("тик 2"), 2);
    assert_eq!(backend.sync_index(19).await.expect("тик 3"), 0);
    assert_eq!(
        std::fs::read_to_string(&budget_file).expect("budget file still readable"),
        BUDGET,
        "квотный файл не читается и не переписывается"
    );
}
