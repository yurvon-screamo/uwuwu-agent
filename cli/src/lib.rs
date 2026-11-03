pub mod affine_blob;
pub mod affine_client;
pub mod affine_collect;
pub mod affine_comments;
pub mod affine_docs;
pub mod affine_gql;
pub mod affine_queries;
pub mod affine_search_server;
pub mod affine_snapshot;
pub mod affine_sse;
pub mod affine_store;
pub mod affine_tables;
pub mod affine_tasks;
pub mod affine_titles;
pub mod cmd;
pub mod log_filter;
pub mod path;
pub mod sofa;
pub mod wiki;

// Отдельные тест-файлы (сиблинги модулей): объявление здесь обязательно —
// cargo не подхватывает src/*_tests.rs сам. Однажды потерялись рефакторингом
// и 23 теста молча не бегали (llvm-cov: 0% при живых файлах).
#[cfg(test)]
mod affine_collect_tests;
#[cfg(test)]
mod affine_comments_tests;
#[cfg(test)]
mod affine_sse_tests;
#[cfg(test)]
mod affine_store_tests;
#[cfg(test)]
mod affine_tables_tests;

use std::path::PathBuf;

pub fn wiki_root() -> PathBuf {
    // UWUWU_WIKI — каноническое имя (общее с python-тулзой ~/uwuwu-agent/tools);
    // WIKI_ROOT — легаси-синоним. Оба имеют приоритет над дефолтом данных.
    if let Ok(dir) = std::env::var("UWUWU_WIKI") {
        PathBuf::from(dir)
    } else if let Ok(dir) = std::env::var("WIKI_ROOT") {
        PathBuf::from(dir)
    } else {
        uwuwu_data_dir()
    }
}

pub fn uwuwu_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("UWUWU_DATA_DIR") {
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

/// UI-URL для ссылок/поиска: env, но устаревший Railway-хост игнорируется
/// (старые env опенкод-серверов не должны вредить).
pub fn ui_url() -> String {
    match std::env::var("UWUWU_AFFINE_UI_URL") {
        Ok(u) if !u.contains("railway.app") => u,
        Ok(u) => {
            eprintln!(
                "uwuwu-cli: UWUWU_AFFINE_UI_URL={u} — Railway-хост мёртв, использую https://affine.uwuwu.net"
            );
            "https://affine.uwuwu.net".into()
        }
        Err(_) => "https://affine.uwuwu.net".into(),
    }
}

pub(crate) fn init_tracing() {
    // Own logs default to info, rest to warn. The embedded DB stack (surrealdb/surrealkv)
    // is force-silenced to warn because surrealdb prints INFO on every kvs-store open,
    // polluting CLI output. RUST_LOG overrides the rest; an explicit per-crate target
    // (e.g. RUST_LOG=surrealdb=debug,surrealkv=debug) re-enables it for debugging.
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
