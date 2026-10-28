pub mod affine_blob;
pub mod affine_client;
pub mod affine_collect;
pub mod affine_docs;
pub mod affine_publish;
pub mod affine_queries;
pub mod affine_search;
pub mod affine_search_server;
pub mod affine_sse;
pub mod affine_store;
pub mod affine_tables;
pub mod affine_tasks;
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
use serde_json::json;

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

/// Сабкоманды uwuwu-cli task.
#[derive(clap::Subcommand)]
enum TaskSub {
    /// Создать задачу: док-тело (тег «Task») + строка Todo
    Create {
        #[arg(long, help = "Заголовок задачи")]
        title: String,
        #[arg(long, help = "Тип: dev | spike | bug (тег + шаблон тела)")]
        r#type: Option<String>,
        #[arg(long, help = "Текст тела из stdin (опционально)")]
        stdin: bool,
        #[arg(
            long = "attach",
            value_name = "FILE",
            help = "Аттач в тело (repeatable)"
        )]
        attach: Vec<std::path::PathBuf>,
    },
    /// Очередь задач доски
    List {
        #[arg(long, help = "Фильтр по статусу (todo|design|in progress|…)")]
        status: Option<String>,
        #[arg(long, help = "Проект (переопределяет --project задачи)")]
        project: Option<String>,
    },
    /// Карточка задачи: колонки + свежие комменты
    Get {
        #[arg(help = "row-id / doc-id / уникальный префикс")]
        id: String,
    },
    /// Правка колонок строки
    Edit {
        #[arg(help = "row-id / doc-id / префикс")]
        id: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        pr: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Коммент в док задачи (диалог юзер ↔ агент)
    Comment {
        #[arg(help = "row-id / doc-id / префикс")]
        id: String,
        #[arg(long, help = "Текст из stdin")]
        stdin: bool,
        #[arg(trailing_var_arg = true, help = "Текст коммента")]
        text: Vec<String>,
    },
    /// Закрыть задачу (Done) с опциональной заметкой
    Close {
        #[arg(help = "row-id / doc-id / префикс")]
        id: String,
        #[arg(long, help = "Заметка-коммент при закрытии")]
        note: Option<String>,
    },
    /// Дописать markdown в тело задачи/крона (stdin или --md файл).
    /// Фенсы ```html_embed [имя] конвертируются в живые embed_html-диаграммы:
    /// именованный фенс заменяет прежнюю диаграмму с этим именем,
    /// пустой (```html_embed имя без тела) — удаляет её.
    Body {
        #[arg(help = "row-id / doc-id / префикс / подстрока заголовка")]
        id: String,
        #[arg(long, help = "Файл с markdown (иначе stdin)")]
        md: Option<String>,
        /// Заменить тело ЦЕЛИКОМ (агент читает task get, правит, шлёт новое).
        /// Rich-контент (embed-диаграммы) не переживает — пересоздай фенсами.
        #[arg(long)]
        replace: bool,
    },
}

