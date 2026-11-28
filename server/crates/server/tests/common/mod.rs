//! Общий харнес пин-тестов мока (#1254): спавн реального бинаря
//! mock_opencode на свободном порту, ожидание готовности, probe-сессии.
//! Вынесен из mock_route_coverage.rs — делится с executor_form_contract.rs.
//! Каждый integration-таргет компилирует модуль целиком — не таргетные
//! хелперы мертвы локально, не по факту.

#![allow(dead_code)]

pub mod form;
pub mod stand;

/// Обязательные секции конфига для стендов (uwuwu#1726): один тестовый
/// носитель вместо копий продовых значений по каждому харнесу.
/// Карты моделей по умолчанию: только если харнес не задал свои.
pub const DEFAULT_MODELS: &str = r#"
[executor.model_aliases]
flash = "opencode-go/hy4-flash-free#high"
common = "opencode-go/hy4-flash-free#high"
smart = "zai-coding-plan/glm-5.4#high"
[executor.models]
default = "flash"
developer = "flash"
plan-reviewer = "smart"
code-quality-reviewer = "smart"
tester = "flash"
test-reviewer = "smart"
analyst = "flash"
analyst-reviewer = "smart"
dreamer = "common"
"#;

pub const RUNTIME_CFG: &str = r#"
[search]
semantic_sync_budget = 512
embed_model = "embeddinggemma-2:740m"
embed_dims = 768
embed_batch = 38
connect_secs = 2
query_total_secs = 15
batch_total_secs = 60
chunk_chars = 1200
digest_chars = 4000
recent_issues_cap = 400
query_overfetch_factor = 3
retention_secs = 180
sync_backoff_ms = 3600000
[pipeline]
sweep_days = 7
peak_provider = "zai-coding-plan"
sweep_batch_cap = 50
budget_floor = 500
env_gc_batch_cap = 20
env_gc_verdict_deadline_ms = 900000
env_gc_bootstrap_delete_attempts = 3
clean_batch_cap = 50
slot_session_live_ms = 7200000
review_round_cap = 4
clean_journal_cap = 100
spawn_cooldown_ms = 900000
stall_silence_ms = 3600000
stall_ping_every_ms = 7200000
stall_max_pings = 2
stall_soft_silence_ms = 900000
stall_soft_every_ms = 1800000
probe_degrade_min_rows = 5
session_lost_escalate_after = 3
limit_signal_threshold = 1
limit_signal_cooldown_ms = 21600000
analyst_wave_cooldown_ms = 900000
analyst_cycle_limit = 3
analyst_review_cooldown_ms = 900000
dreamer_idle_ms = 1800000
dreamer_day_cap = 24
cron_dir = "cron"
cron_run_silence_ms = 21600000
cron_fire_cooldown_ms = 3600000
cron_late_fire_window_ms = 86400000
incident_threshold = 3
incident_window_ms = 3600000
incident_cooldown_ms = 3600000
executor_unavailable_threshold = 3
executor_unavailable_window_ms = 3600000
executor_unavailable_cooldown_ms = 3600000
task_env_command_budget_ms = 20000
verify_attempts = 3
verify_pause_ms = 1000
board_verify_attempts = 3
state_cap = 5000
post_retention_days = 7
spawns_max_per_hour = 10
judge_restart_recent_ms = 3600000
spawn_budget_window_ms = 3600000
memory_snapshot_period_ms = 1800000
sidecar_timeout_ms = 120000
semantic_sync_timeout_ms = 0
task_env_gc_script = "$HOME/uwuwu-agent/server/ops/task-env-gc.sh"
[api]
default_project = "uwuwu"
card_comments = 10
section_guard_floor = 1024
title_max_chars = 120
goal_gist_glyphs = 120
person_slug = "person/README"
post_retention_days = 7
[github]
http_timeout_secs = 120
page_size = 50
max_pages = 20
verify_gql_attempts = 5
verify_gql_pause_ms = 1000
transport_retry_pause_ms = 3000
transport_retry_attempts = 2
item_pages = 10
items_ttl_secs = 240
meta_ttl_secs = 600
status_ttl_secs = 240
snap_cache_ttl_secs = 240
status_cache_ttl_secs = 60
gql_backoff_secs = 300
wiki_cache_ttl_secs = 300
gql_budget_low = 300
rest_per_page = 100
[blob]
max_bytes = 20971520
presign_ttl_secs = 604800
presign_put_ttl_secs = 300
put_sign_ttl_secs = 60
[bots]
tg_poll_sec = 25
retry_pause_ms = 10000
tg_http_timeout_secs = 35
guard_backoff_base_ms = 30000
guard_backoff_cap_ms = 1800000
guard_healthy_ms = 600000
reply_cap_ms = 3600000
reply_poll_ms = 2000
asset_name_max_bytes = 120
chunk_reply_chars = 4000
bot_debounce_ms = 60000
user_msg_poll_ms = 3000
stale_update_ms = 1800000
session_page_limit = 1000
session_max_pages = 100
gate_http_timeout_secs = 20000
poll_limit = 5
list_limit = 10
"#;

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
