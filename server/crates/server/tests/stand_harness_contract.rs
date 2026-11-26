//! Контракт общего харнеса e2e-стендов (`tests/common/stand.rs`, #1574).
//!
//! Два класса пинов:
//!   1. Поведение хелперов, перенесённых в общий модуль (`log_tail`,
//!      `e2e_enabled`) — чистая логика без живых процессов.
//!   2. Инвариант дедупа: оба стенда потребуют общий модуль и не держат
//!      локальных копий харнеса; сцены стендов сохранены.
//!
//! Живые сцены (спавн сервера/мока, health, MCP, Drop-гард) покрыты самими
//! e2e-таргетами под `UWUWU_E2E=1` — здесь они не дублируются.

mod common;

use common::stand::{e2e_enabled, log_tail};
use std::fs;
use std::path::PathBuf;

fn scratch(path: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "stand-harness-contract-{}-{path}",
        std::process::id()
    ))
}

#[test]
fn log_tail_missing_file_is_empty() {
    let dir = scratch("missing");
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(log_tail(&dir.join("absent.log")), "");
}

#[test]
fn log_tail_short_file_returns_all_lines() {
    let dir = scratch("short");
    fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("short.log");
    fs::write(&path, "alpha\nbeta\ngamma\n").expect("write log");
    assert_eq!(log_tail(&path), "alpha\nbeta\ngamma");
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn log_tail_long_file_returns_last_30_lines() {
    let dir = scratch("long");
    fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("long.log");
    let content: String = (0..50).map(|i| format!("line-{i}\n")).collect();
    fs::write(&path, &content).expect("write log");
    let tail = log_tail(&path);
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 30, "хвост обязан быть ровно 30 строк: {tail}");
    assert_eq!(lines.first().copied(), Some("line-20"));
    assert_eq!(lines.last().copied(), Some("line-49"));
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn e2e_enabled_gate_requires_exactly_one() {
    let saved = std::env::var("UWUWU_E2E").ok();
    std::env::set_var("UWUWU_E2E", "1");
    assert!(e2e_enabled(), "UWUWU_E2E=1 включает стенд");
    for off in ["true", "yes", "0", ""] {
        std::env::set_var("UWUWU_E2E", off);
        assert!(!e2e_enabled(), "UWUWU_E2E={off:?} не должен включать стенд");
    }
    std::env::remove_var("UWUWU_E2E");
    assert!(!e2e_enabled(), "без гейта стенд выключен");
    match saved {
        Some(value) => std::env::set_var("UWUWU_E2E", value),
        None => std::env::remove_var("UWUWU_E2E"),
    }
}

const CRON_SRC: &str = include_str!("cron_stand_e2e.rs");
const QUOTA_SRC: &str = include_str!("quota_limit_pause_stand_e2e.rs");
const STAND_SRC: &str = include_str!("common/stand.rs");

#[test]
fn stands_consume_shared_harness_without_local_copies() {
    for (name, src) in [("cron", CRON_SRC), ("quota", QUOTA_SRC)] {
        assert!(src.contains("mod common;"), "{name}: нет mod common");
        assert!(
            src.contains("use common::stand"),
            "{name}: нет импорта общего харнеса"
        );
        for dup in [
            "fn e2e_enabled",
            "fn log_tail",
            "fn free_port",
            "fn bin_exe",
            "fn health",
            "fn mcp_call",
            "fn spawn_stand",
            "struct Stand",
            "impl Drop for Stand",
        ] {
            assert!(
                !src.contains(dup),
                "{name}: локальная копия `{dup}` — дедуп сломан, харнес обязан жить в common/stand.rs"
            );
        }
    }
}

#[test]
fn shared_harness_defines_the_single_copy() {
    for marker in [
        "pub const BEARER",
        "pub struct StandOptions",
        "pub struct Stand",
        "impl Drop for Stand",
        "pub fn e2e_enabled",
        "fn free_port",
        "fn bin_exe",
        "pub async fn spawn_stand",
        "pub fn log_tail",
        "async fn health",
        "pub async fn mcp_call",
    ] {
        assert!(
            STAND_SRC.contains(marker),
            "общий харнес потерял `{marker}` — сцены стендов останутся без хелпера"
        );
    }
}

#[test]
fn stand_scenarios_are_preserved() {
    assert!(CRON_SRC.contains("fn cron_fire_machine_accounting_without_journal"));
    assert!(CRON_SRC.contains("fn corrupt_state_restart_anchors_without_catchup"));
    assert!(QUOTA_SRC.contains("fn limit_pause_holds_row_then_releases_when_window_drains"));
}
