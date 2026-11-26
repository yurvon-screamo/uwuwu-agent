//! Регрессионный пин RUSTSEC-2026-0592 (задача #1509): транзитивный
//! `url` в `server/Cargo.lock` обязан быть >= 2.5.9. На 2.5.8
//! `Url::origin()` переполняет стек на вложенных `blob:` URL, и
//! `cargo audit` падает с RUSTSEC-2026-0592.
//!
//! Тест офлайн и детерминирован: читает закоммиченный lockfile, а не живой
//! advisory-бэкенд. Живой гейт — `cargo audit` (прогоняется руками и в CI);
//! здесь фиксируется именно регрессия отката версии. Пин не ломается при
//! последующих патч-апах выше 2.5.9 — сравнение численное, не строковое.
//!
//! Запуск: cargo test -p uwuwu-server --test dependency_pins

use std::path::{Path, PathBuf};

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

/// Версия пакета, зафиксированная в lockfile.
fn locked_version(package: &str) -> String {
    let lock = lockfile_path();
    let text = std::fs::read_to_string(&lock)
        .unwrap_or_else(|e| panic!("не прочитан {}: {e}", lock.display()));
    for block in text.split("[[package]]") {
        if field(block, "name").as_deref() == Some(package) {
            return field(block, "version").unwrap_or_else(|| {
                panic!("у пакета {package} нет поля version в {}", lock.display())
            });
        }
    }
    panic!("пакет {package} не найден в {}", lock.display());
}

/// Численное сравнение `major.minor.patch`; хвост `-pre`/`+build` отбрасываем.
fn version_triplet(version: &str) -> (u64, u64, u64) {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = core.split('.');
    let take = |n: &str| -> u64 {
        let digits: String = n.chars().take_while(char::is_ascii_digit).collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("неверная версия: {version}"))
    };
    let major = take(parts.next().unwrap_or_default());
    let minor = take(parts.next().unwrap_or_default());
    let patch = take(parts.next().unwrap_or_default());
    (major, minor, patch)
}

#[test]
fn url_is_not_vulnerable_to_rustsec_2026_0592() {
    let version = locked_version("url");
    assert!(
        version_triplet(&version) >= (2, 5, 9),
        "url {version} в server/Cargo.lock уязвим (RUSTSEC-2026-0592); нужен >= 2.5.9"
    );
}
