//! Интеграционные тесты MCP-транспорта против in-process mock-шлюза
//! (std::TcpListener, без новых зависимостей). Каждый запрос — включая
//! initialize и notifications — снимает следующий ответ из сценария;
//! тело-шаблон подставляет {ID} из id запроса (parse_response строго
//! матчит id). HOME подменяется на временный каталог: connect() не должен
//! видеть реальный ~/.uwuwu/mcp-session.txt и не должен его трогать.
//! Ретраи: UWUWU_RETRY_BASE_MS=0 — паузы бэкофа не съедают время сьюта;
//! таймаутные сценарии через UWUWU_CALL_TIMEOUT_SECS=1 и hanging-шаг.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serde_json::json;
use uwuwu_cli::affine_client::{AffineClient, AffineConfig};

const INIT: &str = r#"{"jsonrpc":"2.0","id":{ID},"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"1"}}}"#;
const NOTIFIED: &str = "";
const CALL_OK: &str =
    r#"{"jsonrpc":"2.0","id":{ID},"result":{"structuredContent":{"ok":true,"answer":42}}}"#;

/// Один шаг сценария: HTTP-статус, заголовок mcp-session-id (у initialize),
/// доп. заголовки (Retry-After) и шаблон тела ({ID} → id запроса).
/// hang_secs > 0 — принять запрос, залогировать и МОЛЧАТЬ: клиент упирается
/// в таймаут (UWUWU_CALL_TIMEOUT_SECS), это шаг «шлюз завис».
struct Step {
    status: u16,
    sid: Option<&'static str>,
    tpl: &'static str,
    headers: &'static [(&'static str, &'static str)],
    hang_secs: u64,
}

impl Step {
    fn basic(status: u16, sid: Option<&'static str>, tpl: &'static str) -> Self {
        Self {
            status,
            sid,
            tpl,
            headers: &[],
            hang_secs: 0,
        }
    }

    fn with_headers(
        status: u16,
        sid: Option<&'static str>,
        tpl: &'static str,
        headers: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            status,
            sid,
            tpl,
            headers,
            hang_secs: 0,
        }
    }

    fn hanging() -> Self {
        Self {
            status: 0,
            sid: None,
            tpl: "",
            headers: &[],
            hang_secs: 30,
        }
    }
}

struct MockGateway {
    addr: std::net::SocketAddr,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl MockGateway {
    fn start(steps: Vec<Step>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = requests.clone();
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            for step in steps {
                let stream = loop {
                    if std::time::Instant::now() > deadline {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => return,
                    }
                };
                serve(stream, step, &log);
            }
        });
        MockGateway { addr, requests }
    }

    fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn saw_header(&self, needle: &str) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|req| req.to_lowercase().contains(&needle.to_lowercase()))
    }
}

fn serve(mut stream: std::net::TcpStream, step: Step, log: &std::sync::Mutex<Vec<String>>) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                let head = String::from_utf8_lossy(&buf);
                if let Some(headers_end) = head.find("\r\n\r\n") {
                    let content_length = head[..headers_end]
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if buf.len() >= headers_end + 4 + content_length {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    let req = String::from_utf8_lossy(&buf).to_string();
    log.lock().unwrap().push(req.clone());
    if step.hang_secs > 0 {
        // «шлюз завис»: держим соединение, не отвечаем — клиент упрётся в таймаут
        std::thread::sleep(std::time::Duration::from_secs(step.hang_secs));
        return;
    }
    // echo id запроса в шаблон ответа (у уведомлений id нет — {ID} не встречается).
    // Инвариант порядка: serde_json без preserve_order сериализует ключи
    // объекта алфавитно (BTreeMap), а "id" < "jsonrpc" < "method" — top-level
    // id встречается в теле первым. Смена сериализации потребует честного
    // JSON-парсинга вместо split.
    let id = req
        .split("\"id\":")
        .nth(1)
        .and_then(|rest| {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u64>().ok()
        })
        .unwrap_or_default();
    let body = step.tpl.replace("{ID}", &id.to_string());
    let reason = match step.status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "Status",
    };
    let mut resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        step.status,
        reason,
        body.len()
    );
    for (k, v) in step.headers {
        resp.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(sid) = step.sid {
        resp.push_str(&format!("mcp-session-id: {}\r\n", sid));
    }
    resp.push_str("\r\n");
    resp.push_str(&body);
    stream.write_all(resp.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn test_cfg(mock: &MockGateway) -> AffineConfig {
    AffineConfig {
        url: mock.url(),
        token: "test-token".into(),
        workspace: "ws-test".into(),
    }
}

/// HOME → временный каталог на время замыкания: изоляция общего
/// session-файла и lock-файла от реальной машины (все тесты файла — serial).
/// Бэкоф зажат в ноль (UWUWU_RETRY_BASE_MS=0) — ретраи мгновенны.
/// Drop-guard: реальный env восстанавливается и при панике тела теста.
struct TempHome {
    real: String,
    real_base: Option<String>,
    _dir: tempfile::TempDir,
}

impl Drop for TempHome {
    fn drop(&mut self) {
        std::env::set_var("HOME", &self.real);
        match &self.real_base {
            Some(v) => std::env::set_var("UWUWU_RETRY_BASE_MS", v),
            None => std::env::remove_var("UWUWU_RETRY_BASE_MS"),
        }
    }
}

fn with_temp_home(f: impl FnOnce()) {
    let guard = TempHome {
        real: std::env::var("HOME").unwrap_or_default(),
        real_base: std::env::var("UWUWU_RETRY_BASE_MS").ok(),
        _dir: tempfile::TempDir::new().unwrap(),
    };
    std::env::set_var("HOME", guard._dir.path());
    std::env::set_var("UWUWU_RETRY_BASE_MS", "0");
    f();
    drop(guard);
}

fn session_file() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".uwuwu/mcp-session.txt"))
        .unwrap()
}

