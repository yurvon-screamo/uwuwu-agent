//! Общий харнес e2e-стендов uwuwu-server: живой сервер + mock_opencode
//! на свободных портах, ожидание health, MCP-вызовы и хвост лога.
//! Вынесен из quota_limit_pause_stand_e2e.rs — делится с cron_stand_e2e.rs.
//! Каждый integration-таргет компилирует модуль целиком — не таргетные
//! хелперы мертвы локально, не по факту (lint-уровень унаследован от common).

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const BEARER: &str = "e2e-stand-bearer";

/// То, чем один стенд отличается от другого: секции конфига и env сервера.
pub struct StandOptions<'a> {
    /// Секции после `[executor]`; пустая строка — дополнительных секций нет.
    /// Секции разделяются пустой строкой, как в config.toml сервера.
    pub config_extra: &'a str,
    /// Env сервера сверх базового (сид памяти, документы memory-hub и т.п.).
    pub server_env: &'a [(&'a str, &'a str)],
    /// Содержимое state.json, записанное до старта сервера.
    pub pre_state: Option<&'a str>,
    /// Тестовый прокси перед моком: писать тела запросов в `<dir>/capture.jsonl`.
    /// Нужен, чтобы увидеть модель, которую сервер кладёт в POST /api/session
    /// (сам mock_opencode модель в лог не пишет).
    pub capture_sessions: bool,
}

pub struct Stand {
    pub dir: PathBuf,
    pub mock: Child,
    pub server: Child,
    pub port: u16,
    pub mock_port: u16,
}

impl Drop for Stand {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.mock.kill();
        let _ = self.server.wait();
        let _ = self.mock.wait();
    }
}

pub fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").ok().as_deref() == Some("1")
}

/// Свободный TCP-порт (бинд :0, слушатель сразу отпускается — стендовые
/// гонки портов закрыты повторными попытками health/старта).
fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind :0")
        .local_addr()
        .expect("local_addr")
        .port()
}

fn bin_exe(name: &str) -> PathBuf {
    std::env::var(format!("CARGO_BIN_EXE_{name}"))
        .unwrap_or_else(|_| {
            panic!("CARGO_BIN_EXE_{name} не задан — собери бинари (cargo build --bins)")
        })
        .into()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Один запрос, перехваченный тестовым прокси.
struct CapturedRequest {
    method: String,
    path: String,
    body: Vec<u8>,
    auth: Option<String>,
    ctype: Option<String>,
}

/// Прочитать HTTP-запрос целиком: шапка, тело по Content-Length.
async fn read_captured(client: &mut tokio::net::TcpStream) -> Option<CapturedRequest> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = find_subslice(&data, b"\r\n\r\n") {
            break pos + 4;
        }
        let n = client.read(&mut buf).await.ok()?;
        if n == 0 || data.len() > 1024 * 1024 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
    let mut meta = CapturedRequest {
        method: String::new(),
        path: String::new(),
        body: Vec::new(),
        auth: None,
        ctype: None,
    };
    let mut content_len = 0usize;
    for (index, line) in head.lines().enumerate() {
        if index == 0 {
            let mut parts = line.split_whitespace();
            meta.method = parts.next()?.to_string();
            meta.path = parts.next()?.to_string();
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if key.eq_ignore_ascii_case("content-length") {
            content_len = value.parse().unwrap_or(0);
        } else if key.eq_ignore_ascii_case("authorization") {
            meta.auth = Some(value.to_string());
        } else if key.eq_ignore_ascii_case("content-type") {
            meta.ctype = Some(value.to_string());
        }
    }
    while data.len() < head_end + content_len {
        let n = client.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    meta.body = data[head_end..].to_vec();
    Some(meta)
}

/// Записать запрос в JSONL и передать моку, ответ вернуть клиенту.
async fn pass_through(
    client: &mut tokio::net::TcpStream,
    req: CapturedRequest,
    mock_port: u16,
    capture: &Path,
    http: &reqwest::Client,
) {
    let rec = json!({
        "method": req.method,
        "path": req.path,
        "body": serde_json::from_slice::<Value>(&req.body).unwrap_or(Value::Null),
    });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(capture)
    {
        let _ = writeln!(file, "{rec}");
    }
    let Ok(method) = req.method.parse::<reqwest::Method>() else {
        return;
    };
    let mut forward = http.request(method, format!("http://127.0.0.1:{mock_port}{}", req.path));
    if let Some(value) = req.auth {
        forward = forward.header("authorization", value);
    }
    if let Some(value) = req.ctype {
        forward = forward.header("content-type", value);
    }
    if !req.body.is_empty() {
        forward = forward.body(req.body);
    }
    let Ok(resp) = forward.send().await else {
        return;
    };
    let status = resp.status();
    let bytes = resp.bytes().await.unwrap_or_default();
    let mut out = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or(""),
        bytes.len()
    )
    .into_bytes();
    out.extend_from_slice(&bytes);
    let _ = client.write_all(&out).await;
}

/// Прозрачный HTTP-прокси перед моком (только тесты): ловит method/path/body
/// и пишет их в JSONL, после чего передаёт запрос моку как есть. Позволяет
/// увидеть модель из карты сервера в живом POST /api/session.
async fn capture_proxy(mock_port: u16, capture: PathBuf) -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("capture proxy bind");
    let port = listener.local_addr().expect("local_addr").port();
    let http = reqwest::Client::new();
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else {
                continue;
            };
            let Some(req) = read_captured(&mut client).await else {
                continue;
            };
            pass_through(&mut client, req, mock_port, &capture, &http).await;
        }
    });
    port
}

