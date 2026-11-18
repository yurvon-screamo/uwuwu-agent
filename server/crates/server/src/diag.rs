//! Диагностика окружения памяти (срез A #1294): THP-режим хоста и
//! AnonHugePages процесса. Вердикт THP-пробы — прямое наблюдение в
//! логах (server.thp), а не корреляция падения Railway-метрики.

/// Снимок THP-контекста процесса: режим хоста + huge-страницы процесса.
#[derive(Debug)]
pub struct ThpSnapshot {
    /// Активный режим хоста: содержимое
    /// /sys/kernel/mm/transparent_hugepage/enabled (режим в скобках).
    pub thp: Option<String>,
    /// AnonHugePages процесса, КБ: агрегат из /proc/self/smaps_rollup.
    pub anon_hugepages_kb: Option<u64>,
}

pub fn snapshot() -> ThpSnapshot {
    ThpSnapshot {
        thp: read_sys("/sys/kernel/mm/transparent_hugepage/enabled")
            .and_then(|raw| parse_enabled(&raw)),
        anon_hugepages_kb: read_sys("/proc/self/smaps_rollup").and_then(|raw| parse_rollup(&raw)),
    }
}

fn read_sys(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Активный THP-режим — в квадратных скобках: «always madvise [never]».
fn parse_enabled(raw: &str) -> Option<String> {
    let open = raw.find('[')?;
    let close = raw[open..].find(']')? + open;
    Some(raw[open + 1..close].trim().to_string())
}

/// AnonHugePages из smaps_rollup: строка «AnonHugePages:   2048 kB».
fn parse_rollup(raw: &str) -> Option<u64> {
    raw.lines()
        .find(|l| l.starts_with("AnonHugePages:"))
        .and_then(|l| {
            l.trim_start_matches("AnonHugePages:")
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse::<u64>()
                .ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_active_mode_in_brackets() {
        assert_eq!(
            parse_enabled("always madvise [never]\n"),
            Some("never".to_string())
        );
        assert_eq!(
            parse_enabled("[always] madvise never\n"),
            Some("always".to_string())
        );
        assert_eq!(
            parse_enabled("always [madvise] never\n"),
            Some("madvise".to_string())
        );
        assert_eq!(parse_enabled("always madvise never\n"), None);
    }

    #[test]
    fn rollup_anon_hugepages_kb() {
        let raw = "Rss:              123456 kB\nAnonHugePages:      2048 kB\nSwap:                  0 kB\n";
        assert_eq!(parse_rollup(raw), Some(2048));
        assert_eq!(parse_rollup("Rss: 1 kB\n"), None);
        assert_eq!(parse_rollup("AnonHugePages:       0 kB\n"), Some(0));
    }

    #[test]
    fn snapshot_reads_host_env() {
        // На хосте/в контейнере файлы ядра доступны; проверяем форму, не значение
        let snap = snapshot();
        if std::path::Path::new("/sys/kernel/mm/transparent_hugepage/enabled").exists() {
            assert!(snap.thp.is_some(), "enabled читается — режим парсится");
        }
    }
}
