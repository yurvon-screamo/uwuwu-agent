//! Стенд живого uwuwu-server против disposable GitHub-рига: бинарь
//! ветки, observe-режим, MCP tools/call с bearer-гейтом.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Stand {
    dir: PathBuf,
    server: Child,
    port: u16,
}

impl Drop for Stand {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub async fn spawn(dir: &Path, rig: &str, token: &str) -> Stand {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let bearer = "witness-e2e-bearer";
    let config = format!(
        "mode = \"observe\"\npoll_sec = 3600\nport = {port}\n\
         sweep_days = 0\nenv_gc_days = 0\n\n\
         [hub]\nrepo = \"{rig}\"\n\n\
         [executor]\nname = \"mock\"\nurl = \"http://127.0.0.1:1\"\n\n\
         [policy]\ncup = 1\n\n[policy.peak]\nenabled = false\nstart_min = 360\nend_min = 600\n\n\
         [[project]]\nname = \"rig\"\nboard = \"rig\"\nrepo = \"{rig}\"\n\
         checkout = \"{dir}/checkout\"\nenv_root = \"{dir}\"\n",
        dir = dir.display()
    );
    std::fs::write(dir.join("config.toml"), config).expect("config");
    let log = std::fs::File::create(dir.join("server.log")).expect("server log");
    let server = Command::new(
        std::env::var("CARGO_BIN_EXE_uwuwu-server").expect("CARGO_BIN_EXE_uwuwu-server"),
    )
    .env("UWUWU_SERVER_CONFIG", dir.join("config.toml"))
    .env("UWUWU_SERVER_STATE", dir.join("state.json"))
    .env("UWUWU_SERVER_BEARER", bearer)
    .env("GH_TOKEN", token)
    .env_remove("GEMINI_API_KEY")
    .env("RUST_LOG", "warn")
    .stdout(Stdio::from(log.try_clone().expect("log clone")))
    .stderr(Stdio::from(log))
    .spawn()
    .expect("spawn uwuwu-server");
    let stand = Stand {
        dir: dir.to_path_buf(),
        server,
        port,
    };
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(resp) = http
            .get(format!("http://127.0.0.1:{port}/health"))
            .header("Authorization", format!("Bearer {bearer}"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            if resp.status().is_success() {
                return stand;
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    panic!("сервер рига не поднялся");
}

pub async fn mcp_call(
    http: &reqwest::Client,
    stand: &Stand,
    tool: &str,
    args: Value,
) -> (bool, String) {
    let resp = http
        .post(format!("http://127.0.0.1:{}/mcp", stand.port))
        .header("Authorization", "Bearer witness-e2e-bearer")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": tool, "arguments": args}
        }))
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .expect("mcp transport");
    let body: Value = resp.json().await.expect("mcp json");
    let result = &body["result"];
    let text = result["content"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    (result["isError"].as_bool().unwrap_or(false), text)
}
