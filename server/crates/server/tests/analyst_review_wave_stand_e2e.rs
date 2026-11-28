//! E2E главного сценария #1634 (гейт UWUWU_E2E=1): живая волна ревью
//! аналитики на memory-стенде (hub driver=memory, executor =
//! mock_opencode в agent-режиме) едет в мастер-сессию проекта, а не во
//! fresh-спавн — то, что тестировщик гонял руками на стенде scratch/stand.
//!
//! Сценарий (главный путь, привязка роль:проект):
//!   1. строка 901 в Review Analys → волна спавнит мастер-сессию,
//!      привязка (сессия/модель/checkout) пишется в state.json,
//!      мок доводит строку до Todo;
//!   2. строка 902 в Review Analys → волна reuse'ит ТУ ЖЕ сессию
//!      (новых спавнов нет), строка тоже доходит до Todo;
//!   3. idle-мастер с нулевыми порогами [compact] → нативный compact
//!      admitted, метка в state.json;
//!   4. удаление мастер-сессии → следующая волна пересоздаёт сессию
//!      и перезаписывает привязку (самовосстановление, без застревания).
//!
//! Запуск: UWUWU_E2E=1 cargo test -p uwuwu-server --test analyst_review_wave_stand_e2e
//! Длительность ~2 мин: волны едут по тикам сервера (poll_sec 10).

mod common;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").ok().as_deref() == Some("1")
}

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

const EV_SPAWNED: &str = "analys_review.spawned";
const EV_REUSED: &str = "analys_review.reused";
const EV_COMPACTED: &str = "analys_review.compacted";
const BEARER: &str = "e2e-1634-bearer";
const PROJECT: &str = "stand";
const BOARD: &str = "stand";
const MODEL: &str = "zai-coding-plan/glm-5.4#high";

struct Stand {
    dir: PathBuf,
    mock: Child,
    server: Child,
    port: u16,
    mock_port: u16,
}

impl Drop for Stand {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.mock.kill();
        let _ = self.server.wait();
        let _ = self.mock.wait();
    }
}

async fn spawn_stand(dir: &Path, seed: &str) -> Stand {
    let server_port = free_port();
    let mock_port = free_port();
    let config = format!(
        "mode = \"live\"\npoll_sec = 10\nport = {server_port}\n\n\
         {models}\
         {runtime}\
         [hub]\nrepo = \"e2e/hub\"\ndriver = \"memory\"\n\n\
         [executor]\nname = \"mock\"\nurl = \"http://127.0.0.1:{mock_port}\"\n\n\
         [policy]\ncup = 7\n\n[policy.peak]\nenabled = false\nstart_min = 0\nend_min = 0\n\n\
         [compact]\nidle_min = 0\nmin_context_pct = 0.0\ncooldown_min = 1\n\n\
         [[project]]\nname = \"{PROJECT}\"\nboard = \"{BOARD}\"\nrepo = \"e2e/stand\"\n\
         checkout = \"{dir}/checkout\"\nenv_root = \"{dir}\"\n",
        models = common::DEFAULT_MODELS,
        runtime = common::RUNTIME_CFG,
        dir = dir.display()
    );
    std::fs::write(dir.join("config.toml"), config).expect("config");
    std::fs::create_dir_all(dir.join("checkout")).expect("checkout");

    let mock_log = std::fs::File::create(dir.join("mock.log")).expect("mock log");
    let mock = Command::new(bin_exe("mock_opencode"))
        .env("MOCK_MODE", "agent")
        .env("MOCK_PORT", mock_port.to_string())
        .env("MOCK_MCP_URL", format!("http://127.0.0.1:{server_port}"))
        .env("MOCK_MCP_TOKEN", BEARER)
        .env("MOCK_MCP_PROJECT", PROJECT)
        .env("MOCK_STEP_MS", "300")
        .stdout(Stdio::from(mock_log.try_clone().expect("mock log clone")))
        .stderr(Stdio::from(mock_log))
        .spawn()
        .expect("spawn mock_opencode");
    let server_log = std::fs::File::create(dir.join("server.log")).expect("server log");
    let server = Command::new(bin_exe("uwuwu-server"))
        .env("UWUWU_SERVER_CONFIG", dir.join("config.toml"))
        .env("UWUWU_SERVER_STATE", dir.join("state.json"))
        .env("UWUWU_SERVER_BEARER", BEARER)
        .env("UWUWU_OPENCODE_PASSWORD", "e2e")
        .env("GH_TOKEN", "e2e-dummy-token")
        .env("UWUWU_MOCK_SEED", seed)
        .env("RUST_LOG", "info")
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
    let deadline = Instant::now() + Duration::from_secs(30);
    assert!(
        healthy(&http, stand.port, deadline).await,
        "сервер стенда не поднялся\n{}",
        log_tail(&stand.dir.join("server.log"))
    );
    stand
}

async fn healthy(http: &reqwest::Client, port: u16, deadline: Instant) -> bool {
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

async fn mcp_call(http: &reqwest::Client, port: u16, tool: &str, args: Value) -> String {
    let resp = http
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("Authorization", format!("Bearer {BEARER}"))
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

fn log_tail(path: &Path) -> String {
    let binding = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = binding.lines().collect();
    let start = lines.len().saturating_sub(30);
    lines[start..].join("\n")
}

fn last_event(log: &Path, event: &str) -> Option<Value> {
    std::fs::read_to_string(log)
        .ok()?
        .lines()
        .rev()
        .find_map(|line| {
            let v: Value = serde_json::from_str(line).ok()?;
            (v["fields"]["event"] == event).then_some(v)
        })
}

fn count_event(log: &Path, needle: &str) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(needle))
        .count()
}

