//! CLI surface: clap definitions for uwuwu-cli. Handlers live in the
//! sibling modules; this module only describes the argument grammar.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::request;

#[derive(Parser)]
#[command(name = "uwuwu-cli")]
#[command(
    about = "CLI for uwuwu/wiki: semantic search over experience, access docs, facts (AFFiNE-backed)"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// Сабкоманды uwuwu-cli task.
#[derive(clap::Subcommand)]
pub(crate) enum TaskSub {
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
    /// Цепочки комментов: какая тема, кто что ответил
    Thread {
        #[arg(help = "row-id / doc-id / префикс")]
        id: String,
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
pub(crate) enum CronSub {
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
pub(crate) enum Commands {
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
