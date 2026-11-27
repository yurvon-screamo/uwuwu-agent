//! Общий харнес e2e-стендов uwuwu-server: живой сервер + mock_opencode
//! на свободных портах, ожидание health, MCP-вызовы и хвост лога.
//! Вынесен из quota_limit_pause_stand_e2e.rs — делится с cron_stand_e2e.rs.
//! Каждый integration-таргет компилирует модуль целиком — не таргетные
//! хелперы мертвы локально, не по факту (lint-уровень унаследован от common).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const BEARER: &str = "e2e-stand-bearer";

/// То, чем один стенд отличается от другого: секции конфига и env сервера.
pub struct StandOptions<'a> {
    /// Томл перед `[hub]`: верхнеуровневые ключи (например, `sweep_days`,
    /// `env_gc_days`) или секции (например, `[quota]`). Пустая строка —
    /// дополнительных ключей/секций нет. Верхнеуровневые ключи обязаны
    /// стоять до первой секции — после `[executor]`/`[hub]` они
    /// распарсились бы в чужую таблицу.
    pub config_extra: &'a str,
    /// Env сервера сверх базового (сид памяти, документы memory-hub и т.п.).
    pub server_env: &'a [(&'a str, &'a str)],
    /// Содержимое state.json, записанное до старта сервера.
    pub pre_state: Option<&'a str>,
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

pub async fn spawn_stand(dir: &Path, opts: StandOptions<'_>) -> Stand {
    let server_port = free_port();
    let mock_port = free_port();
    // config_extra — до первой секции: верхнеуровневые ключи (sweep_days,
    // env_gc_days) и секции ([quota]) валидны только вне чужих таблиц.
    let config = format!(
        "mode = \"live\"\npoll_sec = 10\nport = {server_port}\n\n\
         {config_extra}\
         [hub]\nrepo = \"e2e/hub\"\ndriver = \"memory\"\n\n\
         [executor]\nname = \"mock\"\nurl = \"http://127.0.0.1:{mock_port}\"\n\n\
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
