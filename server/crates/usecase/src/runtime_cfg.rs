//! Операционные значения рантайма (uwuwu#1726): единственный носитель —
//! закоммиченный `server/config.toml`, встраиваемый в бинарь через include_str!.
//! В коде нет литералов-дублей: смена значения в TOML меняет поведение без
//! правки сорсов и тестов. Отсутствующий ключ — громкий отказ с именем.

use std::sync::OnceLock;

static RAW: OnceLock<toml::Value> = OnceLock::new();

fn raw() -> &'static toml::Value {
    RAW.get_or_init(|| {
        let src = include_str!("../../../config.toml");
        toml::from_str(src).expect("server/config.toml: валидный TOML (uwuwu#1726)")
    })
}

fn value(section: &str, key: &str) -> &'static toml::Value {
    raw()
        .get(section)
        .and_then(|table| table.get(key))
        .unwrap_or_else(|| panic!("config.toml: нет [{section}] {key} (uwuwu#1726)"))
}

/// Число с явным приведением; неверный тип — отказ с именем ключа.
pub fn usize_of(section: &str, key: &str) -> usize {
    value(section, key)
        .as_integer()
        .unwrap_or_else(|| panic!("config.toml: [{section}] {key} не число")) as usize
}

pub fn u64_of(section: &str, key: &str) -> u64 {
    usize_of(section, key) as u64
}

pub fn u32_of(section: &str, key: &str) -> u32 {
    usize_of(section, key) as u32
}

pub fn bool_of(section: &str, key: &str) -> bool {
    value(section, key)
        .as_bool()
        .unwrap_or_else(|| panic!("config.toml: [{section}] {key} не bool"))
}

pub fn text(section: &str, key: &str) -> String {
    value(section, key)
        .as_str()
        .unwrap_or_else(|| panic!("config.toml: [{section}] {key} не строка"))
        .to_string()
}

/// Секция «имя → строка» (карты моделей).
pub fn table(section: &str, key: &str) -> std::collections::HashMap<String, String> {
    value(section, key)
        .as_table()
        .unwrap_or_else(|| panic!("config.toml: [{section}] {key} не таблица"))
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.as_str()
                    .unwrap_or_else(|| panic!("config.toml: [{section}] {key}.{k} не строка"))
                    .to_string(),
            )
        })
        .collect()
}

/// Карты моделей: единственный носитель — `[executor]` config.toml.
pub mod executor {
    use super::*;
    pub fn models() -> std::collections::HashMap<String, String> {
        table("executor", "models")
    }
    pub fn aliases() -> std::collections::HashMap<String, String> {
        table("executor", "model_aliases")
    }
}

/// Удобные доступы для часто используемых секций.
pub mod search {
    use super::*;
    pub fn budget() -> usize {
        usize_of("search", "semantic_sync_budget")
    }
    pub fn model() -> String {
        text("search", "embed_model")
    }
    pub fn dims() -> usize {
        usize_of("search", "embed_dims")
    }
    pub fn batch() -> usize {
        usize_of("search", "embed_batch")
    }
    pub fn connect_secs() -> u64 {
        u64_of("search", "connect_secs")
    }
    pub fn query_total_secs() -> u64 {
        u64_of("search", "query_total_secs")
    }
    pub fn batch_total_secs() -> u64 {
        u64_of("search", "batch_total_secs")
    }
    pub fn chunk_chars() -> usize {
        usize_of("search", "chunk_chars")
    }
    pub fn digest_chars() -> usize {
        usize_of("search", "digest_chars")
    }
    pub fn recent_issues_cap() -> usize {
        usize_of("search", "recent_issues_cap")
    }
    pub fn overfetch() -> usize {
        usize_of("search", "query_overfetch_factor")
    }
    pub fn retention_secs() -> u64 {
        u64_of("search", "retention_secs")
    }
    pub fn backoff_ms() -> u64 {
        u64_of("search", "sync_backoff_ms")
    }
}

