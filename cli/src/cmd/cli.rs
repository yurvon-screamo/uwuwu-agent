//! CLI surface: clap definitions for uwuwu-cli. Handlers live in the
//! sibling modules; this module only describes the argument grammar.

use clap::{Parser, Subcommand};

/// Version line assembled at build time: semver + git revision + build date
/// (see cli/build.rs). The clap built-in `--version` prints it and exits
/// during parsing — before dispatch and any network access.
const VERSION_STR: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("UWUWU_GIT_HASH"),
    " ",
    env!("UWUWU_BUILD_DATE"),
    ")"
);

#[derive(Parser)]
#[command(name = "uwuwu-cli", version = VERSION_STR)]
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
        #[arg(
            long,
            value_name = "TYPE",
            help = "Тип: dev | spike | bug (колонка «Тип» доски + шаблон тела); без флага — dev"
        )]
        r#type: Option<String>,
        #[arg(long, help = "Текст тела из stdin (опционально)")]
        stdin: bool,
        #[arg(
            long = "attach",
            value_name = "FILE",
            help = "Аттач в тело (repeatable)"
        )]
        attach: Vec<std::path::PathBuf>,
        /// Расписание крона: атрибут custom-schedule в доке-теле; демона
        /// файрит задачу в статусе Вейт по этому расписанию
        #[arg(
            long = "schedule",
            value_name = "EXPR",
            help = "Расписание: 5m|2h|1d | HH:MM [DD.MM] | cron 5 полей"
        )]
        schedule: Option<String>,
        // Осмотр скелета без записи: живой create ради просмотра костяка
        // кормит триаж-демон мусорными черновиками.
        #[arg(
            long = "dry-run",
            help = "Печать скелета тела в stdout, ноль записей в воркспейс (без frontmatter и тега — их добавляют ядро/create)"
        )]
        dry_run: bool,
    },
    /// Очередь задач доски
    List {
        #[arg(long, help = "Фильтр по статусу (todo|design|in progress|…)")]
        status: Option<String>,
        #[arg(long, help = "Фильтр по типу (bug|dev|spike)")]
        r#type: Option<String>,
        #[arg(long, help = "Проект (переопределяет --project задачи)")]
        project: Option<String>,
    },
    /// Карточка задачи: колонки + свежие комменты
    Get {
        #[arg(
            allow_hyphen_values = true,
            help = "row-id / doc-id / уникальный префикс"
        )]
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
        /// Тип задачи: колонка «Тип» доски; пустая строка снимает
        /// (клетка очищается — тип «не задан»)
        #[arg(
            long,
            value_name = "TYPE",
            help = "Тип: dev | spike | bug; \"\" — снять"
        )]
        r#type: Option<String>,
        /// Расписание крона: записать/сменить; пустая строка снимает
        /// (пауза = закрыть задачу, расписания без задачи не бывает)
        #[arg(
            long = "schedule",
            value_name = "EXPR",
            help = "Расписание: 5m|2h|1d | HH:MM [DD.MM] | cron 5 полей; \"\" — снять"
        )]
        schedule: Option<String>,
    },
    /// Заявить сессию на задачу: sessionID в ячейку «Сессия dev» — структурное
    /// свидетельство «работу ведёт эта сессия». Фабрика живую заявку уважает:
    /// не спавнит дубль и не считает строку сиротой. SessionID берётся из
    /// --session либо из env OPENCODE_SESSION_ID; нет ни того, ни другого —
    /// отказ. Колонка «Сессия dev» на доске обязательна (громкий отказ иначе).
    Claim {
        #[arg(allow_hyphen_values = true, help = "row-id / doc-id / префикс")]
        id: String,
        #[arg(
            long,
            value_name = "SESSION_ID",
            help = "Явный sessionID (иначе OPENCODE_SESSION_ID из env)"
        )]
        session: Option<String>,
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
    /// Фенсы ```html_embed [имя] конвертируются в живые HTML-блоки:
    /// именованный фенс заменяет прежнюю диаграмму с этим именем
    /// (адресация по маркер-абзацу ⁂ diagram:<имя>),
    /// пустой (```html_embed имя без тела) — удаляет её,
    /// анонимный — вставляет html инлайн. Отказы до мутаций: дубль
    /// имени фенса в вводе, пустая строка внутри html-тела.
    /// --attach (repeatable) грузит файлы в assets/ и дописывает референсы
    /// после контент-операции; вызов с одними аттачами (без --stdin/--md) валиден.
    /// Вход — контент тела, не обёртка task get: шапка карточки
    /// («Задача:/Статус:/Строка:…»), маркер «----- тело -----» и frontmatter
    /// в контенте — отказ (frontmatter дока ведёт ядро, титульный H1 —
    /// из заголовка дока); снапшот --base допустим полным — cli срезает
    /// обёртку при сверке сам.
    Body {
        #[arg(
            allow_hyphen_values = true,
            help = "row-id / doc-id / префикс / подстрока заголовка"
        )]
        id: String,
        #[arg(long, conflicts_with = "md", help = "Текст из stdin")]
        stdin: bool,
        #[arg(long, help = "Файл с markdown (иначе stdin)")]
        md: Option<String>,
        /// Заменить тело ЦЕЛИКОМ (агент читает task get, правит, шлёт новое).
        /// Живые HTML-блоки тела переживают перезапись (экспорт ядра отдаёт
        /// их сырым HTML); новые диаграммы вставляй фенсами ```html_embed —
        /// именованные инлайнятся парами «⁂ diagram:<имя> + html».
        /// Требует --base (снапшот свежести) или --force: без снапшота
        /// параллельная правка тела теряется тихо. Секция «## Комменты»
        /// системная: если новый контент её не несёт, существующая переносится
        /// дословно в конец — осознанная замена/снос выражаются самим контентом.
        #[arg(long)]
        replace: bool,
        /// Снапшот свежести для --replace: файл с выводом `task get` целиком,
        /// сырым экспортом тела или чистым телом, снятый ДО правки. Replace
        /// сверяет снапшот со свежим телом (нормализованно) и отказывает при
        /// расхождении — параллельная правка не затирается. Комменты — после
        /// replace: свой коммент между снапшотом и replace уронит сверку.
        #[arg(
            long,
            value_name = "SNAPSHOT",
            requires = "replace",
            conflicts_with_all = ["replace_section", "force"]
        )]
        base: Option<String>,
        /// Осознанный overwrite для --replace: пишет без сверки свежести
        /// (--force старше --base). Секция «## Комменты» переносится как
        /// и без --force — сила флага только в отказе от сверки.
        #[arg(
            long,
            requires = "replace",
            conflicts_with_all = ["replace_section", "base"]
        )]
        force: bool,
        /// Заменить содержимое секции по заголовку («Решение» или «## Решение»):
        /// блоки секции удаляются, новый markdown встаёт на их месте,
        /// заголовок и аттачи/эмбеды вне секции не трогаются. Markdown — БЕЗ
        /// строки заголовка; пустой ввод очищает секцию. Матч только ##:
        /// ATX-префикс запроса срезается («## Решение» = «Решение»), тёзка
        /// на уровне 1/≥3 (учёт двойных пробелов и ^block-id) — отказ
        /// вместо тихого append-дубля в конец. html_embed-фенс в секции с
        /// именем, уже живущим вне секции, даст два одинаковых маркера
        /// ⁂ diagram:<имя> — последующий append адресует первый по порядку
        /// (секция не ищет по всему доку).
        #[arg(long, value_name = "HEADING", conflicts_with = "replace")]
        replace_section: Option<String>,
        #[arg(
            long = "attach",
            value_name = "FILE",
            help = "Аттач в конец тела (repeatable)"
        )]
        attach: Vec<std::path::PathBuf>,
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

