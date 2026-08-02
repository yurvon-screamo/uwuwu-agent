pub mod access;
pub mod cache;
pub mod chunk;
pub mod embed;
pub mod enrich;
pub mod file_io;
pub mod get;
pub mod grep;
pub mod log_filter;
pub mod path;
pub mod projects;
pub mod request;
pub mod request_util;
pub mod search;
pub mod task;
pub mod task_asset;
pub mod task_clone;
pub mod task_create;
pub mod task_edit;
pub mod task_get;
pub mod task_grep;
pub mod task_list;
pub mod task_list_render;
pub mod task_query;
pub mod task_search;
pub mod task_set_status;
pub mod time_util;
pub mod wiki;
pub mod worklog;

use std::path::PathBuf;

use clap::{Parser, Subcommand};


pub fn wiki_root() -> PathBuf {
    if let Ok(dir) = std::env::var("WIKI_ROOT") {
        PathBuf::from(dir)
    } else {
        uwuwu_data_dir()
    }
}

#[derive(Parser)]
#[command(name = "uwuwu-cli")]
#[command(
    about = "CLI for uwuwu/wiki: semantic search over experience, task tracker, access docs, projects listing"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Semantic search/grep/get over experience/ (NOT access — use `access` subgroup).
    Wiki {
        #[command(subcommand)]
        command: wiki::WikiSub,
    },
    /// List all projects with their README content.
    Projects,
    /// Personal task tracker (project-scoped).
    Task {
        #[command(subcommand)]
        command: task::TaskSub,
    },
    /// Access docs per project (credentials, stands, topology).
    Access {
        #[command(subcommand)]
        command: access::AccessSub,
    },
    /// Manage change requests in .requests/ (create/list/get/delete).
    Request {
        #[command(subcommand)]
        command: request::RequestSub,
    },
    /// Enrich markdown files with title + description via a local Ollama model.
    Enrich {
        #[arg(help = "File or directory to enrich")]
        path: String,
        #[arg(long, help = "Preview without writing")]
        dry_run: bool,
    },
}

pub fn uwuwu_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("UWUWU_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .expect("USERPROFILE or HOME environment variable must be set");
    PathBuf::from(home).join(".uwuwu")
}

pub fn cache_path() -> PathBuf {
    uwuwu_data_dir().join("embeddings.db")
}

pub fn requests_dir() -> PathBuf {
    wiki_root().join(".requests")
}

fn init_tracing() {
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

pub async fn run() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let root = wiki_root();

    match cli.command {
        Commands::Wiki { command } => wiki::handle(command, &root, &cache_path()).await?,
        Commands::Projects => projects::handle_projects(&root)?,
        Commands::Task { command } => task::handle(command, &root, &cache_path()).await?,
        Commands::Access { command } => access::handle(command, &root, &cache_path()).await?,
        Commands::Request { command } => request::handle(command, &requests_dir())?,
        Commands::Enrich { path, dry_run } => {
            enrich::run(&PathBuf::from(path), dry_run).await?;
        }
    }

    Ok(())
}
