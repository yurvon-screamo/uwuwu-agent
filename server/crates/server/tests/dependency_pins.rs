//! Тесты RUSTSEC-2026-0592 (задача #1509) для транзитивного `url`:
//! - офлайн-пин читает закоммиченный `server/Cargo.lock` и требует, чтобы
//!   КАЖДАЯ копия `url` была >= 2.5.9 (численно, пре-релиз ниже релиза);
//! - живой приёмочный тест под `UWUWU_E2E=1` гоняет `cargo audit --file`
//!   и требует отсутствия RUSTSEC-2026-0592 в выдаче.
//!
//! На 2.5.8 `Url::origin()` переполняет стек на вложенных `blob:` URL.
//! Офлайн-пин детерминирован и не требует базы адвизориев: он ловит откат
//! версии, но не сдвиг самого адвизори. Живой гейт держат ручной прогон
//! и ночной аудит из постановки; в `ci.yml` шага `cargo audit` НЕТ
//! (там fmt, clippy, test, qlty).
//!
//! Запуск:
//!   cargo test -p uwuwu-server --test dependency_pins
//!   UWUWU_E2E=1 cargo test -p uwuwu-server --test dependency_pins -- --nocapture

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const ADVISORY: &str = "RUSTSEC-2026-0592";
const MIN_VERSION: (u64, u64, u64) = (2, 5, 9);
const AUDIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Путь к workspace-lockfile: CARGO_MANIFEST_DIR = server/crates/server.
fn lockfile_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("Cargo.lock")
}

/// Первое поле `key = "..."` в блоке записи lockfile.
fn field(block: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = \"");
    block.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix('"'))
            .map(str::to_owned)
    })
}

/// Все версии пакета в lockfile: дубли должны быть пропатчены все.
fn locked_versions(package: &str) -> Vec<String> {
    let lock = lockfile_path();
    let text = std::fs::read_to_string(&lock)
        .unwrap_or_else(|e| panic!("не прочитан {}: {e}", lock.display()));
    let versions: Vec<String> = text
        .split("[[package]]")
        .filter(|block| field(block, "name").as_deref() == Some(package))
        .map(|block| {
            field(block, "version").unwrap_or_else(|| {
                panic!("у пакета {package} нет поля version в {}", lock.display())
            })
        })
        .collect();
    assert!(
        !versions.is_empty(),
        "пакет {package} не найден в {}",
        lock.display()
    );
    versions
}

/// Semver-порядок относительно версии-релиза: build-метаданные (`+…`) не
/// влияют на порядок, пре-релиз (`-rc.1`) ниже того же `major.minor.patch`.
fn version_at_least(version: &str, min: (u64, u64, u64)) -> bool {
    let without_build = version.split('+').next().unwrap_or(version);
    let (core, prerelease) = match without_build.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (without_build, None),
    };
    let mut parts = core.split('.');
    let take = |n: &str| -> u64 {
        let digits: String = n.chars().take_while(char::is_ascii_digit).collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("неверная версия: {version}"))
    };
    let actual = (
        take(parts.next().unwrap_or_default()),
        take(parts.next().unwrap_or_default()),
        take(parts.next().unwrap_or_default()),
    );
    if actual == min {
        prerelease.is_none()
    } else {
        actual > min
    }
}

#[test]
fn url_is_not_vulnerable_to_rustsec_2026_0592() {
    for version in locked_versions("url") {
        assert!(
            version_at_least(&version, MIN_VERSION),
            "url {version} в server/Cargo.lock уязвим ({ADVISORY}); нужен >= 2.5.9"
        );
    }
}

/// Команда с таймаутом: exit≠0 не ошибка — `cargo audit` возвращает 1, когда
/// нашёл уязвимости, и кладёт полный отчёт в stdout. Оба потока читаются
/// параллельно: большой JSON иначе забивает pipe и child висит до таймаута.
fn run_with_timeout(cmd: &mut Command) -> std::process::Output {
    use std::io::Read;
    use std::sync::mpsc;

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("не удалось запустить cargo audit: {e}"));
    let mut stdout_pipe = child.stdout.take().expect("stdout pipe");
    let mut stderr_pipe = child.stderr.take().expect("stderr pipe");
    let (tx_out, rx_out) = mpsc::channel();
    let (tx_err, rx_err) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        let _ = tx_out.send(buf);
    });
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        let _ = tx_err.send(buf);
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= AUDIT_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("cargo audit превысил таймаут {AUDIT_TIMEOUT:?}");
                }
                thread::sleep(Duration::from_millis(200));
            }
            Err(e) => panic!("cargo audit: {e}"),
        }
    };
    std::process::Output {
        status,
        stdout: rx_out.recv().unwrap_or_default(),
        stderr: rx_err.recv().unwrap_or_default(),
    }
}

#[test]
fn live_cargo_audit_has_no_rustsec_2026_0592() {
    if std::env::var("UWUWU_E2E").ok().as_deref() != Some("1") {
        println!("e2e выключен: UWUWU_E2E=1 для живого cargo audit");
        return;
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    // Честный пропуск без инструмента, а не «аудит зелёный».
    if !Command::new(&cargo)
        .args(["audit", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        println!("cargo-audit не установлен — живой аудит пропущен");
        return;
    }

    // Без --no-fetch: база адвизориев обновляется, кэш не должен протухнуть.
    let mut cmd = Command::new(&cargo);
    cmd.args([
        "audit",
        "--file",
        lockfile_path().to_str().expect("путь lockfile — UTF-8"),
        "--json",
    ]);
    let output = run_with_timeout(&mut cmd);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "cargo audit не выдал JSON ({e}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let list = report["vulnerabilities"]["list"]
        .as_array()
        .unwrap_or_else(|| panic!("в выдаче cargo audit нет vulnerabilities.list: {stdout}"));
    let hits: Vec<&Value> = list
        .iter()
        .filter(|v| v["advisory"]["id"].as_str() == Some(ADVISORY))
        .collect();
    assert!(
        hits.is_empty(),
        "живой cargo audit нашёл {ADVISORY}: {hits:?}"
    );
}