#[test]
#[serial_test::serial]
fn handshake_saves_session_file_and_reuse_skips_initialize() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        {
            let _client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
            assert_eq!(
                std::fs::read_to_string(session_file()).unwrap().trim(),
                "sess-A"
            );
        }
        // «процесс завершился» (лок освобождён): повторный connect на живой
        // сессии не делает НИ ОДНОГО запроса. Пока первый клиент жив, лок
        // удерживается — конкурент видит private-сессию (см. тест ниже).
        let _again = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(mock.request_count(), 2);
    });
}

#[test]
#[serial_test::serial]
fn call_tool_returns_structured_content() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("search_docs", json!({"q": "x"})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 3);
    });
}

#[test]
#[serial_test::serial]
fn gateway_ok_false_convention_fails_fast_without_retry() {
    with_temp_home(|| {
        let rejected = r#"{"jsonrpc":"2.0","id":{ID},"result":{"ok":false,"error":"инцидент"}}"#;
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, rejected),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("add_database_row", json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("инцидент"), "got: {err}");
        // логический фейл — Fatal: ни одного повторного запроса
        assert_eq!(mock.request_count(), 3);
    });
}

#[test]
#[serial_test::serial]
fn transport_5xx_is_retried_and_then_succeeds() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(502, None, "gateway hiccup"),
            Step::basic(502, None, "gateway hiccup again"),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("list_tags", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 5);
    });
}

#[test]
#[serial_test::serial]
fn retry_after_header_replaces_backoff_pause() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            // 429 с Retry-After: 1 — пауза из заголовка, не из бэкофа (base=0)
            Step::with_headers(429, None, "slow down", &[("Retry-After", "1")]),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let started = Instant::now();
        let out = rt
            .block_on(client.call_tool("list_tags", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        // пауза пришла из Retry-After (≥0.9s), а не из base=0
        assert!(
            started.elapsed() >= Duration::from_millis(900),
            "elapsed {:?} — Retry-After не уважен",
            started.elapsed()
        );
        assert_eq!(mock.request_count(), 4);
    });
}

#[test]
#[serial_test::serial]
fn stale_session_400_triggers_rehandshake_and_retry() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(400, None, "session rejected"),
            Step::basic(200, Some("sess-B"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("read_rows", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        // пере-handshake перезаписал общий файл новой сессией
        assert_eq!(
            std::fs::read_to_string(session_file()).unwrap().trim(),
            "sess-B"
        );
        assert_eq!(mock.request_count(), 6);
    });
}

#[test]
#[serial_test::serial]
fn retryable_ok_false_is_retried_logical_ok_false_is_not() {
    with_temp_home(|| {
        let retryable = r#"{"jsonrpc":"2.0","id":{ID},"result":{"ok":false,"error":"перегрузка","retryable":true}}"#;
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, retryable),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("list_tags", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 4);
    });
}

#[test]
#[serial_test::serial]
fn retryable_inside_text_blob_is_retried() {
    with_temp_home(|| {
        let blob = r#"{"jsonrpc":"2.0","id":{ID},"result":{"content":[{"type":"text","text":"{\"ok\": false, \"error\": \"гонка записи\", \"retryable\": true}"}]}}"#;
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, blob),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("read_doc", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 4);
    });
}

#[test]
#[serial_test::serial]
fn write_retryable_queue_timeout_is_retried() {
    with_temp_home(|| {
        // WRITE_QUEUE_TIMEOUT — запрос ждал в очереди и не диспеттился:
        // шлюз гарантирует неприменение, запись ретраится (ADR-006)
        let queued = r#"{"jsonrpc":"2.0","id":{ID},"result":{"ok":false,"error":"Write for scope waited too long in the queue.","code":"WRITE_QUEUE_TIMEOUT","retryable":true}}"#;
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, queued),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("create_doc", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 4);
    });
}

