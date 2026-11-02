//! Общая инфраструктура интеграционных тестов: in-process mock-шлюз
//! (std::TcpListener, без новых зависимостей) и изоляция HOME.
//! Каждый запрос — включая initialize и notifications — снимает следующий
//! ответ из сценария; тело-шаблон подставляет {ID} из id запроса
//! (parse_response строго матчит id).
#![allow(dead_code)]

use std::io::{Read, Write};

use uwuwu_cli::affine_client::AffineConfig;

pub const INIT: &str = r#"{"jsonrpc":"2.0","id":{ID},"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"1"}}}"#;
pub const NOTIFIED: &str = "";
pub const CALL_OK: &str =
    r#"{"jsonrpc":"2.0","id":{ID},"result":{"structuredContent":{"ok":true,"answer":42}}}"#;

/// Один шаг сценария: HTTP-статус, заголовок mcp-session-id (у initialize)
/// и шаблон тела ({ID} → id запроса).
pub struct Step(pub u16, pub Option<&'static str>, pub String);

pub struct MockGateway {
    addr: std::net::SocketAddr,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl MockGateway {
    pub fn start(steps: Vec<Step>) -> Self {
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

    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    /// Хоть один запрос вызова указанного тулла.
    pub fn called(&self, tool: &str) -> bool {
        self.requests()
            .iter()
            .any(|r| r.contains(&format!("\"{tool}\"")))
    }
}

fn serve(
    mut stream: std::net::TcpStream,
    Step(status, sid, tpl): Step,
    log: &std::sync::Mutex<Vec<String>>,
) {
    let tpl: &str = &tpl;
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

pub fn test_cfg(mock: &MockGateway) -> AffineConfig {
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

pub fn with_temp_home(f: impl FnOnce()) {
    let guard = TempHome {
        real: std::env::var("HOME").unwrap_or_default(),
        _dir: tempfile::TempDir::new().unwrap(),
    };
    std::env::set_var("HOME", guard._dir.path());
    f();
    drop(guard);
}

pub fn session_file() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".uwuwu/mcp-session.txt"))
        .unwrap()
}
