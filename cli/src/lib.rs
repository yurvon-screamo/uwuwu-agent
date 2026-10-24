pub mod affine_blob;
pub mod affine_client;
pub mod affine_collect;
pub mod affine_docs;
pub mod affine_migrate;
pub mod affine_publish;
pub mod affine_queries;
pub mod affine_search;
pub mod affine_search_server;
pub mod affine_sse;
pub mod affine_store;
pub mod affine_tables;
pub mod affine_write;
pub mod content_cache;
pub mod embed;
pub mod fact_index;
pub mod facts;
pub mod log_filter;
pub mod path;
pub mod request;
pub mod sofa;
pub mod wiki;

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};

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

#[derive(Parser)]
#[command(name = "uwuwu-cli")]
#[command(
    about = "CLI for uwuwu/wiki: semantic search over experience, access docs, facts (AFFiNE-backed)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List all projects with their description.
    Projects,
    /// Manage change requests (AFFiNE "Requests" queue). Bare call with
    /// text/--stdin creates a request; subcommands: list/get/delete.
    Request {
        #[command(subcommand)]
        command: Option<request::RequestSub>,
        /// Request body text (quick create; multiline → --stdin)
        #[arg(help = "Request body text (quick create). Note: a text starting with \
                    'list'/'get'/'delete' is parsed as a subcommand — use --stdin.")]
        text: Vec<String>,
        #[arg(long, help = "Read the request body from stdin")]
        stdin: bool,
        #[arg(long, value_parser = ["create", "update", "delete"], default_value = "create")]
        action: String,
        #[arg(
            long,
            help = "Target experience/-relative path (required for quick create)"
        )]
        target: Option<String>,
        #[arg(long, help = "Reason for this request (required for quick create)")]
        reason: Option<String>,
    },
    /// Read a document: <slug> (experience) | facts/<slug> | <project>/<slug> (access) | sofa/<post_id>.
    Read {
        #[arg(
            help = "Slug: 'axum' | 'facts/<slug>' | '<project>/<slug>' (access) | 'sofa/<post_id>'"
        )]
        slug: String,
    },
    /// Download a workspace blob (image/attachment) by key. Keys appear in
    /// read output as affine://blob/<key>.
    Blob {
        #[arg(help = "Blob key, e.g. pixel.png")]
        key: String,
        #[arg(long, help = "Output file ('-' = stdout). Default: <key> in cwd")]
        out: Option<String>,
    },
    /// Screenshot an AFFiNE doc page (incl. edgeless: mindmaps, canvas) to
    /// PNG via headless chromium — the visual view of interactive content.
    Shot {
        #[arg(help = "Doc slug or title (same addressing as read)")]
        slug: String,
        #[arg(long, help = "Output PNG path. Default: shot-<docId>.png in cwd")]
        out: Option<String>,
        #[arg(
            long,
            help = "Open the doc in edgeless mode (mindmaps, canvas) instead of page"
        )]
        edgeless: bool,
    },
    /// Unified semantic search: experience + facts + access docs + SOFA.
    /// AFFiNE is the source; local Ollama embeddings cache per machine.
    Search {
        #[arg(help = "Search query")]
        query: String,
        #[arg(
            long,
            default_value = "5",
            help = "experience/ articles limit (0 = off)"
        )]
        top: usize,
        #[arg(long, default_value = "10", help = "facts/ limit (0 = off)")]
        facts: usize,
        #[arg(long, default_value = "5", help = "projects/** docs limit (0 = off)")]
        access: usize,
        #[arg(long, default_value = "5", help = "SOFA public limit (0 = off)")]
        sofa: usize,
        #[arg(long, default_value = "0.3", help = "min score threshold (0..1)")]
        threshold: f32,
    },
    /// Grep: substring search over the AFFiNE content cache
    /// (experience + project docs + facts).
    Grep {
        #[arg(help = "Literal substring pattern")]
        pattern: String,
    },
    /// Factize raw text: extraction → sanitize → full dedup → AFFiNE daily
    /// buckets (mandatory publish, FS-free). Single engine for manual input
    /// and the python orchestrator.
    Facts {
        /// Read the fact source text from stdin instead of <text...>
        #[arg(long)]
        stdin: bool,
        /// Source label (default: manual/<today>; orchestrator passes opencode/<date>/<sid>)
        #[arg(long)]
        source: Option<String>,
        /// Fact date YYYY-MM-DD (default: today; bucket grouping)
        #[arg(long)]
        date: Option<String>,
        /// Deprecated no-op (the pipeline no longer touches git/FS)
        #[arg(long)]
        sync: bool,
        /// Raw text to factize (ignored with --stdin/--sync)
        text: Vec<String>,
    },
    /// Write a document straight into AFFiNE: markdown from stdin → doc.
    /// `<slug>` (experience) or --project <proj> <slug> (project doc).
    /// Env: UWUWU_AFFINE_URL/TOKEN/WORKSPACE_ID.
    Write {
        #[arg(help = "Document slug (unique across the workspace)")]
        slug: String,
        #[arg(long, help = "Target project (projects/<proj>/…); omit for experience")]
        project: Option<String>,
        #[arg(long, help = "Doc title (defaults to slug)")]
        title: Option<String>,
    },
    /// Force a cache resync from AFFiNE (content cache, dedup index,
    /// embeddings stay lazy). Use on a new machine or after heavy edits.
    Sync,
    /// Migrate wiki data into the AFFiNE workspace: experience/ and projects/
    /// as documents, facts/ as daily journal buckets. Idempotent (resume-safe).
    /// Env: UWUWU_AFFINE_URL (default localhost:3000/mcp), UWUWU_AFFINE_TOKEN,
    /// UWUWU_AFFINE_WORKSPACE_ID.
    Affine {
        #[arg(long, help = "migrate experience/ articles")]
        experience: bool,
        #[arg(long, help = "migrate projects/ docs")]
        projects: bool,
        #[arg(long, help = "migrate facts/ into daily journal buckets")]
        facts: bool,
        #[arg(long, help = "migrate everything")]
        all: bool,
        #[arg(
            long,
            help = "backfill slug: tags for all mapped docs (multi-PC addressing)"
        )]
        tag_slugs: bool,
    },
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
    let cfg = affine_client::AffineConfig::from_env()?;

    match cli.command {
        Commands::Read { slug } => {
            if let Some(post_id) = slug.strip_prefix("sofa/") {
                wiki::sofa_post(post_id).await?
            } else {
                let content = affine_queries::read(&root, &cfg, &slug).await?;
                println!("{content}");
            }
        }
        Commands::Blob { key, out } => {
            let ss = affine_search_server::ServerSearchConfig::from_env()
                .context("нужны UWUWU_AFFINE_UI_URL/_EMAIL/_PASSWORD")?;
            let bytes = affine_blob::download_blob(&ss, &cfg.workspace, &key).await?;
            match out.as_deref() {
                Some("-") => {
                    use std::io::Write;
                    std::io::stdout().write_all(&bytes)?;
                }
                Some(path) => {
                    std::fs::write(path, &bytes)?;
                    println!("{} → {} bytes → {path}", key, bytes.len());
                }
                None => {
                    let name = key.rsplit('/').next().unwrap_or("blob");
                    std::fs::write(name, &bytes)?;
                    println!("{} → {} bytes → {}", key, bytes.len(), name);
                }
            }
        }
        Commands::Shot {
            slug,
            out,
            edgeless,
        } => {
            let ss = affine_search_server::ServerSearchConfig::from_env()
                .context("нужны UWUWU_AFFINE_UI_URL/_EMAIL/_PASSWORD")?;
            let doc_id = affine_queries::resolve_doc_id(&root, &cfg, &slug)
                .await?
                .with_context(|| format!("док '{slug}' не найден"))?;
            let out = out.unwrap_or_else(|| format!("shot-{doc_id}.png"));
            let path = affine_blob::shot_doc(
                &ss,
                &cfg.workspace,
                &doc_id,
                std::path::Path::new(&out),
                edgeless,
            )
            .await?;
            println!("скриншот: {path}");
        }
        Commands::Projects => {
            let (_, cache, _) = affine_queries::session(&root, &cfg).await?;
            let rows = affine_queries::projects(&cache);
            if rows.is_empty() {
                println!("No projects found in the workspace.");
            }
            for row in rows {
                println!("{}\t{} docs\t{}", row.name, row.docs, row.description);
            }
        }
        Commands::Request {
            command,
            text,
            stdin,
            action,
            target,
            reason,
        } => {
            let quick = if command.is_none() && (!text.is_empty() || stdin) {
                let (target, reason) = (target.as_deref(), reason.as_deref());
                if target.is_none() || reason.is_none() {
                    anyhow::bail!(
                        "быстрое создание заявки требует --target <path> и --reason \"...\" \
                         (текст — аргументом или --stdin); очередь: uwuwu-cli request list"
                    );
                }
                Some((text, stdin, action, target.unwrap(), reason.unwrap()))
            } else if command.is_none() && (target.is_some() || reason.is_some()) {
                anyhow::bail!(
                    "указаны --target/--reason без тела заявки: передайте текст аргументом \
                     или --stdin; просмотр очереди — без флагов"
                );
            } else {
                None
            };
            match quick {
                Some((text, stdin, action, target, reason)) => {
                    let body = facts::read_text(stdin, &text)?;
                    let doc_id =
                        affine_queries::create_request(&root, &cfg, &action, target, reason, &body)
                            .await?;
                    println!("request created: {doc_id}");
                }
                None => match command {
                    Some(request::RequestSub::Delete { title }) => {
                        affine_queries::delete_request(&root, &cfg, &title).await?;
                        println!("request deleted: {title}");
                    }
                    other => {
                        let (_, cache, _) = affine_queries::session(&root, &cfg).await?;
                        request::render_queue(other, &cache)?;
                    }
                },
            }
        }
        Commands::Search {
            query,
            top,
            facts,
            access,
            sofa,
            threshold,
        } => {
            if !(0.0..=1.0).contains(&threshold) {
                anyhow::bail!("--threshold должен быть в 0..1, получено: {threshold}");
            }
            println!("Searching for: \"{query}\"\n");
            let sofa_fut = if sofa > 0 {
                Some(sofa::search(&query, sofa))
            } else {
                None
            };
            // Server-side full-text search first (fresh index, no local deps);
            // fall back to local semantic search when unavailable or useless.
            let server_res = async {
                let limit = top.max(access).max(10);
                match affine_search_server::ServerSearchConfig::from_env() {
                    Some(ss) => {
                        match affine_search_server::ServerSearch::connect(&ss, &cfg.workspace).await
                        {
                            Ok(conn) => Some(conn.search(&query, limit).await),
                            Err(e) => {
                                eprintln!(
                                    "affine: серверный поиск недоступен ({e:#}); локальный ниже"
                                );
                                None
                            }
                        }
                    }
                    None => None,
                }
            }
            .await;
            // Map server doc_ids back to slugs via the content cache and drop
            // noisy kinds (daily buckets, requests): keyword hits in fact
            // bodies return date-titled buckets and pollute the output.
            let mut use_local = true;
            if let Some(Ok(hits)) = &server_res {
                // doc_id→slug mapping from the LOCAL cache file only — the
                // server branch must not depend on the MCP gateway. Known
                // limitation: docs created from another machine after the
                // last sync are filtered out as "unknown" until the cache
                // catches up (freshness degrades to the cache age).
                let cache = content_cache::ContentCache::load(&root).unwrap_or_default();
                let by_doc_id: std::collections::BTreeMap<&str, (&str, &str)> = cache
                    .iter()
                    .map(|(slug, doc)| {
                        let kind = match doc.kind {
                            content_cache::DocKind::Experience => "experience",
                            content_cache::DocKind::Project => "access",
                            content_cache::DocKind::Request => "request",
                            content_cache::DocKind::Daily => "fact",
                            content_cache::DocKind::Other => "other",
                        };
                        (doc.doc_id.as_str(), (slug.as_str(), kind))
                    })
                    .collect();
                let mut printed = 0usize;
                for h in hits {
                    let Some((slug, kind)) = by_doc_id.get(h.doc_id.as_str()) else {
                        continue; // daily buckets / requests / unknown — noise
                    };
                    println!("server\t{kind}\t{}\t{}", h.title, slug);
                    if !h.snippet.is_empty() {
                        println!("    {}", h.snippet.replace('\n', " "));
                    }
                    printed += 1;
                }
                if printed > 0 {
                    use_local = false;
                }
            }
            if let Some(Err(e)) = &server_res {
                eprintln!("affine: серверный поиск упал ({e:#}); локальный ниже");
            }
            let local_fut = async {
                if !use_local || top + facts + access == 0 {
                    return Ok(Vec::new());
                }
                let (_, cache, index) = affine_queries::session(&root, &cfg).await?;
                affine_search::search(
                    &root,
                    &cache,
                    &index,
                    &query,
                    &affine_search::SearchOpts {
                        experience: top,
                        facts,
                        access,
                        threshold,
                    },
                )
                .await
            };
            let (local_res, sofa_res) = match sofa_fut {
                Some(fut) => {
                    let (a, c) = tokio::join!(local_fut, fut);
                    (a, Some(c))
                }
                None => (local_fut.await, None),
            };
            let hits = local_res?;
            for hit in &hits {
                let kind = match hit.kind {
                    content_cache::DocKind::Daily => "fact",
                    content_cache::DocKind::Project => "access",
                    _ => "experience",
                };
                println!("{:.3}\t{kind}\t{}\t{}", hit.score, hit.title, hit.slug);
                if kind == "fact" {
                    for line in hit.body.lines() {
                        println!("    {line}");
                    }
                }
            }
            let sofa_hits = sofa_res
                .map(|r| {
                    r.unwrap_or_else(|err| {
                        eprintln!("warning: SOFA недоступен ({err:#}); локальные результаты выше");
                        Vec::new()
                    })
                })
                .unwrap_or_default();
            if !sofa_hits.is_empty() {
                println!("\nPublic (Stack Overflow for Agents):");
                for h in sofa_hits {
                    println!("  [{}] {}", h.content_type, h.title);
                    println!("    {} (id {})", h.url, h.id);
                }
            }
        }
        Commands::Grep { pattern } => {
            let (_, cache, index) = affine_queries::session(&root, &cfg).await?;
            let hits = affine_queries::grep(&cache, &index, &pattern);
            if hits.is_empty() {
                println!("  No matches found.");
                return Ok(());
            }
            let mut last = String::new();
            for hit in &hits {
                if hit.slug != last {
                    println!("\n{}", hit.slug);
                    last = hit.slug.clone();
                }
                println!("  {}: {}", hit.line_number, hit.line);
            }
            println!("\n{} matches", hits.len());
        }
        Commands::Facts {
            stdin,
            source,
            date,
            sync,
            text,
        } => {
            let today = local_today();
            let date = date.unwrap_or_else(|| today.clone());
            if !valid_date(&date) {
                anyhow::bail!("--date должен быть YYYY-MM-DD, получено: '{date}'");
            }
            if sync {
                eprintln!("facts --sync: deprecated no-op (the pipeline is AFFiNE-first)");
            } else {
                let text = facts::read_text(stdin, &text)?;
                let source = source.unwrap_or_else(|| format!("manual/{today}"));
                let out = facts::run_facts(&text, &source, &date, stdin).await;
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
        }
        Commands::Write {
            slug,
            project,
            title,
        } => {
            let markdown = affine_write::read_markdown_stdin()?;
            let title = title.unwrap_or_else(|| slug.clone());
            let doc_id =
                affine_write::run_write(&root, &cfg, &slug, project.as_deref(), &title, &markdown)
                    .await?;
            println!("created: {slug} → {doc_id}");
        }
        Commands::Sync => {
            let mut client = affine_client::AffineClient::connect(&cfg).await?;
            // full refetch: fresh cache replaces the incremental one
            let mut fresh = content_cache::ContentCache::default();
            let report = fresh.sync(&mut client, &cfg, &root).await?;
            let mut index = fact_index::FactIndex::load(&root)?;
            affine_queries::sync_fact_buckets(&mut client, &cfg, &root, &mut index).await?;
            println!(
                "synced: {} docs fetched, {} facts in dedup index",
                report.fetched,
                index.iter().count()
            );
        }
        Commands::Affine {
            experience,
            projects,
            facts,
            all,
            tag_slugs,
        } => {
            if tag_slugs {
                let (tagged, skipped) = affine_docs::backfill_slug_tags(&root, &cfg).await?;
                println!("slug tags: {tagged} set, {skipped} skipped");
                return Ok(());
            }
            let mut kinds = Vec::new();
            if all || experience {
                kinds.push(affine_migrate::Kind::Experience);
            }
            if all || projects {
                kinds.push(affine_migrate::Kind::Projects);
            }
            if all || facts {
                kinds.push(affine_migrate::Kind::Facts);
            }
            if kinds.is_empty() {
                anyhow::bail!("выберите --experience / --projects / --facts или --all");
            }
            let report = affine_migrate::run_migration(&root, &kinds, &cfg).await?;
            println!(
                "affine migrate: created {}, skipped {}, errors {}",
                report.created,
                report.skipped,
                report.errors.len()
            );
            for error in &report.errors {
                eprintln!("  ! {error}");
            }
        }
    }

    Ok(())
}
