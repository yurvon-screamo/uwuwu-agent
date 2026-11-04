//! uwuwu-cli: доступ агентам к платформе uwuwu (SiYuan-бекенд).
//!
//! История: до 04.11.2026 платформа была закрыта (модули-предшественник удалён),
//!мигрировано на SiYuan — старый бекенд удалён (спайк 8INRfGpceo,
//! PR #30/#31 в yurvon-screamo/uwuwu-agent).

pub mod cmd;
pub mod log_filter;
pub mod md_heading;
pub mod path;
pub mod siyuan;
pub mod siyuan_id;
pub mod sofa;
pub mod siyuan_task;

#[cfg(test)]
mod siyuan_task_tests;

use std::path::PathBuf;

pub fn wiki_root() -> PathBuf {
    if let Ok(dir) = std::env::var("UWUWU_WIKI_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("UWUWU_WIKI") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .expect("USERPROFILE or HOME environment variable must be set");
    PathBuf::from(home).join(".uwuwu")
}

pub fn uwuwu_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("UWUWU_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .expect("USERPROFILE or HOME environment variable must be set");
    PathBuf::from(home).join(".local/share/uwuwu")
}

/// Публичный URL платформы (ссылки в выдаче).
pub fn ui_url() -> String {
    std::env::var("UWUWU_SIYUAN_PUBLIC_URL")
        .unwrap_or_else(|_| "https://siyuan-relay-production.up.railway.app".into())
}

pub(crate) fn init_tracing() {
    let base = std::env::var("RUST_LOG").unwrap_or_else(|_| log_filter::DEFAULT_BASE.to_string());
    let filter = log_filter::build(&base);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Entry point for the binary crate: parse args and dispatch (see `cmd`).
pub async fn run() -> anyhow::Result<()> {
    cmd::run().await
}
