//! Общий харнес пин-тестов мока (#1254): спавн реального бинаря
//! mock_opencode на свободном порту, ожидание готовности, probe-сессии.
//! Вынесен из mock_route_coverage.rs — делится с executor_form_contract.rs.
//! Каждый integration-таргет компилирует модуль целиком — не таргетные
//! хелперы мертвы локально, не по факту.

#![allow(dead_code)]

pub mod form;
pub mod github_rig;
pub mod github_stand;

use std::process::{Command, Stdio};

/// Спавн бинаря мока на свободном порту; Drop-гард убивает процесс.
pub struct MockProc(pub std::process::Child);

impl Drop for MockProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Базовый env стенда + `extra` — env-ручки конкретной пробы (окна
/// turn-режима, пагинация и пр.); дефолтная форма — пустой срез.
pub fn spawn_mock(extra: &[(&str, &str)]) -> (String, MockProc) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mock_opencode"));
    cmd.env("MOCK_MODE", "plain")
        .env("MOCK_PORT", port.to_string())
        .env("MOCK_STEP_MS", "50")
        .env("MOCK_MCP_URL", "http://127.0.0.1:1")
        .env("MOCK_MCP_TOKEN", "")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (name, value) in extra {
        cmd.env(name, value);
    }
    let child = cmd
        .spawn()
        .expect("spawn mock_opencode (cargo строит бин для integration-тестов)");
    (format!("http://127.0.0.1:{port}"), MockProc(child))
}

pub async fn wait_ready(base: &str) {
    let http = reqwest::Client::new();
    for _ in 0..100 {
        if let Ok(resp) = http.get(format!("{base}/api/session")).send().await {
            if resp.status().is_success() {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("мок не поднялся за 5с: {base}");
}

/// Probe-сессия с произвольным телом create (agent — для role-сессий):
/// маршруты с {id} ходят каждый по своей — DELETE не ломает соседей.
pub async fn create_session_with(
    base: &str,
    http: &reqwest::Client,
    body: serde_json::Value,
) -> String {
    let resp = http
        .post(format!("{base}/api/session"))
        .json(&body)
        .send()
        .await
        .expect("create probe session");
    let json: serde_json::Value = resp.json().await.expect("create json");
    json["data"]["id"].as_str().expect("probe id").to_string()
}

pub async fn probe_session(base: &str, http: &reqwest::Client) -> String {
    create_session_with(base, http, serde_json::json!({ "title": "route probe" })).await
}
