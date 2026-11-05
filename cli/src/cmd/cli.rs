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
        #[arg(allow_hyphen_values = true, help = "row-id / doc-id / уникальный префикс")]
        id: String,
        /// Быстрая проверка статуса: без экспорта тела и картинок
        #[arg(long = "no-body")]
        no_body: bool,
    },
    /// Правка колонок строки
    Edit {
        #[arg(allow_hyphen_values = true, help = "row-id / doc-id / префикс")]
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
    /// Коммент в док задачи (диалог юзер ↔ агент). Текст — единственный
    /// позиционный аргумент (или --stdin); читать комменты: task get / task thread.
    /// Токены-подкоманды и флаги в тексте отклоняются гвардом до отправки.
    Comment {
        #[arg(allow_hyphen_values = true, help = "row-id / doc-id / префикс")]
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
        #[arg(allow_hyphen_values = true, help = "row-id / doc-id / префикс")]
        id: String,
    },
    /// Закрыть задачу (Done) с опциональной заметкой
    Close {
        #[arg(allow_hyphen_values = true, help = "row-id / doc-id / префикс")]
        id: String,
        #[arg(long, help = "Заметка-коммент при закрытии")]
        note: Option<String>,
    },
    /// Дописать markdown в тело задачи/крона (stdin или --md файл).
    /// Фенсы ```html_embed [имя] конвертируются в живые embed_html-диаграммы:
    /// именованный фенс заменяет прежнюю диаграмму с этим именем,
    /// пустой (```html_embed имя без тела) — удаляет её.
    Body {
        #[arg(allow_hyphen_values = true, help = "row-id / doc-id / префикс / подстрока заголовка")]
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
        /// строки заголовка; пустой ввод очищает секцию. Матч только ##:
        /// ATX-префикс запроса срезается («## Решение» = «Решение»), тёзка
        /// на уровне 1/≥3 (учёт двойных пробелов и ^block-id) — отказ
        /// вместо тихого append-дубля в конец.
        #[arg(long, value_name = "HEADING", conflicts_with = "replace")]
        replace_section: Option<String>,
    },
    /// Удалить задачу: строка вниз + тело в трэш (восстановимо restore_doc).
    /// Только Draft-черновики (их чистит триаж); живые задачи — отказ.
    /// Тело без строки (сирота упавшего create) — трэш по docId.
    /// Guard: вердикт по независимым источникам, undo-журнал до мутации.
    Rm {
        #[arg(allow_hyphen_values = true, help = "row-id / doc-id / префикс")]
        id: String,
    },
    /// Батч-снос строк канбана с guard'ом: вердикты по независимым
    /// источникам (FS тела / SQL по id / полный render / док по тайтлу),
    /// dry-run дефолт, undo-журнал в воркспейсе, пост-чек состава доски.
    /// Live-строки — только с per-row ack (--live) и без удаления тела.
    Clean {
        #[arg(
            long,
            allow_hyphen_values = true,
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
        /// Режим списка: сильнее позиционного (с ним и с голым вызовом).
        #[arg(long, help = "Список журналов вместо восстановления")]
        list: bool,
        /// Без аргумента (или с --list) — список журналов.
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
    /// Все кроны доски (--project → «Кроны <proj>», деф. uwuwu)
    List {
        #[arg(long, help = "Проект (доска «Кроны <proj>»; деф. uwuwu)")]
        project: Option<String>,
    },
    /// Остановить (Status=paused)
    Pause {
        #[arg(allow_hyphen_values = true, help = "row-id / префикс")]
        id: String,
        #[arg(long, help = "Проект (доска «Кроны <proj>»; деф. uwuwu)")]
        project: Option<String>,
    },
    /// Запустить обратно (Status=active)
    Resume {
        #[arg(allow_hyphen_values = true, help = "row-id / префикс")]
        id: String,
        #[arg(long, help = "Проект (доска «Кроны <proj>»; деф. uwuwu)")]
        project: Option<String>,
    },
    /// Удалить крон (строка + тело в трэш)
    Rm {
        #[arg(allow_hyphen_values = true, help = "row-id / префикс")]
        id: String,
        #[arg(long, help = "Проект (доска «Кроны <proj>»; деф. uwuwu)")]
        project: Option<String>,
    },
}

/// Живые kernel-пробы: диагностика уровня ядра из агентской сессии без
/// env-токена (токен берётся из конфигурации cli). One-shot, без ретраев —
/// сырое состояние ядра видно на первом ответе. Диагностические вердикты
/// (Bytes/Missing, пустой export/ls) — exit 0; неудача пробы — exit ≠0.
#[derive(clap::Subcommand)]
pub(crate) enum KernelSub {
    /// Живость ядра: авторизуемый sql «select 1» (транспорт+auth+DB;
    /// version-эндпоинт отвечает и без токена — пробой auth не является)
    Ping,
    /// SQL-проба: полный конверт ядра JSON (data + limit/truncated as-is)
    Sql {
        #[arg(help = "SQL одним аргументом (кавычь в шелле), например \"select count(*) as n from blocks\"")]
        stmt: String,
    },
    /// getFile-проба: Bytes/Missing — диагностические ответы (exit 0),
    /// Failed — неудача (exit ≠ 0)
    File {
        #[arg(help = "Путь от корня воркспейса (data/…)")]
        path: String,
        #[arg(long, help = "Записать байты файла в этот путь")]
        out: Option<String>,
    },
    /// readDir-проба: имена записей, директории с «/»
    Ls {
        #[arg(help = "Путь от корня воркспейса (data/…)")]
        path: String,
    },
    /// exportMdContent-проба: markdown дока по id (пустой контент виден
    /// сразу — подсказка в stderr)
    Export {
        #[arg(help = "docId или id блока")]
        doc_id: String,
    },
    /// render-проба доски (MCP database render): rowCount/page/itemCount
    /// as-is — деградация досок и пагинация строк
    Render {
        #[arg(help = "av-id доски")]
        av_id: String,
        #[arg(help = "blockID дока-носителя доски")]
        block_id: String,
        #[arg(long, default_value = "1", help = "Номер страницы")]
        page: u64,
    },
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
    /// Персона владельца (SiYuan-док Experience:/personal/PERSON): show —
    /// прочитать (агенты в начале работы), append — дописать долговременный
    /// факт (владелец/дример), replace — полная перезапись на месте.
    Person {
        #[command(subcommand)]
        command: PersonSub,
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
    /// Универсальный поиск: полнотекст (SQL) + семантика (локальный
    /// эмбеддинг-индекс) — top-n по каждому методу; опционально SOFA.
    Search {
        #[arg(help = "Search query")]
        query: String,
        #[arg(long, default_value = "10", help = "hits limit per method")]
        limit: usize,
        #[arg(long, default_value = "0", help = "SOFA public limit (0 = off)")]
        sofa: usize,
        #[arg(long, default_value = "0.35", help = "semantic cosine threshold (0..1)")]
        threshold: f32,
    },
    /// Локальный эмбеддинг-индекс (семантическая половина поиска):
    /// обслуживание — полный пересбор и статус.
    Semantic {
        #[command(subcommand)]
        command: SemanticSub,
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
    /// Живые kernel-пробы SiYuan: ping/sql/file/ls/export/render —
    /// диагностика ядра из агентской сессии без env-токена.
    Kernel {
        #[command(subcommand)]
        command: KernelSub,
        /// Таймаут пробы в секундах (тихий вис должен различаться быстро)
        #[arg(
            long,
            global = true,
            default_value = "10",
            value_name = "SECS",
            help = "Таймаут пробы, сек (штатные команды — 120)"
        )]
        timeout: u64,
    },
    /// Диагностика деградации ядра SiYuan: kernel/wave/доска/FS↔SQL.
    /// Exit: 0 чисто, 1 деградация (включая недоступное ядро), 2 пробы не выполнились.
    Doctor {
        /// Project board для пробы доски
        #[arg(long, default_value = "uwuwu")]
        project: String,
        /// Окно wave-скана docker logs, минут
        #[arg(long, default_value = "30", value_name = "MIN")]
        since: u64,
    },
}

/// Персона владельца: приватный SiYuan-док (Experience:/personal/PERSON).
/// Один инстанс SiYuan на все машины — синков и пуш-политики нет.
#[derive(Subcommand)]
pub(crate) enum PersonSub {
    /// Актуальная персона (markdown). Дока нет — честная ошибка с подсказкой
    /// создания (не молчаливый пустой вывод).
    Show,
    /// Дописать markdown в конец (долговременный факт/секция). Дока нет —
    /// создать со скелетом (шапка приватности + секции) и сразу с этим
    /// текстом. Дедуп перед append — на пишущем: прочитай `person show`.
    Append {
        #[arg(long, help = "Текст из stdin")]
        stdin: bool,
        #[arg(trailing_var_arg = true, help = "Markdown-текст (факт или секция)")]
        text: Vec<String>,
    },
    /// Перезаписать контент целиком на месте (docId стабилен); дока нет —
    /// создать без скелета (полная ответственность пишущего за контент).
    Replace {
        #[arg(long, conflicts_with = "md", help = "Текст из stdin")]
        stdin: bool,
        #[arg(long, help = "Файл с markdown (иначе stdin)")]
        md: Option<String>,
    },
}

/// Сабкоманды uwuwu-cli semantic.
#[derive(Subcommand)]
pub(crate) enum SemanticSub {
    /// Полный пересбор индекса (первичная инициализация ~55–70 мин
    /// или смена модели/размерности). Прогресс — в stderr.
    Rebuild {
        /// Продолжить прерванный билд с watermark (не стирать готовые
        /// чанки; сбой часового прогона не стоит часа GPU заново).
        #[arg(long)]
        resume: bool,
    },
    /// Состояние индекса: чанки, модель, watermark, очередь дельты.
    Status,
}