pub mod pipeline {
    use super::*;
    pub fn peak_provider() -> String {
        text("pipeline", "peak_provider")
    }
    pub fn sweep_cap() -> usize {
        usize_of("pipeline", "sweep_batch_cap")
    }
    pub fn budget_floor() -> u64 {
        u64_of("pipeline", "budget_floor")
    }
    pub fn env_gc_cap() -> usize {
        usize_of("pipeline", "env_gc_batch_cap")
    }
    pub fn env_gc_deadline_ms() -> u64 {
        u64_of("pipeline", "env_gc_verdict_deadline_ms")
    }
    pub fn clean_cap() -> usize {
        usize_of("pipeline", "clean_batch_cap")
    }
    pub fn task_env_gc_script() -> String {
        text("pipeline", "task_env_gc_script")
    }
    pub fn spawn_cooldown_ms() -> u64 {
        u64_of("pipeline", "spawn_cooldown_ms")
    }
    pub fn stall_silence_ms() -> u64 {
        u64_of("pipeline", "stall_silence_ms")
    }
    pub fn stall_ping_every_ms() -> u64 {
        u64_of("pipeline", "stall_ping_every_ms")
    }
    pub fn stall_max_pings() -> u32 {
        u32_of("pipeline", "stall_max_pings")
    }
    pub fn stall_soft_silence_ms() -> u64 {
        u64_of("pipeline", "stall_soft_silence_ms")
    }
    pub fn stall_soft_every_ms() -> u64 {
        u64_of("pipeline", "stall_soft_every_ms")
    }
    pub fn probe_degrade_min_rows() -> usize {
        usize_of("pipeline", "probe_degrade_min_rows")
    }
    pub fn session_lost_escalate_after() -> u32 {
        u32_of("pipeline", "session_lost_escalate_after")
    }
    pub fn review_round_cap() -> u32 {
        u32_of("pipeline", "review_round_cap")
    }
    pub fn task_env_command_budget_ms() -> u64 {
        u64_of("pipeline", "task_env_command_budget_ms")
    }
    pub fn cron_dir() -> String {
        text("pipeline", "cron_dir")
    }
    pub fn cron_run_silence_ms() -> u64 {
        u64_of("pipeline", "cron_run_silence_ms")
    }
    pub fn incident_cooldown_ms() -> u64 {
        u64_of("pipeline", "incident_cooldown_ms")
    }
    pub fn verify_attempts() -> usize {
        usize_of("pipeline", "verify_attempts")
    }
    pub fn verify_pause_ms() -> u64 {
        u64_of("pipeline", "verify_pause_ms")
    }
    pub fn state_cap() -> usize {
        usize_of("pipeline", "state_cap")
    }
    pub fn sidecar_timeout_ms() -> u64 {
        u64_of("pipeline", "sidecar_timeout_ms")
    }
    pub fn spawns_max_per_hour() -> usize {
        usize_of("pipeline", "spawns_max_per_hour")
    }
}

pub mod github {
    use super::*;
    pub fn http_timeout_secs() -> u64 {
        u64_of("github", "http_timeout_secs")
    }
    pub fn rest_per_page() -> usize {
        usize_of("github", "rest_per_page")
    }
    pub fn gql_budget_low() -> u64 {
        u64_of("github", "gql_budget_low")
    }
}

pub mod blob {
    use super::*;
    pub fn max_bytes() -> usize {
        usize_of("blob", "max_bytes")
    }
    pub fn presign_ttl_secs() -> u64 {
        u64_of("blob", "presign_ttl_secs")
    }
    pub fn presign_put_ttl_secs() -> u64 {
        u64_of("blob", "presign_put_ttl_secs")
    }
    pub fn put_sign_ttl_secs() -> u64 {
        u64_of("blob", "put_sign_ttl_secs")
    }
}

pub mod api {
    use super::*;
    pub fn default_project() -> String {
        text("api", "default_project")
    }
    pub fn card_comments() -> usize {
        usize_of("api", "card_comments")
    }
    pub fn section_guard_floor() -> usize {
        usize_of("api", "section_guard_floor")
    }
    pub fn title_max_chars() -> usize {
        usize_of("api", "title_max_chars")
    }
    pub fn person_slug() -> String {
        text("api", "person_slug")
    }
}