/// Кроны на доске Crons: агентские отложенные/периодические работы.
/// Дефолтный механизм: сессионный cron_set умирает с сессией — эти живут.
#[derive(clap::Subcommand)]
enum CronSub {
    /// Поставить крон (тело = промпт из stdin или заглушка).
    /// --project <proj> → доска «Кроны <proj>» в папке проекта,
    /// без --project → глобальная доска «Crons».
    Set {
        #[arg(long, help = "Заголовок крона")]
        title: String,
        #[arg(long, value_name = "10m|90s|2h", help = "Разово через N (one-shot)")]
        r#in: Option<String>,
        #[arg(long, value_name = "5m|2h|1d", help = "Повторять каждые N")]
        every: Option<String>,
        #[arg(long, value_name = "09:30 [01.12]", help = "Разово в указанное время")]
        at: Option<String>,
        #[arg(long, value_name = "EXPR", help = "Cron-выражение (5 полей)")]
        cron_expr: Option<String>,
        #[arg(long, help = "Проект (доска «Кроны <proj>» в папке проекта)")]
        project: Option<String>,
        #[arg(long, help = "Промпт из stdin (иначе заглушка)")]
        stdin: bool,
    },
    /// Все кроны доски
    List,
    /// Остановить (Status=paused)
    Pause { id: String },
    /// Запустить обратно (Status=active)
    Resume { id: String },
    /// Удалить крон (строка + тело в трэш)
    Rm { id: String },
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
        #[arg(
            long = "attach",
            value_name = "FILE",
            help = "Attach a file/image to the created request (repeatable)"
        )]
        attach: Vec<PathBuf>,
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
        #[arg(long, default_value = "10", help = "task/cron bodies limit (0 = off)")]
        tasks: usize,
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
        /// Raw text to factize (ignored with --stdin)
        text: Vec<String>,
    },
    /// Crons on the AFFiNE «Crons» board (durable agent schedules).
    Cron {
        #[command(subcommand)]
        command: CronSub,
    },
    /// Kanban tasks on AFFiNE («Задачи <Proj>» boards): create/list/get/
    /// edit/comment/close. Body doc gets the type tag; comments are
    /// native AFFiNE comments on the body.
    Task {
        #[command(subcommand)]
        command: TaskSub,
        /// Project board (обязательно: uwuwu | origa | foilcap | …)
        #[arg(long, global = true)]
        project: Option<String>,
    },
    /// Refresh local caches from AFFiNE (content cache, dedup index,
    /// embeddings stay lazy). Use on a new machine or after heavy edits.
    Refresh,
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
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

/// Резолв строки по id/заголовку: сперва доска проекта, затем доска Crons
/// (крон-агенты комментят свои тела через тот же task comment).
pub async fn resolve_row_any_board(
    client: &mut affine_client::AffineClient,
    cfg: &affine_client::AffineConfig,
    root: &std::path::Path,
    project: &str,
    id: &str,
) -> anyhow::Result<(affine_tasks::Board, affine_tasks::TaskRow)> {
    let board = affine_tasks::find_board(client, cfg, project).await?;
    let rows = affine_tasks::read_rows(client, cfg, &board, root).await?;
    if let Ok(row) = affine_tasks::resolve_row(&rows, id) {
        return Ok((board, row.clone()));
    }
    // фоллбек: крон-доски (Crons + «Кроны <proj>» всех проектов)
    for bt in affine_tasks::cron_board_titles(client, cfg).await? {
        if bt == "Crons" {
            continue;
        }
        let Ok(b) = affine_tasks::find_board_by_title(client, cfg, &bt).await else {
            continue;
        };
        let Ok(rows) = affine_tasks::read_rows(client, cfg, &b, root).await else {
            continue;
        };
        if let Ok(row) = affine_tasks::resolve_row(&rows, id) {
            return Ok((b, row.clone()));
        }
    }
    let cron_board = affine_tasks::find_board_by_title(client, cfg, "Crons").await?;
    let cron_rows = affine_tasks::read_rows(client, cfg, &cron_board, root).await?;
    let row = affine_tasks::resolve_row(&cron_rows, id)
        .map_err(|e| anyhow::anyhow!("{e} (искали на доске проекта и всех крон-досках)"))?;
    Ok((cron_board, row.clone()))
}

/// Найти крон на любой крон-доске (Crons + «Кроны <proj>» проектов).
pub async fn resolve_cron_any_board(
    client: &mut affine_client::AffineClient,
    cfg: &affine_client::AffineConfig,
    board_titles: &[String],
    root: &std::path::Path,
    id: &str,
) -> anyhow::Result<(affine_tasks::Board, affine_tasks::TaskRow)> {
    let mut last_err = String::new();
    for bt in board_titles {
        let Ok(board) = affine_tasks::find_board_by_title(client, cfg, bt).await else {
            continue;
        };
        let Ok(rows) = affine_tasks::read_rows(client, cfg, &board, root).await else {
            continue;
        };
        match affine_tasks::resolve_row(&rows, id) {
            Ok(row) => return Ok((board, row.clone())),
            Err(e) => last_err = format!("{e}"),
        }
    }
    anyhow::bail!("крон «{id}» не найден на крон-досках ({last_err})")
}

