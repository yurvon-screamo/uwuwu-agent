//! Flock-лок общего клон-каталога (задача 1062): сериализация sync/write
//! между параллельными cli-процессами. flock освобождается ОС при смерти
//! процесса — стейл-локи исключены (файл-флаг с O_EXCL ими болеет).

use anyhow::{bail, Context, Result};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Потолок ожидания лока: fetch hub-репо с холодной сетью — секунды,
/// write-path (commit+push+rebase-retry) — до десятков секунд.
const LOCK_WAIT_BUDGET: Duration = Duration::from_secs(30);
const BACKOFF_START: Duration = Duration::from_millis(25);
const BACKOFF_CAP: Duration = Duration::from_millis(500);

/// Держатель эксклюзивного лока; освобождение — в Drop.
pub(super) struct CloneLock {
    _file: std::fs::File,
}

/// Взять лок `<clone>/.git/uwuwu-cli.lock`: спин на try_lock с экспоненциальным
/// backoff; исчерпание бюджета — громкий отказ с путём лока.
pub(super) async fn clone_lock(clone_dir: &str) -> Result<CloneLock> {
    let path: PathBuf = Path::new(clone_dir).join(".git").join("uwuwu-cli.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("лок-файл {} не открывается", path.display()))?;
    let deadline = Instant::now() + LOCK_WAIT_BUDGET;
    let mut backoff = BACKOFF_START;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(CloneLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    bail!(
                        "клон занят другим процессом: лок {} держится дольше 30с — \
                         разбор вручную (возможен подвисший git-процесс)",
                        path.display()
                    );
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_CAP);
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(e).context(format!("лок {} не берётся", path.display()))
            }
        }
    }
}
