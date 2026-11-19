//! Диагностика окружения памяти: срез A #1294 — THP-режим хоста и
//! AnonHugePages процесса (server.thp); срез B' #1294 — разводка
//! «куча vs cgroup-учёт» (server.memory): Railway-метрика =
//! cgroup memory.current = RSS + page cache + kernel, mmap LanceDB
//! оседает в page cache и засчитывается метрикой, не будучи кучей.

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

/// Разводка памяти процесса и cgroup (срез B' #1294): «кто держит
/// полгига» — куча (RSS) или page cache (mmap-чтение LanceDB).
#[derive(Debug)]
pub struct MemorySnapshot {
    /// RSS процесса, КБ (smaps_rollup Rss).
    pub rss_kb: Option<u64>,
    /// cgroup memory.current, КБ — то, что показывает Railway-метрика.
    pub cgroup_current_kb: Option<u64>,
    /// cgroup memory.stat `file` — page cache, КБ.
    pub cgroup_file_kb: Option<u64>,
    /// cgroup memory.stat `anon` — анонимная память cgroup, КБ.
    pub cgroup_anon_kb: Option<u64>,
}

pub fn memory_snapshot() -> MemorySnapshot {
    MemorySnapshot {
        rss_kb: read_sys("/proc/self/smaps_rollup").and_then(|raw| rollup_field(&raw, "Rss:")),
        cgroup_current_kb: read_sys("/sys/fs/cgroup/memory.current")
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .map(bytes_to_kb),
        cgroup_file_kb: read_sys("/sys/fs/cgroup/memory.stat")
            .and_then(|raw| cgroup_stat_field(&raw, "file")),
        cgroup_anon_kb: read_sys("/sys/fs/cgroup/memory.stat")
            .and_then(|raw| cgroup_stat_field(&raw, "anon")),
    }
}

fn read_sys(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn bytes_to_kb(bytes: u64) -> u64 {
    bytes / 1024
}

/// Активный THP-режим — в квадратных скобках: «always madvise [never]».
fn parse_enabled(raw: &str) -> Option<String> {
    let open = raw.find('[')?;
    let close = raw[open..].find(']')? + open;
    Some(raw[open + 1..close].trim().to_string())
}

/// AnonHugePages из smaps_rollup: строка «AnonHugePages:   2048 kB».
fn parse_rollup(raw: &str) -> Option<u64> {
    rollup_field(raw, "AnonHugePages:")
}

/// Поле smaps_rollup: «<field>  2048 kB» → КБ.
fn rollup_field(raw: &str, field: &str) -> Option<u64> {
    raw.lines().find(|l| l.starts_with(field)).and_then(|l| {
        l.trim_start_matches(field)
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse::<u64>()
            .ok()
    })
}

/// Поле memory.stat cgroup v2 («anon 123456»): значение в байтах → КБ.
/// Точное совпадение имени: `file` не ловит `file_mapped`.
fn cgroup_stat_field(raw: &str, field: &str) -> Option<u64> {
    raw.lines()
        .filter_map(|l| l.split_once(' '))
        .find(|(name, _)| *name == field)
        .and_then(|(_, v)| v.parse::<u64>().ok())
        .map(bytes_to_kb)
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
    fn rollup_garbage_value_is_none_not_panic() {
        // Контракт snapshot(): мусорный вход → None (не паника) — событие
        // server.thp уходит с "n/a", бут сервера не ломается.
        assert_eq!(parse_rollup("AnonHugePages:       abc kB\n"), None);
        assert_eq!(parse_rollup("AnonHugePages: -42 kB\n"), None);
    }

    #[test]
    fn enabled_empty_or_garbage_is_none() {
        assert_eq!(parse_enabled(""), None);
        assert_eq!(parse_enabled("AnonHugePages: 0 kB\n"), None);
    }

    #[test]
    fn rollup_field_reads_rss() {
        let raw = "Rss:              45940 kB\nAnonHugePages:         0 kB\n";
        assert_eq!(rollup_field(raw, "Rss:"), Some(45940));
        assert_eq!(rollup_field("VmRSS: 100 kB\n", "Rss:"), None);
        assert_eq!(rollup_field("Rss: abc kB\n", "Rss:"), None);
    }

    #[test]
    fn cgroup_stat_exact_name_bytes_to_kb() {
        let raw = "anon 251658240\nfile 1048576\nfile_mapped 2048\nshmem 4096\n";
        // точное имя: file не ловит file_mapped
        assert_eq!(cgroup_stat_field(raw, "file"), Some(1024));
        assert_eq!(cgroup_stat_field(raw, "file_mapped"), Some(2));
        assert_eq!(cgroup_stat_field(raw, "anon"), Some(245760));
        assert_eq!(cgroup_stat_field(raw, "sock"), None);
        assert_eq!(cgroup_stat_field("garbage\n", "anon"), None);
        assert_eq!(cgroup_stat_field("anon not-a-number\n", "anon"), None);
    }

    #[test]
    fn memory_snapshot_reads_local_env() {
        // Форма: в контейнере/на хосте procfs доступен; None допустим
        // только при недоступности файлов (не мусорных значениях).
        let snap = memory_snapshot();
        if std::path::Path::new("/proc/self/smaps_rollup").exists() {
            assert!(snap.rss_kb.is_some(), "Rss парсится");
        }
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
