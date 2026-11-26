//! Контракт общего харнеса e2e-стендов (`tests/common/stand.rs`, #1574).
//!
//! Три класса пинов:
//!   1. Поведение хелперов, перенесённых в общий модуль (`log_tail`,
//!      `e2e_enabled`) — чистая логика без живых процессов.
//!   2. Инвариант дедупа: оба стенда потребуют общий модуль и не держат
//!      локальных копий харнеса.
//!   3. Drop-гард харнеса: убивает и убирает оба дочерних процесса —
//!      поведенческий тест на холостых процессах, сервер не нужен.
//!
//! Живые сцены (спавн сервера/мока, config, health, MCP) покрыты самими
//! e2e-таргетами под `UWUWU_E2E=1` — здесь они не дублируются.

mod common;

use common::stand::{e2e_enabled, log_tail, Stand};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Прецедент гард-уборки: `tests/search/churn_maintenance.rs` (`TempDirGuard`),
/// `src/rename_journal_test_support.rs` (`TempDir`). Каталог убирается и при
/// панике — упавший ассерт не оставляет хвост во временной папке.
struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "stand-harness-contract-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("scratch dir");
        Self(path)
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Возврат env-гейта при любом исходе теста — упавший ассерт не оставляет
/// мутированный `UWUWU_E2E` до конца процесса.
struct EnvGuard {
    key: &'static str,
    saved: Option<String>,
}

impl EnvGuard {
    fn new(key: &'static str) -> Self {
        Self {
            key,
            saved: std::env::var(key).ok(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.saved {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
fn log_tail_missing_file_is_empty() {
    let dir = TempDirGuard::new("missing");
    assert_eq!(log_tail(&dir.0.join("absent.log")), "");
}

#[test]
fn log_tail_short_file_returns_all_lines() {
    let dir = TempDirGuard::new("short");
    let path = dir.0.join("short.log");
    fs::write(&path, "alpha\nbeta\ngamma\n").expect("write log");
    assert_eq!(log_tail(&path), "alpha\nbeta\ngamma");
}

#[test]
fn log_tail_long_file_returns_last_30_lines() {
    let dir = TempDirGuard::new("long");
    let path = dir.0.join("long.log");
    let content: String = (0..50).map(|i| format!("line-{i}\n")).collect();
    fs::write(&path, &content).expect("write log");
    let tail = log_tail(&path);
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 30, "хвост обязан быть ровно 30 строк: {tail}");
    assert_eq!(lines.first().copied(), Some("line-20"));
    assert_eq!(lines.last().copied(), Some("line-49"));
}

#[test]
fn e2e_enabled_gate_requires_exactly_one() {
    let _env = EnvGuard::new("UWUWU_E2E");
    std::env::set_var("UWUWU_E2E", "1");
    assert!(e2e_enabled(), "UWUWU_E2E=1 включает стенд");
    for off in ["true", "yes", "0", ""] {
        std::env::set_var("UWUWU_E2E", off);
        assert!(!e2e_enabled(), "UWUWU_E2E={off:?} не должен включать стенд");
    }
    std::env::remove_var("UWUWU_E2E");
    assert!(!e2e_enabled(), "без гейта стенд выключен");
}

/// Наличие процесса по PID: `kill -0` (POSIX), без новых dev-зависимостей.
fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Гард обязан убить и убрать ОБА процесса. Холостые `sleep` вместо сервера
/// и мока: проверяется только `Drop`, а не спавн/health. Структура `Stand`
/// собрана напрямую — её поля pub, доступ к хелперам спавна не нужен.
#[test]
fn stand_drop_kills_and_reaps_both_children() {
    let dir = TempDirGuard::new("drop");
    let mock = Command::new("sleep")
        .arg("300")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn dummy mock");
    let server = Command::new("sleep")
        .arg("300")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn dummy server");
    let mock_pid = mock.id();
    let server_pid = server.id();
    // Владельцем процессов сразу становится Stand: при панике на ассерте
    // его Drop убьёт детей, а не оставит долгоживущие sleep.
    let stand = Stand {
        dir: dir.0.clone(),
        mock,
        server,
        port: 0,
        mock_port: 0,
    };
    assert!(process_alive(mock_pid), "холостой mock жив до drop");
    assert!(process_alive(server_pid), "холостой server жив до drop");
    drop(stand);
    assert!(
        !process_alive(mock_pid),
        "Drop обязан убить и убрать mock pid {mock_pid}"
    );
    assert!(
        !process_alive(server_pid),
        "Drop обязан убить и убрать server pid {server_pid}"
    );
}

const CRON_SRC: &str = include_str!("cron_stand_e2e.rs");
const QUOTA_SRC: &str = include_str!("quota_limit_pause_stand_e2e.rs");

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

/// Инвентарь gated-сцен, а не проверка их поведения: CI эти тесты не гоняет,
/// поэтому удаление сцены незаметно для `cargo test --workspace`. Поведение
/// и ассерты сцен подтверждает живой прогон и диф-ревью, не этот пин.
#[test]
fn gated_stand_scenes_remain_registered() {
    for (name, src, scenes) in [
        (
            "cron",
            CRON_SRC,
            &[
                "cron_fire_machine_accounting_without_journal",
                "corrupt_state_restart_anchors_without_catchup",
            ][..],
        ),
        (
            "quota",
            QUOTA_SRC,
            &["limit_pause_holds_row_then_releases_when_window_drains"][..],
        ),
    ] {
        for scene in scenes {
            assert!(
                src.contains(scene),
                "{name}: gated-сцена `{scene}` исчезла из исходника — CI её не гоняет, потеря останется незаметной"
            );
        }
    }
}
