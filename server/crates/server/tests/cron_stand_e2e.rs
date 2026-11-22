//! E2E основного сценария #1480 (гейт UWUWU_E2E=1): живой uwuwu-server
//! (hub driver=memory, executor = mock_opencode) файрит кроны — машинный
//! учёт ранов в state.json, без журнального носителя.
//!
//! Сценарий (главный путь UC-02, то, что тестировщик гонял руками на
//! стенде scratch/stand):
//!   1. файр: state-запись до спавна (last_fire + run-трек), сессия с
//!      тайтлом «⏰ …» на проект-исполнителе;
//!   2. завершение рана: сеттлмент снимает трек с сессии исполнителя
//!      (outcome succeeded → трек закрыт, отказов нет);
//!   3. `cron_list` показывает машинный учёт (last fire, fails);
//!   4. журнала-отчёта не существует: docs_ls cron — только README,
//!      ни один док не появился как отчётная сводка.
//!
//! Запуск: UWUWU_E2E=1 cargo test -p uwuwu-server --test cron_stand_e2e

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").ok().as_deref() == Some("1")
}

/// Свободный TCP-порт (бинд :0, слушатель сразу отпускается — стендовый
/// гонки окон закрыты повторными попытками health/старта).
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

struct Stand {
    #[allow(dead_code)]
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

fn log_tail(path: &Path) -> String {
    let binding = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = binding.lines().collect();
    let start = lines.len().saturating_sub(30);
    lines[start..].join("\n")
}

async fn health(http: &reqwest::Client, port: u16, bearer: &str, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if let Ok(resp) = http
            .get(format!("http://127.0.0.1:{port}/health"))
            .header("Authorization", format!("Bearer {bearer}"))
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

async fn mcp_call(
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

#[tokio::test]
async fn cron_fire_machine_accounting_without_journal() {
    if !e2e_enabled() {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }

    let dir = std::env::temp_dir().join(format!("cron-stand-e2e-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("checkout")).expect("tempdir");
    let server_port = free_port();
    let mock_port = free_port();
    let bearer = "e2e-stand-bearer";

    let masters = json!({
        "cron/e2e-zayavitel/README": "---\ntitle: E2E заявитель\nschedule: 1m\nproject: stand\n---\n\nЗаявитель: находка → заявка. Журнала нет.\n",
        "cron/e2e-isp/README": "---\ntitle: E2E исполнитель\nschedule: 1m\nproject: stand\n---\n\nИсполнитель: тихий тик в целевом носителе.\n"
    });
    let config = format!(
        "mode = \"live\"\npoll_sec = 10\nport = {server_port}\n\n\
         [hub]\nrepo = \"e2e/hub\"\ndriver = \"memory\"\n\n\
         [executor]\nname = \"mock\"\nurl = \"http://127.0.0.1:{mock_port}\"\n\n\
         [policy]\ncup = 2\n\n[policy.peak]\nenabled = false\nstart_min = 360\nend_min = 600\n\n\
         [[project]]\nname = \"stand\"\nboard = \"stand\"\nrepo = \"e2e/stand\"\n\
         checkout = \"{dir}/checkout\"\nenv_root = \"{dir}\"\n",
        server_port = server_port,
        mock_port = mock_port,
        dir = dir.display()
    );
    std::fs::write(dir.join("config.toml"), config).expect("config");

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
    let server = Command::new(bin_exe("uwuwu-server"))
        .env("UWUWU_SERVER_CONFIG", dir.join("config.toml"))
        .env("UWUWU_SERVER_STATE", dir.join("state.json"))
        .env("UWUWU_SERVER_BEARER", bearer)
        .env("UWUWU_OPENCODE_PASSWORD", "e2e")
        .env("GH_TOKEN", "e2e-dummy-token")
        .env("UWUWU_MOCK_DOCS", masters.to_string())
        .env("RUST_LOG", "info")
        .stdout(Stdio::from(
            server_log.try_clone().expect("server log clone"),
        ))
        .stderr(Stdio::from(server_log))
        .spawn()
        .expect("spawn uwuwu-server");
    let stand = Stand {
        dir: dir.clone(),
        mock,
        server,
        port: server_port,
        mock_port,
    };

    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    assert!(
        health(&http, stand.port, bearer, deadline).await,
        "сервер стенда не поднялся\n{}",
        log_tail(&stand.dir.join("server.log"))
    );

    // 1+2. Файр + сеттлмент: оба мастера с last_fire, треки сняты, отказов
    // нет (мок plain: ход завершается succeeded за step_ms, следующий тик
    // закрывает трек). Тик ~10 c, файр-волна успевает за два цикла.
    let state_path = stand.dir.join("state.json");
    let poll_deadline = Instant::now() + Duration::from_secs(45);
    let state: Value = loop {
        assert!(
            Instant::now() < poll_deadline,
            "state не дожил до учётного вида\nstate: {:?}\n{}",
            std::fs::read_to_string(&state_path).unwrap_or_default(),
            log_tail(&stand.dir.join("server.log"))
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        let Ok(text) = std::fs::read_to_string(&state_path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let crons = &v["crons"];
        let z = &crons["e2e-zayavitel"];
        let i = &crons["e2e-isp"];
        let fired = |row: &Value| row["last_fire_ms"].as_u64().unwrap_or(0) > 0;
        let settled = |row: &Value| row.get("run").is_none();
        let clean = |row: &Value| row["fails"].as_u64().unwrap_or(0) == 0;
        if [z, i].iter().all(|r| fired(r) && settled(r) && clean(r)) {
            break v;
        }
    };
    assert!(
        state["crons"]["e2e-zayavitel"]["last_fire_ms"].is_u64(),
        "last_fire not machine-recorded"
    );

    // Сессии ранов на моке: тайтл «⏰ …» (UC-05), directory — чекаут проекта.
    let sessions: Value = http
        .get(format!("http://127.0.0.1:{}/api/session", stand.mock_port))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("mock sessions")
        .json()
        .await
        .expect("mock sessions JSON");
    let rows = sessions["data"].as_array().expect("session list");
    let cron_rows: Vec<&Value> = rows
        .iter()
        .filter(|s| {
            s["title"]
                .as_str()
                .map(|t| t.starts_with("⏰ "))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        cron_rows.len() >= 2,
        "ожидаем раны обоих мастеров, есть: {cron_rows:?}"
    );
    for s in &cron_rows {
        let dir_of = s["location"]["directory"].as_str().unwrap_or_default();
        assert_eq!(
            dir_of,
            format!("{}/checkout", dir.display()),
            "ран спавнится в checkout проекта"
        );
    }

    // 3. cron_list — машинный учёт наружу: last fire + fails, без «never».
    let list = mcp_call(&http, stand.port, bearer, "cron_list", json!({})).await;
    assert!(
        list.contains("E2E заявитель") && list.contains("E2E исполнитель"),
        "{list}"
    );
    assert!(
        list.contains("· last fire 2026-"),
        "last fire не показан: {list}"
    );
    assert!(list.contains("fails 0"), "fails не показан: {list}");
    assert!(
        !list.contains("never"),
        "после файра «never» недопустим: {list}"
    );

    // 4. Журнала нет: инвентарь /cron — только README-мастера, ни одного
    // отчётного дока (снесённый носитель не возрождается).
    let docs = mcp_call(
        &http,
        stand.port,
        bearer,
        "docs_ls",
        json!({ "path": "cron" }),
    )
    .await;
    assert!(docs.contains("cron/e2e-zayavitel/README"), "{docs}");
    assert!(docs.contains("cron/e2e-isp/README"), "{docs}");
    assert!(
        !docs.contains("Журнал"),
        "журнальный носитель возродился: {docs}"
    );
    assert_eq!(
        docs.lines().filter(|l| !l.trim().is_empty()).count(),
        2,
        "в /cron появились посторонние доки: {docs}"
    );

    drop(stand);
    let _ = std::fs::remove_dir_all(&dir);
}
