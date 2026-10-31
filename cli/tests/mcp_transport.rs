//! Интеграционные тесты MCP-транспорта против in-process mock-шлюза
//! (std::TcpListener, без новых зависимостей). Каждый запрос — включая
//! initialize и notifications — снимает следующий ответ из сценария;
//! тело-шаблон подставляет {ID} из id запроса (parse_response строго
//! матчит id). HOME подменяется на временный каталог: connect() не должен
//! видеть реальный ~/.uwuwu/mcp-session.txt и не должен его трогать.

use std::io::{Read, Write};

use serde_json::json;
use uwuwu_cli::affine_client::{AffineClient, AffineConfig};

const INIT: &str = r#"{"jsonrpc":"2.0","id":{ID},"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"1"}}}"#;
const NOTIFIED: &str = "";
const CALL_OK: &str =
    r#"{"jsonrpc":"2.0","id":{ID},"result":{"structuredContent":{"ok":true,"answer":42}}}"#;

/// Один шаг сценария: HTTP-статус, заголовок mcp-session-id (у initialize)
/// и шаблон тела ({ID} → id запроса).
struct Step(u16, Option<&'static str>, &'static str);

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
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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
}

fn serve(
    mut stream: std::net::TcpStream,
    Step(status, sid, tpl): Step,
    log: &std::sync::Mutex<Vec<String>>,
) {
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
    let body = tpl.replace("{ID}", &id.to_string());
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(sid) = sid {
        resp.push_str(&format!("mcp-session-id: {sid}\r\n"));
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
/// session-файла от реальной машины (все тесты файла — serial).
/// Drop-guard: реальный HOME восстанавливается и при панике тела теста.
struct TempHome {
    real: String,
    _dir: tempfile::TempDir,
}

impl Drop for TempHome {
    fn drop(&mut self) {
        std::env::set_var("HOME", &self.real);
    }
}

fn with_temp_home(f: impl FnOnce()) {
    let guard = TempHome {
        real: std::env::var("HOME").unwrap_or_default(),
        _dir: tempfile::TempDir::new().unwrap(),
    };
    std::env::set_var("HOME", guard._dir.path());
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
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(
            std::fs::read_to_string(session_file()).unwrap().trim(),
            "sess-A"
        );
        // повторный connect на живой сессии не делает НИ ОДНОГО запроса
        let _again = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        assert_eq!(mock.request_count(), 2);
    });
}

#[test]
#[serial_test::serial]
fn call_tool_returns_structured_content() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(200, None, CALL_OK),
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
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(200, None, rejected),
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
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(500, None, "gateway hiccup"),
            Step(200, None, CALL_OK),
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
fn stale_session_400_triggers_rehandshake_and_retry() {
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(400, None, "session rejected"),
            Step(200, Some("sess-B"), INIT),
            Step(202, None, NOTIFIED),
            Step(200, None, CALL_OK),
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
fn empty_body_is_reported_as_unparsable_not_panicked() {
    // живой инцидент сессии: шлюз ответил 200 с пустым телом
    with_temp_home(|| {
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(200, None, ""),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("anything", json!({})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unparsable gateway response"), "got: {err}");
    });
}

#[test]
#[serial_test::serial]
fn rpc_error_id_mismatch_is_rejected_by_sse_parser() {
    with_temp_home(|| {
        // тело с ЧУЖИМ id — parse_response должен отвергнуть
        let wrong_id = r#"{"jsonrpc":"2.0","id":999,"result":{"structuredContent":{}}}"#;
        let mock = MockGateway::start(vec![
            Step(200, Some("sess-A"), INIT),
            Step(202, None, NOTIFIED),
            Step(200, None, wrong_id),
        ]);
        let cfg = test_cfg(&mock);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(client.call_tool("x", json!({})))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("id mismatch") || err.contains("unparsable"),
            "got: {err}"
        );
    });
}