/// Кроны — задачи с расписанием (custom-schedule в доке-теле, статус Вейт):
/// `cron list` — инвентарь мастеров доски (расписание + last run из state
/// демона). Установка/снятие — task create/edit --schedule; сессионные
/// set/pause/resume/rm выпилены вместе с крон-досками (унификация кронов
/// с задачами).
#[derive(clap::Subcommand)]
pub(crate) enum CronSub {
    /// Мастера крон-задач доски проекта (--project, деф. uwuwu)
    List {
        #[arg(long, help = "Проект (доска «Задачи <proj>»; деф. uwuwu)")]
        project: Option<String>,
    },
}

/// Реестр долгосрочных целей проекта: доска «Цели <proj>» (заводит владелец
/// руками в UI — одноразовая настройка). Строки bound к телам-докам,
/// статусы канонические: todo / in progress / done. Связей с задачами нет.
#[derive(clap::Subcommand)]
pub(crate) enum GoalSub {
    /// Реестр целей доски проекта
    List {
        #[arg(long, help = "Фильтр по статусу (todo|in progress|done)")]
        status: Option<String>,
    },
    /// Новая цель: строка + тело-док «## Суть» (контекст из stdin)
    Add {
        #[arg(long, help = "Заголовок цели")]
        title: String,
        #[arg(long, help = "Статус цели (деф. todo: todo | in progress | done)")]
        status: Option<String>,
        #[arg(long, help = "Контекст цели из stdin (тело под «Суть»)")]
        stdin: bool,
    },
    /// Правка цели: статус и заголовок (тело — в UI или task-механикой тел)
    Edit {
        #[arg(
            allow_hyphen_values = true,
            help = "row-id / doc-id / префикс / подстрока"
        )]
        id: String,
        #[arg(long, help = "Новый статус (todo | in progress | done)")]
        status: Option<String>,
        #[arg(long, help = "Новый заголовок")]
        title: Option<String>,
    },
    /// Снести цель: строка вниз + тело в трэш (undo-журнал)
    Rm {
        #[arg(
            allow_hyphen_values = true,
            help = "row-id / doc-id / префикс / подстрока"
        )]
        id: String,
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
        #[arg(
            help = "SQL одним аргументом (кавычь в шелле), например \"select count(*) as n from blocks\""
        )]
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
    /// Перечисление доков по hpath-слагу: штатный путь «какие доки в
    /// проекте/папке» (ADR-нумерация, Design-скан, гигиена доков).
    Docs {
        #[command(subcommand)]
        command: DocsSub,
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
    /// Универсальный поиск: полнотекст (SQL) + доки по заголовкам (hpath,
    /// case-insensitive, AND-токены) + семантика (локальный
    /// эмбеддинг-индекс) — top-n по каждому методу; опционально SOFA.
    Search {
        #[arg(help = "Search query")]
        query: String,
        #[arg(long, default_value = "10", help = "hits limit per method")]
        limit: usize,
        #[arg(long, default_value = "0", help = "SOFA public limit (0 = off)")]
        sofa: usize,
        #[arg(
            long,
            default_value = "0.35",
            help = "semantic cosine threshold (0..1)"
        )]
        threshold: f32,
    },
    /// Локальный эмбеддинг-индекс (семантическая половина поиска):
    /// обслуживание — полный пересбор и статус.
    Semantic {
        #[command(subcommand)]
        command: SemanticSub,
    },
    /// Кроны: инвентарь крон-задач (расписание в доке-теле задачи).
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
    /// Реестр долгосрочных целей («Цели <Proj>» SiYuan database): доску
    /// заводит владелец руками в UI; list/add/edit/rm, close = edit --status done.
    Goal {
        #[command(subcommand)]
        command: GoalSub,
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

/// Сабкоманды `docs`.
#[derive(Subcommand)]
pub(crate) enum DocsSub {
    /// Прямые дети hpath-префикса: доки именами, подпапки с хвостовым «/».
    /// Адресация как у read (слаг без ведущего «/»): `uwuwu`,
    /// `uwuwu/ADR`, `platform`. Неизвестное пространство — отказ
    /// с раскладкой доступных; известное без доков — «пусто».
    Ls {
        #[arg(help = "hpath-слаг: <проект>[/<подпапка>]")]
        prefix: String,
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
        #[arg(long, conflicts_with = "text", help = "Текст из stdin")]
        stdin: bool,
        #[arg(trailing_var_arg = true, help = "Markdown-текст (факт или секция)")]
        text: Vec<String>,
    },
    /// Перезаписать контент целиком на месте (docId стабилен); дока нет —
    /// создать без скелета (полная ответственность пишущего за контент).
    Replace {
        #[arg(long, conflicts_with = "md", help = "Текст из stdin")]
        stdin: bool,
        #[arg(long, help = "Файл с markdown (обязателен без --stdin)")]
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
    /// Ручная дотяжка дельты и reconcile вне бюджета поиска (очередь в
    /// тысячи строк; хвост над бюджетом догоняется повтором команды).
    Sync,
}