async fn wait_event(log: &Path, event: &str, secs: u64, what: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = last_event(log, event) {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "{what} не наступило за {secs}с\n{}",
            log_tail(log)
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn wait_status(
    http: &reqwest::Client,
    port: u16,
    number: u64,
    want: &str,
    secs: u64,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let row = mcp_call(
            http,
            port,
            "task_get",
            json!({ "number": number, "project": PROJECT }),
        )
        .await;
        if row.contains(want) {
            return row;
        }
        assert!(
            Instant::now() < deadline,
            "строка {number} не дошла до «{want}» за {secs}с: {row}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn binding_of(dir: &Path) -> Value {
    let raw = std::fs::read_to_string(dir.join("state.json")).expect("state.json читается");
    let state: Value = serde_json::from_str(&raw).expect("state.json валиден");
    state["master"]["analyst-reviewer:stand"].clone()
}

/// Номер из квитанции «task #N created …» — строки нумерует носитель.
fn created_number(receipt: &str) -> u64 {
    receipt
        .split('#')
        .nth(1)
        .and_then(|t| t.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("create не отдал номер: {receipt}"))
}

async fn seed_review_row(http: &reqwest::Client, port: u16, title: &str, probe: &str) -> u64 {
    let receipt = mcp_call(
        http,
        port,
        "task_create",
        json!({ "project": PROJECT, "title": title,
                "body": format!("## Постановка\n\n{probe}\n"),
                "priority": "medium", "automerge": true }),
    )
    .await;
    let number = created_number(&receipt);
    mcp_call(
        http,
        port,
        "task_edit",
        json!({ "number": number, "project": PROJECT, "status": "Review Analys" }),
    )
    .await;
    number
}

#[tokio::test]
async fn review_wave_rides_master_session_then_respawns_after_loss() {
    if !e2e_enabled() {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }
    let dir = std::env::temp_dir().join(format!("analys-review-stand-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tempdir");
    let stand = spawn_stand(&dir, &format!("{BOARD}:901:Review Analys")).await;
    let http = reqwest::Client::new();
    let server_log = stand.dir.join("server.log");

    // 1. Первая волна: привязки нет — спавн мастер-сессии, мок доводит
    // строку до Todo, привязка пишется в state.json.
    let spawned = wait_event(&server_log, EV_SPAWNED, 60, "первый спавн волны").await;
    let master = spawned["fields"]["session"]
        .as_str()
        .expect("session id")
        .to_string();
    wait_status(&http, stand.port, 901, "Todo", 60).await;
    let bound = binding_of(&stand.dir);
    assert_eq!(
        bound["session"].as_str(),
        Some(master.as_str()),
        "привязка указывает на спавн-сессию"
    );
    assert_eq!(
        bound["model"].as_str(),
        Some(MODEL),
        "модель рождения — карта сервера (срез 4)"
    );
    assert!(
        bound["checkout"].as_str().is_some_and(|c| !c.is_empty()),
        "checkout привязки записан"
    );

    // 2. Вторая волна: та же сессия reuse'ится, новых спавнов нет.
    let second = seed_review_row(&http, stand.port, "вторая волна", "probe-reuse").await;
    let reused = wait_event(&server_log, EV_REUSED, 60, "reuse второй волны").await;
    assert_eq!(
        reused["fields"]["session"].as_str(),
        Some(master.as_str()),
        "вторая волна едет в ту же сессию"
    );
    assert_eq!(
        count_event(&server_log, EV_SPAWNED),
        1,
        "новых спавнов нет — reuse, не пересоздание"
    );
    wait_status(&http, stand.port, second, "Todo", 60).await;

    // 3. Idle-компакт: нулевые пороги admit'ят нативный compact мастера.
    wait_event(&server_log, EV_COMPACTED, 90, "компакт мастера").await;
    assert!(
        binding_of(&stand.dir)["compact_ms"].as_u64().unwrap_or(0) > 0,
        "метка компакта персистентна"
    );

    // 4. Потеря сессии: удаление на исполнителе → следующая волна
    // пересоздаёт сессию и перезаписывает привязку (без застревания).
    let del: reqwest::Response = http
        .delete(format!(
            "http://127.0.0.1:{}/api/session/{master}",
            stand.mock_port
        ))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("DELETE session");
    assert!(del.status().is_success(), "мок удалил сессию");
    let third = seed_review_row(&http, stand.port, "третья волна", "probe-respawn").await;
    let deadline = Instant::now() + Duration::from_secs(60);
    let respawned = loop {
        if count_event(&server_log, EV_SPAWNED) >= 2 {
            break last_event(&server_log, EV_SPAWNED).expect("второй спавн");
        }
        assert!(
            Instant::now() < deadline,
            "пересоздание после потери не наступило\n{}",
            log_tail(&server_log)
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    let fresh = respawned["fields"]["session"].as_str().expect("session id");
    assert_ne!(fresh, master, "сессия пересоздана, не воскрешена");
    assert_eq!(
        binding_of(&stand.dir)["session"].as_str(),
        Some(fresh),
        "привязка перезаписана новой сессией"
    );
    wait_status(&http, stand.port, third, "Todo", 60).await;

    drop(stand);
    let _ = std::fs::remove_dir_all(&dir);
}
