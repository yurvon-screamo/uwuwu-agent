pub mod access;
pub mod cache;
pub mod chunk;
pub mod embed;
pub mod facts;
pub mod file_io;
pub mod get;
pub mod grep;
pub mod log_filter;
pub mod path;
pub mod projects;
pub mod request;
pub mod request_util;
pub mod search;
pub mod sofa;
pub mod time_util;
pub mod wiki;

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
    about = "CLI for uwuwu/wiki: semantic search over experience, access docs, projects listing"
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
    /// Unified search: experience + facts + access docs + SOFA in one call.
    /// Per-source flags are LIMITS from one shared ranked pool (sections can
    /// crowd each other out at low thresholds), not guaranteed quotas.
    Search {
        #[arg(help = "Search query")]
        query: String,
        #[arg(long, default_value = "5", help = "experience/ articles limit (0 = off)")]
        top: usize,
        #[arg(long, default_value = "10", help = "facts/ limit (0 = off)")]
        facts: usize,
        #[arg(long, default_value = "5", help = "projects/*/access limit (0 = off)")]
        access: usize,
        #[arg(long, default_value = "5", help = "SOFA public limit (0 = off)")]
        sofa: usize,
        #[arg(long, default_value = "0.3", help = "min score threshold (0..1)")]
        threshold: f32,
    },
    /// Grep: substring search over experience/ + facts/ + access docs.
    Grep {
        #[arg(help = "Literal substring pattern")]
        pattern: String,
        #[arg(long, help = "skip facts/")]
        no_facts: bool,
        #[arg(long, help = "skip projects/*/access")]
        no_access: bool,
    },
    /// Factize raw text: extraction → sanitize → full dedup → facts/ + git.
    /// Single engine for manual input and the python orchestrator.
    /// Trivial-threshold (1000 chars) applies ONLY to --stdin (session digests);
    /// a manual text argument is intentional input and skips the threshold.
    Facts {
        /// Read the fact source text from stdin instead of <text...>
        #[arg(long)]
        stdin: bool,
        /// Source label (default: manual/<today>); orchestrator passes opencode/<date>/<sid>
        #[arg(long)]
        source: Option<String>,
        /// Fact date YYYY-MM-DD (default: today; sets facts/YYYY-MM dir + updated)
        #[arg(long)]
        date: Option<String>,
        /// Only flush a scoped git commit for facts/sidecar files (no LLM)
        #[arg(long)]
        sync: bool,
        /// Raw text to factize (ignored with --stdin/--sync)
        text: Vec<String>,
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

/// Локальная дата дня (как в audit/python: локальные даты, не UTC).
fn local_today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// YYYY-MM-DD или паника слайсинга месяца каталога — валидируем на входе.
fn valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 {
        return false;
    }
    if !b.iter().enumerate().all(|(i, c)| match i {
        4 | 7 => *c == b'-',
        _ => c.is_ascii_digit(),
    }) {
        return false;
    }
    let num = |from: usize, to: usize| -> u32 { s[from..to].parse().unwrap_or(0) };
    (1..=12).contains(&num(5, 7)) && (1..=31).contains(&num(8, 10))
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
        Commands::Access { command } => access::handle(command, &root, &cache_path()).await?,
        Commands::Request { command } => request::handle(command, &requests_dir())?,
        Commands::Search { query, top, facts, access, sofa, threshold } => {
            if !(0.0..=1.0).contains(&threshold) {                anyhow::bail!("--threshold должен быть в 0..1, получено: {threshold}");
            }
            let opts = wiki::UnifiedOpts {
                top, facts, access, sofa, threshold,
            };
            wiki::unified_search(&query, &opts, &root, &cache_path()).await?
        }
        Commands::Grep { pattern, no_facts, no_access } => {
            wiki::unified_grep(&pattern, !no_facts, !no_access, &root)?
        }
        Commands::Facts { stdin, source, date, sync, text } => {
            let today = local_today();
            let date = date.unwrap_or_else(|| today.clone());
            if !valid_date(&date) {
                anyhow::bail!("--date должен быть YYYY-MM-DD, получено: '{date}'");
            }
            if sync {
                facts::sync_git("facts: sidecar sync")?;
            } else {
                let text = facts::read_text(stdin, &text)?;
                let source = source.unwrap_or_else(|| format!("manual/{today}"));
                let out = facts::run_facts(&text, &source, &date, stdin).await;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        }
    }

    Ok(())
}