/// Выгрузить картинки тела (`affine://blob/<key>` в markdown) в
/// /tmp/uwuwu-task-<row>/ — агент получает локальные пути одним get.
/// UI-кредов нет / шлюз лежит → молча остаёмся на ссылках.
async fn save_body_images(md: &str, row_id: &str) -> Vec<String> {
    const MAX: usize = 10;
    let mut out = Vec::new();
    // собрать уникальные ключи в порядке появления
    let mut keys: Vec<String> = Vec::new();
    for cap in regex::Regex::new(r"affine://blob/([A-Za-z0-9._%~:+-]+)")
        .unwrap()
        .captures_iter(md)
    {
        let raw = cap.get(1).unwrap().as_str().to_string();
        // ключ в URL percent-encoded — декодируем %XX
        let key = percent_decode(&raw);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if keys.is_empty() {
        return out;
    }
    let Some(ss) = affine_search_server::ServerSearchConfig::from_env() else {
        return out;
    };
    let Ok(ws_cfg) = affine_client::AffineConfig::from_env() else {
        return out;
    };
    let dir = format!(
        "/tmp/uwuwu-task-{}",
        row_id
            .trim_start_matches('-')
            .chars()
            .take(8)
            .collect::<String>()
    );
    if std::fs::create_dir_all(&dir).is_err() {
        return out;
    }
    for (i, key) in keys.iter().take(MAX).enumerate() {
        match affine_blob::download_blob(&ss, &ws_cfg.workspace, key).await {
            Ok(bytes) if !bytes.is_empty() => {
                let ext = sniff_ext(&bytes);
                let path = format!("{dir}/img-{}.{ext}", i + 1);
                if std::fs::write(&path, &bytes).is_ok() {
                    out.push(path);
                }
            }
            _ => {}
        }
    }
    out
}

/// Простой %XX-декодер (ключи блобов приезжают URL-encoded).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Расширение по magic bytes (png/jpg/gif/webp, иначе bin).
fn sniff_ext(b: &[u8]) -> &'static str {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if b.starts_with(b"GIF8") {
        "gif"
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "webp"
    } else {
        "bin"
    }
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
                    if unsafe { libc::isatty(1) } == 1 {
                        eprintln!("warning: бинарные данные ({}) в терминал; перенаправь в файл: uwuwu-cli blob {key} --out файл", bytes.len());
                    }
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
            attach,
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
                    if !attach.is_empty() {
                        affine_blob::attach_files(&cfg, &doc_id, &attach).await?;
                    }
                }
                None => match command {
                    Some(request::RequestSub::Delete { title }) => {
                        affine_queries::delete_request(&root, &cfg, &title).await?;
                        println!("request deleted: {title}");
                    }
                    other => {
                        let (mut client, cache, _) = affine_queries::session(&root, &cfg).await?;
                        request::render_queue(&mut client, &cfg, other, &cache).await?;
                    }
                },
            }
        }
        Commands::Search {
            query,
            top,
            tasks,
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
                let by_doc_id: std::collections::BTreeMap<&str, (&str, &str, &str)> = cache
                    .iter()
                    .map(|(slug, doc)| {
                        let kind = match doc.kind {
                            content_cache::DocKind::Experience => "experience",
                            content_cache::DocKind::Project => "access",
                            content_cache::DocKind::Request => "request",
                            content_cache::DocKind::Daily => "fact",
                            content_cache::DocKind::Task => "task",
                            content_cache::DocKind::Cron => "cron",
                            content_cache::DocKind::Other => "other",
                        };
                        (
                            doc.doc_id.as_str(),
                            (slug.as_str(), kind, doc.title.as_str()),
                        )
                    })
                    .collect();
                let mut printed = 0usize;
                for h in hits {
                    let Some((slug, kind, title)) = by_doc_id.get(h.doc_id.as_str()) else {
                        continue; // daily buckets / requests / unknown — noise
                    };
                    let title = if h.title.is_empty() {
                        *title
                    } else {
                        h.title.as_str()
                    };
                    println!("server\t{kind}\t{title}\t{slug}");
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
                // тела задач/кронов — кэш с TTL (живой проход не чаще 5 мин);
                // шлюз недоступен → деградация на кэш, поиск не падает
                let extra = match affine_client::AffineClient::connect(&cfg).await {
                    Ok(mut client) => {
                        affine_queries::tagged_docs_cached(&mut client, &cfg, &root).await
                    }
                    Err(e) => {
                        eprintln!("affine: шлюз недоступен ({e:#}) — семантика задач из кэша");
                        affine_queries::tagged_docs_cache_load(&root)
                    }
                };
                eprintln!("affine: тел задач/кронов в семантике: {}", extra.len());
                // search НЕ гонит синки (list_workspace_tree на 946 доках
                // упирается в лимит шлюза и сжирает минуту): локальные кэши
                // + tagged-кэш TTL 5 мин. Свежесть дают остальные cli-команды.
                let cache = content_cache::ContentCache::load(&root).unwrap_or_default();
                let index = crate::fact_index::FactIndex::load(&root).unwrap_or_default();
                affine_search::search(
                    &root,
                    &cache,
                    &index,
                    &query,
                    &affine_search::SearchOpts {
                        experience: top,
                        facts,
                        access,
                        tasks,
                        threshold,
                    },
                    &extra,
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
                    content_cache::DocKind::Task => "task",
                    content_cache::DocKind::Cron => "cron",
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
            text,
        } => {
            let today = local_today();
            let date = date.unwrap_or_else(|| today.clone());
            if !valid_date(&date) {
                anyhow::bail!("--date должен быть YYYY-MM-DD, получено: '{date}'");
            }
            let text = facts::read_text(stdin, &text)?;
            let source = source.unwrap_or_else(|| format!("manual/{today}"));
            let out = facts::run_facts(&text, &source, &date, stdin).await;
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        Commands::Cron { ref command } => {
            let mut client = affine_client::AffineClient::connect(&cfg).await?;
            // крон-доски: глобальная «Crons» + «Кроны <proj>» по папкам Projects
            let cron_boards = affine_tasks::cron_board_titles(&mut client, &cfg).await?;
            let command_ref = command;
            match command_ref {
                CronSub::Set {
                    title,
                    r#in,
                    every,
                    at,
                    cron_expr,
                    project,
                    stdin,
                } => {
                    let board = match project.as_deref() {
                        Some(proj) => {
                            let bt = format!("Кроны {proj}");
                            affine_tasks::find_board_by_title(&mut client, &cfg, &bt)
                                .await
                                .with_context(|| {
                                    format!("доска «{bt}» не найдена — создай док-таблицу в папке проекта «{proj}»")
                                })?
                        }
                        None => affine_tasks::find_board_by_title(&mut client, &cfg, "Crons")
                            .await
                            .context("доска «Crons» не найдена")?,
                    };
                    let specs = [
                        r#in.as_ref(),
                        every.as_ref(),
                        at.as_ref(),
                        cron_expr.as_ref(),
                    ];
                    let n = specs.iter().flatten().count();
                    if n != 1 {
                        anyhow::bail!("ровно один из --in/--every/--at/--cron (передано {n})");
                    }
                    let schedule = if let Some(off) = &r#in {
                        affine_tasks::one_shot_schedule(affine_tasks::parse_offset_minutes(off)?)
                    } else if let Some(e) = &every {
                        e.clone()
                    } else if let Some(a) = &at {
                        a.clone()
                    } else {
                        cron_expr.clone().unwrap_or_default()
                    };
                    affine_tasks::validate_schedule(&schedule)?;
                    let prompt = if *stdin {
                        let mut buf = String::new();
                        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                        buf
                    } else {
                        String::new()
                    };
                    let body_md = affine_tasks::cron_body_stub(title, &schedule, &prompt);
                    let resp = client
                        .call_tool(
                            "create_doc_from_markdown",
                            json!({"workspaceId": cfg.workspace, "title": title, "markdown": body_md, "parentDocId": board.doc_id}),
                        )
                        .await
                        .context("create_doc_from_markdown")?;
                    let body = resp
                        .get("docId")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let _ = client
                        .call_tool(
                            "add_tag_to_doc",
                            json!({"workspaceId": cfg.workspace, "docId": body, "tag": affine_tasks::CRON_TAG}),
                        )
                        .await;
                    let row = client
                        .call_tool(
                            "add_database_row",
                            json!({"workspaceId": cfg.workspace, "docId": board.doc_id,
                                   "databaseBlockId": board.db_id,
                                   "cells": {"Title": title, "Schedule": schedule, "Status": "active"},
                                   "linkedDocId": body}),
                        )
                        .await
                        .context("add_database_row")?;
                    println!("крон поставлен: {title}");
                    println!("  расписание: {schedule}");
                    println!(
                        "  строка: {}",
                        row.get("rowBlockId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                    );
                    println!("  тело:   {body}");
                }
                CronSub::List => {
                    println!(
                        "{:<14} {:<8} {:<18} {:<17} {:<13} ЗАГОЛОВОК",
                        "ДОСКА", "СТАТУС", "РАСПИСАНИЕ", "LAST RUN", "СТРОКА"
                    );
                    let mut summary: Vec<String> = Vec::new();
                    for bt in &cron_boards {
                        // полный статус каждой доски: пустые/трэш/отсутствующие
                        // тоже видны — молчаливые пропуски читаются как баг
                        match affine_tasks::board_lookup(&mut client, &cfg, bt).await {
                            Ok(affine_tasks::BoardFind::Found(board)) => {
                                let rows =
                                    affine_tasks::read_rows(&mut client, &cfg, &board, &root)
                                        .await?;
                                summary.push(format!("{bt}: {}", rows.len()));
                                for r in &rows {
                                    println!(
                                        "{:<14} {:<8} {:<18} {:<17} {:<13} {}",
                                        truncate(bt, 13),
                                        r.status,
                                        r.cron_schedule_of(),
                                        r.last_run_display(),
                                        &r.row_id[..r.row_id.len().min(12)],
                                        truncate(&r.title, 36)
                                    );
                                }
                            }
                            Ok(affine_tasks::BoardFind::Trashed) => {
                                if bt == "Crons" {
                                    summary
                                        .push("Crons: в трэше (глобальная не используется)".into());
                                } else {
                                    summary.push(format!(
                                        "{bt}: доска в трэше — восстанови или пересоздай"
                                    ));
                                }
                            }
                            Ok(affine_tasks::BoardFind::Missing) => {
                                if bt == "Crons" {
                                    summary.push(
                                        "Crons: глобальной доски нет (не используется)".into(),
                                    );
                                } else {
                                    summary
                                        .push(format!("{bt}: доски нет — создай в папке проекта"));
                                }
                            }
                            Err(e) => summary.push(format!("{bt}: ошибка поиска ({e:#})")),
                        }
                    }
                    if !summary.is_empty() {
                        println!();
                        println!("доски: {}", summary.join(" · "));
                    }
                }
                CronSub::Pause { id } | CronSub::Resume { id } => {
                    let status = if matches!(command_ref, CronSub::Pause { .. }) {
                        "paused"
                    } else {
                        "active"
                    };
                    let (board, row) =
                        resolve_cron_any_board(&mut client, &cfg, &cron_boards, &root, id).await?;
                    let patch = affine_tasks::RowPatch::cron_status(status);
                    affine_tasks::edit_row(&mut client, &cfg, &board, &row, &patch).await?;
                    println!("статус: {status}");
                }
                CronSub::Rm { id } => {
                    let (board, row) =
                        resolve_cron_any_board(&mut client, &cfg, &cron_boards, &root, id).await?;
                    client
                        .call_tool("delete_database_row", json!({"workspaceId": cfg.workspace, "docId": board.doc_id, "databaseBlockId": board.db_id, "rowBlockId": row.row_id}))
                        .await
                        .context("delete_database_row")?;
                    if !row.body_doc.is_empty() {
                        let _ = client
                            .call_tool(
                                "trash_doc",
                                json!({"workspaceId": cfg.workspace, "docId": row.body_doc}),
                            )
                            .await;
                    }
                    println!("крон удалён: {}", row.title);
                }
            }
        }
        Commands::Task { command, project } => {
            // required-флаг в clap не живёт после субкоманды — проверяем сами
            let Some(project) = project else {
                anyhow::bail!("--project <имя> обязателен (uwuwu | origa | foilcap | …)");
            };
            let mut client = affine_client::AffineClient::connect(&cfg).await?;
            let board = affine_tasks::find_board(&mut client, &cfg, &project)
                .await
                .with_context(|| format!("доска проекта «{project}» не найдена"))?;
            match command {
                TaskSub::Create {
                    title,
                    r#type: ty,
                    stdin,
                    attach,
                } => {
                    let task_type = affine_tasks::TaskType::parse(ty.as_deref().unwrap_or("dev"))
                        .with_context(|| {
                        format!(
                            "--type '{}': ожидался dev|spike|bug",
                            ty.as_deref().unwrap_or("")
                        )
                    })?;
                    let body = if stdin {
                        let mut buf = String::new();
                        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                        buf
                    } else {
                        String::new()
                    };
                    let row = affine_tasks::create_task(
                        &mut client,
                        &cfg,
                        &board,
                        &title,
                        &body,
                        task_type,
                    )
                    .await?;
                    println!("создана: {}", row.title);
                    println!("  строка: {}", row.row_id);
                    println!("  тело:   {}", row.body_doc);
                    // аттачи — после «создана»: сбой не прячет id задачи,
                    // ретрай create осознан (задача уже существует)
                    if !attach.is_empty() {
                        affine_blob::attach_files(&cfg, &row.body_doc, &attach).await?;
                    }
                }
                TaskSub::List {
                    status,
                    project: proj_override,
                } => {
                    let project = proj_override
                        .or_else(|| Some(project.clone()))
                        .unwrap_or_else(|| "uwuwu".to_string());
                    let mut client = affine_client::AffineClient::connect(&cfg).await?;
                    let board = affine_tasks::find_board(&mut client, &cfg, &project)
                        .await
                        .with_context(|| format!("доска проекта «{project}» не найдена"))?;
                    let rows = affine_tasks::read_rows(&mut client, &cfg, &board, &root).await?;
                    let filtered: Vec<_> = rows
                        .iter()
                        .filter(|r| {
                            status
                                .as_deref()
                                .map(|s| r.status.to_lowercase().starts_with(&s.to_lowercase()))
                                .unwrap_or(true)
                        })
                        .collect();
                    if filtered.is_empty() {
                        println!("Задач нет.");
                    }
                    println!("{:<13} {:<12} {:<34} PR", "СТАТУС", "СТРОКА", "ЗАГОЛОВОК");
                    for r in &filtered {
                        println!(
                            "{:<13} {:<12} {:<34} {}",
                            r.status,
                            &r.row_id[..r.row_id.len().min(12)],
                            truncate(&r.title, 32),
                            r.pr
                        );
                    }
                }
                TaskSub::Get { id } => {
                    let (_board, row) =
                        resolve_row_any_board(&mut client, &cfg, &root, &project, &id).await?;
                    println!("Задача:   {}", row.title);
                    println!("Статус:   {}", row.status);
                    if !row.pr.is_empty() {
                        println!("PR:       {}", row.pr);
                    }
                    if !row.branch.is_empty() {
                        println!("Ветка:    {}", row.branch);
                    }
                    println!("Строка:   {}", row.row_id);
                    if !row.body_doc.is_empty() {
                        let ui = std::env::var("UWUWU_AFFINE_UI_URL")
                            .unwrap_or_else(|_| "https://affine.uwuwu.net".into());
                        println!(
                            "Тело:     {ui}/workspace/{}/{}",
                            cfg.workspace, row.body_doc
                        );
                        // тело целиком: task get — единственный cli-путь агента
                        // к содержимому; картинки из markdown выгружаются рядом
                        if let Ok(e) = client
                            .call_tool(
                                "export_doc_markdown",
                                json!({"workspaceId": cfg.workspace, "docId": row.body_doc}),
                            )
                            .await
                        {
                            let md = e.get("markdown").and_then(|m| m.as_str()).unwrap_or("");
                            if !md.trim().is_empty() {
                                println!("\n----- тело -----\n{md}");
                                let imgs = save_body_images(md, &row.row_id).await;
                                if !imgs.is_empty() {
                                    println!("Картинки:");
                                    for p in imgs {
                                        println!("  {p}");
                                    }
                                }
                            }
                        }
                        let comments =
                            affine_tasks::recent_comments(&mut client, &cfg, &row, 5).await?;
                        if !comments.is_empty() {
                            println!("Комменты:");
                            for (text, _) in comments {
                                println!("  • {}", truncate(&text, 90));
                            }
                        }
                    }
                }
                TaskSub::Edit {
                    id,
                    status,
                    title,
                    pr,
                    branch,
                } => {
                    let (_board, row) =
                        resolve_row_any_board(&mut client, &cfg, &root, &project, &id).await?;
                    let patch = affine_tasks::RowPatch {
                        status: status.clone(),
                        title: title.clone(),
                        pr: pr.clone(),
                        branch: branch.clone(),
                    };
                    affine_tasks::edit_row(&mut client, &cfg, &_board, &row, &patch).await?;
                    println!("обновлено: {}", row.title);
                }
                TaskSub::Comment { id, stdin, text } => {
                    let (_board, row) =
                        resolve_row_any_board(&mut client, &cfg, &root, &project, &id).await?;
                    let body = facts::read_text(stdin, &text)?;
                    if body.trim().is_empty() {
                        anyhow::bail!("пустой коммент");
                    }
                    affine_tasks::comment_task(&mut client, &cfg, &row, &body).await?;
                    println!("коммент отправлен: {}", truncate(&body, 60));
                }
                TaskSub::Close { id, note } => {
                    let (_board, row) =
                        resolve_row_any_board(&mut client, &cfg, &root, &project, &id).await?;
                    let patch = affine_tasks::RowPatch {
                        status: Some("Done".into()),
                        ..Default::default()
                    };
                    affine_tasks::edit_row(&mut client, &cfg, &_board, &row, &patch).await?;
                    if let Some(note) = note {
                        affine_tasks::comment_task(&mut client, &cfg, &row, &note).await?;
                    }
                    println!("закрыта: {}", row.title);
                }
                TaskSub::Body { id, md, replace } => {
                    let (_board, row) =
                        resolve_row_any_board(&mut client, &cfg, &root, &project, &id).await?;
                    if row.body_doc.is_empty() {
                        anyhow::bail!("у задачи «{}» нет тела-дока", row.title);
                    }
                    let text = match &md {
                        Some(path) => {
                            std::fs::read_to_string(path).with_context(|| format!("--md {path}"))?
                        }
                        None => {
                            let mut buf = String::new();
                            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                            buf
                        }
                    };
                    if replace {
                        let n_diagrams =
                            affine_tasks::replace_body(&mut client, &cfg, &row.body_doc, &text)
                                .await?;
                        println!("тело заменено: {} (диаграмм: {n_diagrams})", row.title);
                    } else {
                        let (n_diagrams, n_deleted) = affine_tasks::append_body_markdown(
                            &mut client,
                            &cfg,
                            &row.body_doc,
                            &text,
                        )
                        .await?;
                        println!(
                            "тело дописано: {} (диаграмм: {n_diagrams}, заменено/удалено: {n_deleted})",
                            row.title
                        );
                    }
                }
            }
        }
        Commands::Refresh => {
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
    }

    Ok(())
}
