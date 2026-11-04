//! CLI surface: clap definitions for uwuwu-cli. Handlers live in the
//! sibling modules; this module only describes the argument grammar.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "uwuwu-cli")]
#[command(about = "CLI for uwuwu/wiki: доступ агентам к платформе uwuwu (SiYuan)")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// Сабкоманды uwuwu-cli task.
#[derive(clap::Subcommand)]
pub(crate) enum TaskSub {
    /// Создать задачу: док-тело (тег «Task») + строка Draft — черновик
    /// на валидации триажем (Todo/Design ставят триаж и владелец)
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
        /// Быстрая проверка статуса: без экспорта тела и картинок
        #[arg(long = "no-body")]
        no_body: bool,
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
        #[arg(
            long,
            value_name = "THREAD_ID",
            help = "Ответить в тред: префикс id верхнеуровневого коммента (id — в task thread)"
        )]
        reply: Option<String>,
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
        #[arg(long, conflicts_with = "md", help = "Текст из stdin")]
        stdin: bool,
        #[arg(long, help = "Файл с markdown (иначе stdin)")]
        md: Option<String>,
        /// Заменить тело ЦЕЛИКОМ (агент читает task get, правит, шлёт новое).
        /// Rich-контент (embed-диаграммы) не переживает — пересоздай фенсами.
        #[arg(long)]
        replace: bool,
        /// Заменить содержимое секции по заголовку («Решение» или «## Решение»):
        /// блоки секции удаляются, новый markdown встаёт на их месте,
        /// заголовок и аттачи/эмбеды вне секции не трогаются. Markdown — БЕЗ
        /// строки заголовка; пустой ввод очищает секцию.
        #[arg(long, value_name = "HEADING", conflicts_with = "replace")]
        replace_section: Option<String>,
    },
    /// Удалить задачу: строка вниз + тело в трэш (восстановимо restore_doc).
    /// Только Draft-черновики (их чистит триаж); живые задачи — отказ.
    /// Тело без строки (сирота упавшего create) — трэш по docId.
    /// Guard: вердикт по независимым источникам, undo-журнал до мутации.
    Rm {
        #[arg(help = "row-id / doc-id / префикс")]
        id: String,
    },
    /// Батч-снос строк канбана с guard'ом: вердикты по независимым
    /// источникам (FS тела / SQL по id / полный render / док по тайтлу),
    /// dry-run дефолт, undo-журнал в воркспейсе, пост-чек состава доски.
    /// Live-строки — только с per-row ack (--live) и без удаления тела.
    Clean {
        #[arg(
            long,
            value_name = "FRAG[,FRAG…]",
            help = "Кандидаты: row-id/doc-id/префикс/подстрока, через запятую"
        )]
        ids: String,
        /// Без флага — dry-run: печать плана с вердиктами, ноль мутаций.
        #[arg(long, help = "Исполнить план (иначе dry-run)")]
        yes: bool,
        #[arg(
            long,
            value_name = "FRAG[,FRAG…]",
            help = "Per-row ack на Live-строки: отвязка строки, тело не трогается"
        )]
        live: Option<String>,
    },
    /// Восстановление строк из undo-журнала (data/assets/undo/ воркспейса).
    /// Идемпотентен: тайтл уже на доске → no-op. Меняет id строк (новый
    /// itemID/docID) — карта старый→новый в выводе.
    Undo {
        /// Без аргумента — список журналов.
        #[arg(help = "Имя журнала (data/assets/undo/<name>) или путь; нет аргумента — список")]
        journal: Option<String>,
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
    /// Read a document by slug: полный hpath (`<project>/<doc>`,
    /// `<topic>/<doc>`, слаг Daily) или одиночный заголовок (если однозначен;
    /// иначе — перечень кандидатов) | sofa/<post_id>.
    Read {
        #[arg(help = "Slug: project/doc | topic/doc | слаг Daily | Заголовок | sofa/<post_id>")]
        slug: String,
    },
    /// Download a workspace asset (image/attachment) by key from assets/.
    Blob {
        #[arg(help = "Blob key, e.g. pixel.png")]
        key: String,
        #[arg(long, help = "Output file ('-' = stdout). Default: <key> in cwd")]
        out: Option<String>,
    },
    /// (не портирован на SiYuan v1)
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
    /// Полнотекстовый поиск по блокам SiYuan + опционально SOFA.
    Search {
        #[arg(help = "Search query")]
        query: String,
        #[arg(long, default_value = "10", help = "hits limit")]
        limit: usize,
        #[arg(long, default_value = "5", help = "SOFA public limit (0 = off)")]
        sofa: usize,
        #[arg(long, default_value = "0.3", help = "min score threshold (0..1)")]
        threshold: f32,
    },
    /// Кроны: строки доски «Кроны <Proj>» (SiYuan database).
    Cron {
        #[command(subcommand)]
        command: CronSub,
    },
    /// Канбан-задачи («Задачи <Proj>» SiYuan database): create/list/get/
    /// edit/comment/close; комменты — секция тел задач.
    Task {
        #[command(subcommand)]
        command: TaskSub,
        /// Project board (обязательно: uwuwu | origa | foilcap | …)
        #[arg(long, global = true)]
        project: Option<String>,
    },
}