#[test]
#[serial_test::serial]
fn write_retryable_untrusted_code_fails_fast_without_retry() {
    with_temp_home(|| {
        // DOCUMENT_MOVE_PARTIAL — операция частично применена, флаг шлюза
        // нарушает гарантию: клиент НЕ доверяет его записи — Fatal без ретрая
        let partial = r#"{"jsonrpc":"2.0","id":{ID},"result":{"ok":false,"error":"The document move did not reach a complete state.","code":"DOCUMENT_MOVE_PARTIAL","retryable":true}}"#;
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, partial),
            // этот шаг не должен быть запрошен — ретрая нет
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("move_doc", json!({})))
            .unwrap_err();
        assert!(err.to_string().contains("не доверяется"), "{err}");
        // INIT + notified + единственный вызов — ретрая не было
        assert_eq!(mock.request_count(), 3);
    });
}

#[test]
#[serial_test::serial]
fn read_retryable_untrusted_code_is_still_retried() {
    with_temp_home(|| {
        // чтение не применяет ничего — флаг доверяется при любом коде
        let flaky = r#"{"jsonrpc":"2.0","id":{ID},"result":{"ok":false,"error":"upstream boom","code":"upstream_unavailable","retryable":true}}"#;
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, flaky),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("list_docs", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 4);
    });
}

#[test]
#[serial_test::serial]
fn empty_body_on_write_is_fatal_without_retry() {
    // живой инцидент сессии: шлюз ответил 200 с пустым телом. Запись:
    // ответ потерян, операция могла примениться — без ретраев.
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, ""),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("create_doc", json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unparsable gateway response"), "got: {err}");
        assert!(err.contains("могла примениться"), "got: {err}");
        assert_eq!(mock.request_count(), 3);
    });
}

#[test]
#[serial_test::serial]
fn empty_body_on_read_is_retried() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, ""),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let out = rt
            .block_on(client.call_tool("list_docs", json!({})))
            .unwrap();
        assert_eq!(out["answer"], 42);
        assert_eq!(mock.request_count(), 4);
    });
}

#[test]
#[serial_test::serial]
fn rpc_error_id_mismatch_is_rejected_by_sse_parser() {
    with_temp_home(|| {
        // тело с ЧУЖИМ id — parse_response должен отвергнуть; запись —
        // Fatal без ретраев (ответ мог уйти в чужой процесс кросс-током)
        let wrong_id = r#"{"jsonrpc":"2.0","id":999,"result":{"structuredContent":{}}}"#;
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, wrong_id),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("create_x", json!({})))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("id mismatch") || err.contains("unparsable"),
            "got: {err}"
        );
        assert_eq!(mock.request_count(), 3);
    });
}

#[test]
#[serial_test::serial]
fn timeout_read_retries_write_fails_fast() {
    with_temp_home(|| {
        std::env::set_var("UWUWU_CALL_TIMEOUT_SECS", "1");
        // чтение: hanging-шлюз → таймаут → ретраи до исчерпания (4 попытки)
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::hanging(),
            Step::hanging(),
            Step::hanging(),
            Step::hanging(),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("list_tags", json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("failed after 3 retries"), "got: {err}");
        assert_eq!(mock.request_count(), 3, "read: init+notify+1 висящий вызов");

        // запись: тот же зависший шлюз — одна попытка, честная ошибка
        let mock_w = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::hanging(),
        ]);
        let mut wclient = rt
            .block_on(AffineClient::connect(&test_cfg(&mock_w)))
            .unwrap();
        let err = rt
            .block_on(wclient.call_tool("create_doc", json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("могла примениться"), "got: {err}");
        assert_eq!(mock_w.request_count(), 3, "write: без повторов");
        std::env::remove_var("UWUWU_CALL_TIMEOUT_SECS");
    });
}

#[test]
#[serial_test::serial]
fn concurrent_connect_goes_private_and_shared_file_stays_intact() {
    with_temp_home(|| {
        // A: лок свободен → shared, публикует sess-A в файл
        let mock = MockGateway::start(vec![
            Step::basic(200, Some("sess-A"), INIT),
            Step::basic(202, None, NOTIFIED),
            // B: лок занят → private handshake sess-P + один tools/call
            Step::basic(200, Some("sess-P"), INIT),
            Step::basic(202, None, NOTIFIED),
            Step::basic(200, None, CALL_OK),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let a = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(
            std::fs::read_to_string(session_file()).unwrap().trim(),
            "sess-A"
        );

        // B: лок удерживается A → private-сессия, файл не тронут
        let mut b = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(
            std::fs::read_to_string(session_file()).unwrap().trim(),
            "sess-A",
            "private-процесс не должен перезаписывать общий файл"
        );
        let out = rt.block_on(b.call_tool("list_tags", json!({}))).unwrap();
        assert_eq!(out["answer"], 42);
        // B ходит со СВОЕЙ сессией, не с общей
        assert!(mock.saw_header("mcp-session-id: sess-P"));
        assert_eq!(mock.request_count(), 5);
        drop(b);

        // A вышел → лок свободен → следующий старт переиспользует sess-A
        drop(a);
        let _c = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(
            std::fs::read_to_string(session_file()).unwrap().trim(),
            "sess-A"
        );
        assert_eq!(
            mock.request_count(),
            5,
            "C переиспользовал сессию без handshake"
        );
    });
}
