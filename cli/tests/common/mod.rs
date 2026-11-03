//! Общая инфраструктура интеграционных тестов: in-process mock-шлюз
//! (std::TcpListener, без новых зависимостей). Каждый запрос — включая
//! initialize и notifications — снимает следующий ответ из сценария;
//! тело-шаблон подставляет {ID} из id запроса (parse_response строго
//! матчит id). HOME подменяется на временный каталог: connect() не должен
//! видеть реальный ~/.uwuwu/mcp-session.txt и не должен его трогать.
//! Ретраи: UWUWU_RETRY_BASE_MS=0 — паузы бэкофа не съедают время сьюта;
//! таймаутные сценарии через UWUWU_CALL_TIMEOUT_SECS=1 и hanging-шаг.

#![allow(dead_code)]

use std::borrow::Cow;
use std::io::{Read, Write};

use uwuwu_cli::affine_client::AffineConfig;

pub const INIT: &str = r#"{"jsonrpc":"2.0","id":{ID},"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"1"}}}"#;
pub const NOTIFIED: &str = "";
pub const CALL_OK: &str =
    r#"{"jsonrpc":"2.0","id":{ID},"result":{"structuredContent":{"ok":true,"answer":42}}}"#;

/// Один шаг сценария: HTTP-статус, заголовок mcp-session-id (у initialize),
/// доп. заголовки (Retry-After) и шаблон тела ({ID} → id запроса).
/// hang_secs > 0 — принять запрос, залогировать и МОЛЧАТЬ: клиент упирается
/// в таймаут (UWUWU_CALL_TIMEOUT_SECS), это шаг «шлюз завис».
pub struct Step {
    status: u16,
    sid: Option<&'static str>,
    tpl: Cow<'static, str>,
    headers: &'static [(&'static str, &'static str)],
    hang_secs: u64,
}

impl Step {
    pub fn basic(status: u16, sid: Option<&'static str>, tpl: &'static str) -> Self {
        Self {
            status,
            sid,
            tpl: tpl.into(),
            headers: &[],
            hang_secs: 0,
        }
    }

    /// Владеющий шаблон: сценарные тела, собранные в рантайме (без leak).
    pub fn owned(status: u16, sid: Option<&'static str>, tpl: String) -> Self {
        Self {
            status,
            sid,
            tpl: tpl.into(),
            headers: &[],
            hang_secs: 0,
        }
    }

    pub fn with_headers(
        status: u16,
        sid: Option<&'static str>,
        tpl: &'static str,
        headers: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            status,
            sid,
            tpl: tpl.into(),
            headers,
            hang_secs: 0,
        }
    }

    pub fn hanging() -> Self {
        Self {
            status: 0,
            sid: None,
            tpl: "".into(),
            headers: &[],
            hang_secs: 30,
        }
    }
}

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

    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    pub fn saw_header(&self, needle: &str) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|req| req.to_lowercase().contains(&needle.to_lowercase()))
    }

    /// Хоть один запрос вызова указанного тулла (лог хранит сырые тела).
    pub fn called(&self, tool: &str) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|req| req.contains(&format!("\"{tool}\"")))
    }

    /// Снимок лога сырых запросов.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
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

pub fn test_cfg(mock: &MockGateway) -> AffineConfig {
    AffineConfig {
        url: mock.url(),
        token: "test-token".into(),
        workspace: "ws-test".into(),
    }
}

/// HOME → временный каталог + пин env на время замыкания: изоляция общего
/// session/lock-файлов и retry-бюджета от реальной машины и соседних тестов
/// (все тесты файла — serial; guard восстанавливает env и при панике тела).
/// Бэкоф зажат в ноль, попытки — дефольные 3 ретрая: ассерты на «failed
/// after 3 retries» не зависят от внешнего env.
struct TempHome {
    real_home: String,
    saved: Vec<(&'static str, Option<String>)>,
    _dir: tempfile::TempDir,
}

impl Drop for TempHome {
    fn drop(&mut self) {
        std::env::set_var("HOME", &self.real_home);
        for (key, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

pub fn with_temp_home(f: impl FnOnce()) {
    let guard = TempHome {
        real_home: std::env::var("HOME").unwrap_or_default(),
        saved: [
            "UWUWU_RETRY_BASE_MS",
            "UWUWU_RETRY_ATTEMPTS",
            "UWUWU_CALL_TIMEOUT_SECS",
        ]
        .map(|k| (k, std::env::var(k).ok()))
        .into(),
        _dir: tempfile::TempDir::new().unwrap(),
    };
    std::env::set_var("HOME", guard._dir.path());
    std::env::set_var("UWUWU_RETRY_BASE_MS", "0");
    std::env::set_var("UWUWU_RETRY_ATTEMPTS", "3");
    std::env::remove_var("UWUWU_CALL_TIMEOUT_SECS");
    f();
    drop(guard);
}

pub fn session_file() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".uwuwu/mcp-session.txt"))
        .unwrap()
}
