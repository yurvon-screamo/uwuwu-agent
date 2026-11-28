//! E2E #1273: TOML-ключ `port` доезжает до HTTP-бинда живого сервера.
//!
//! Юнит `config_toml_port_key_reaches_port` проверяет парсинг → `port()`;
//! этот тест — связку «конфиг-файл → бинарь → TCP listening», которую
//! стенды не гоняют (порты стендов передаются env-ом). Спавн uwuwu-server
//! с memory-конфигом: порт берётся у ОС (bind :0 → отпустить → отдать
//! конфигу), PORT у ребёнка снят — проверяется именно TOML-путь, без
//! оглядки на глобальное окружение раннера.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Свободный порт: ОС выдаёт ephemeral на bind :0, слушатель тут же
/// отпущен — к моменту бинда ребёнка порт почти наверняка свободен.
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind :0")
        .local_addr()
        .expect("local_addr")
        .port()
}

/// Минимальный memory-конфиг + логи ребёнка — всё в tempdir (сервер
/// кладёт lancedb рядом с cwd, мусор в тестовом каталоге не нужен).
fn spawn_server(port: u16) -> (Child, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("uwuwu_port_bind_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tempdir создан");
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"mode = "observe"
poll_sec = 3600
port = {port}

[hub]
repo = "a/b"
driver = "memory"

[executor]
name = "kisa"
url = "http://127.0.0.1:1"

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

[policy]
cup = 1
peak = {{ enabled = false, start_min = 0, end_min = 0 }}

[[project]]
name = "p"
board = "B"
repo = "a/c"
checkout = "/tmp/co"
env_root = "/tmp/env"

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
peak_provider = "zai-coding-plan"
sweep_batch_cap = 50
sweep_days = 7
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
"#,
        ),
    )
    .expect("конфиг записан");
    let log = dir.join("server.log");
    let child = Command::new(env!("CARGO_BIN_EXE_uwuwu-server"))
        .current_dir(&dir)
        .env_remove("PORT")
        .env("UWUWU_SERVER_CONFIG", &cfg)
        .env("GH_TOKEN", "test")
        .env("UWUWU_SERVER_BEARER", "test")
        .env("UWUWU_OPENCODE_PASSWORD", "mockpass")
        .stdout(Stdio::from(
            std::fs::File::create(&log).expect("лог открыт"),
        ))
        .stderr(Stdio::null())
        .spawn()
        .expect("сервер спавнен");
    (child, log, dir)
}

#[test]
fn toml_port_key_reaches_http_bind() {
    let port = free_port();
    let (mut child, log, dir) = spawn_server(port);

    // Подтверждение бинда в логе — событие http.listening с нашим портом
    // (JSON-лог сервера, оба поля одного tracing-события). Греп закрывает
    // TOCTOU-окно free_port: чужой слушатель на отпущенном ephemeral-порту
    // дал бы зелёный connect без нашей строки в логе.
    let bind_line = format!("\"event\":\"http.listening\",\"port\":{port}");
    let logged = || std::fs::read_to_string(&log).is_ok_and(|l| l.contains(&bind_line));

    // TCP-connect на TOML-порт до успеха, смерти ребёнка или таймаута.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut bound = false;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            bound = true;
            break;
        }
        if child.try_wait().expect("статус ребёнка").is_some() {
            break; // умер раньше бинда — assert ниже покажет лог
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Строка http.listening может отставать от успешного connect на
    // миллисекунды — дать логу дозаписаться до kill (2с максимум).
    if bound {
        let mut logged_now = logged();
        let write_deadline = Instant::now() + Duration::from_secs(2);
        while !logged_now && Instant::now() < write_deadline {
            if child.try_wait().map_or(true, |s| s.is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            logged_now = logged();
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    // Лог читается до очистки каталога: после remove_dir_all файла нет,
    // и диагностика падения была бы всегда пустой (Low 1 тест-ревью).
    let log_contents = std::fs::read_to_string(&log).unwrap_or_default();
    std::fs::remove_dir_all(&dir).ok(); // lancedb/логи — мусор за собой

    assert!(
        bound && log_contents.contains(&bind_line),
        "сервер не подтвердил http.listening port={port} (bound={bound}); лог:\n{}",
        log_contents
    );
}