pub async fn spawn_stand(dir: &Path, opts: StandOptions<'_>) -> Stand {
    let server_port = free_port();
    let mock_port = free_port();
    let executor_port = if opts.capture_sessions {
        capture_proxy(mock_port, dir.join("capture.jsonl")).await
    } else {
        mock_port
    };
    let config = format!(
        "mode = \"live\"\npoll_sec = 10\nport = {server_port}\n\n\
         [hub]\nrepo = \"e2e/hub\"\ndriver = \"memory\"\n\n\
         [executor]\nname = \"mock\"\nurl = \"http://127.0.0.1:{executor_port}\"\n\n\
         {config_extra}\
         [policy]\ncup = 2\n\n[policy.peak]\nenabled = false\nstart_min = 360\nend_min = 600\n\n\
         [[project]]\nname = \"stand\"\nboard = \"stand\"\nrepo = \"e2e/stand\"\n\
         checkout = \"{dir}/checkout\"\nenv_root = \"{dir}\"\n",
        config_extra = opts.config_extra,
        dir = dir.display()
    );
    std::fs::create_dir_all(dir.join("checkout")).expect("checkout");
    std::fs::write(dir.join("config.toml"), config).expect("config");
    if let Some(raw) = opts.pre_state {
        std::fs::write(dir.join("state.json"), raw).expect("pre-state");
    }

    let mock_log = std::fs::File::create(dir.join("mock.log")).expect("mock log");
    let mock = Command::new(bin_exe("mock_opencode"))
        .env("MOCK_MODE", "plain")
        .env("MOCK_PORT", mock_port.to_string())
        .env("MOCK_STEP_MS", "500")
        .stdout(Stdio::from(mock_log.try_clone().expect("mock log clone")))
        .stderr(Stdio::from(mock_log))
        .spawn()
        .expect("spawn mock_opencode");

    let server_log = std::fs::File::create(dir.join("server.log")).expect("server log");
    let mut server_cmd = Command::new(bin_exe("uwuwu-server"));
    server_cmd
        .env("UWUWU_SERVER_CONFIG", dir.join("config.toml"))
        .env("UWUWU_SERVER_STATE", dir.join("state.json"))
        .env("UWUWU_SERVER_BEARER", BEARER)
        .env("UWUWU_OPENCODE_PASSWORD", "e2e")
        .env("GH_TOKEN", "e2e-dummy-token")
        .env("RUST_LOG", "info");
    for (name, value) in opts.server_env {
        server_cmd.env(name, value);
    }
    let server = server_cmd
        .stdout(Stdio::from(
            server_log.try_clone().expect("server log clone"),
        ))
        .stderr(Stdio::from(server_log))
        .spawn()
        .expect("spawn uwuwu-server");

    let stand = Stand {
        dir: dir.to_path_buf(),
        mock,
        server,
        port: server_port,
        mock_port,
    };
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    assert!(
        health(&http, stand.port, deadline).await,
        "сервер стенда не поднялся\n{}",
        log_tail(&stand.dir.join("server.log"))
    );
    stand
}

pub fn log_tail(path: &Path) -> String {
    let binding = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = binding.lines().collect();
    let start = lines.len().saturating_sub(30);
    lines[start..].join("\n")
}

async fn health(http: &reqwest::Client, port: u16, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if let Ok(resp) = http
            .get(format!("http://127.0.0.1:{port}/health"))
            .header("Authorization", format!("Bearer {BEARER}"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            if resp.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

pub async fn mcp_call(
    http: &reqwest::Client,
    port: u16,
    bearer: &str,
    tool: &str,
    args: Value,
) -> String {
    let resp = http
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("Authorization", format!("Bearer {bearer}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        }))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("MCP POST");
    let body: Value = resp.json().await.expect("MCP JSON");
    body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}
