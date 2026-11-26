//! Write-юзкейсы задач: create/edit/claim/comment/close/body.
//! Канон cli: тайтл уникален на доске, create идемпотентен по тайтлу,
//! fm ≡ поля (двойная запись держится портом), комменты — issue comments.

use crate::error::{UsecaseError, UsecaseResult};
use crate::naming;
use crate::ports::{
    Clock, CommentRef, IssueFieldStore, StatusDelivery, TaskBoardRepo, TickStateStore,
};
use crate::tasks::md_section;
use async_trait::async_trait;
use std::sync::Arc;
use uwuwu_domain::{Slot, Status, Task, TaskType};

/// Параметры создания задачи (канон create: Analys + дефолты, #1135).
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    pub title: String,
    pub body_md: String,
    /// Тип задачи; None — дефолт feature (канон).
    pub kind: Option<TaskType>,
    pub priority: String,
    /// Автомерж: мерж PR при входе в Review, строка остаётся в Review (#1232).
    /// Дефолт true — пишется на носитель явно (create-письмо, не умолчание чтения).
    pub automerge: bool,
    /// direct-класс (#1357): прямое поручение владельца — строка рождается
    /// сразу в In Progress (поля разбора create пишет явно), финализируется
    /// агентом в Review. Create-only: task_edit флаг не принимает.
    pub direct: bool,
    /// fact_track-класс (#1631): укороченный пайп без Design и без
    /// тестирования. Create-only, immutable: task_edit флаг не принимает
    /// (чужой ключ — громкий отказ гарда #1572 на границе MCP).
    /// Взаимоисключающ с direct (оба true — громкий отказ create).
    pub fact_track: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionAction {
    /// Секция заменена на месте.
    Replaced,
    /// Секции не было (пин h2) — добавлена в конец.
    Appended,
}

/// Квитанция записи тела: хэш и размер тела как записано (fm-граница
/// применена). body_hash — токен свежести последующих секционных правок.
/// Честная квитанция (#1331): verified=true — тело перечитано с носителя,
/// hash/size от прочитанного; verified=false — носитель не прочитан
/// (деградация контрольного чтения), hash/size от отправленного тела.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyWriteReceipt {
    pub body_hash: String,
    pub body_size: usize,
    pub verified: bool,
}

/// Квитанция edit (#1437): статусная запись несёт исход подтверждения
/// носителем — текст ответа MCP task_edit строится из неё, не из факта
/// отсутствия ошибки (echo мутации и серверные кеши доказательством
/// не служат, инстансы 19–20.11).
#[derive(Debug, Clone)]
pub struct EditReceipt {
    /// Some — патч нёс статус, исход подтверждения носителем.
    pub status: Option<StatusDelivery>,
    /// #1619: ответ был передан, но строка уже не была в Needs attention —
    /// ключ проигнорирован, квитанция обязана сказать это явно.
    pub owner_answer_ignored: bool,
}

/// Квитанция секционной правки (#1152): исход + тело после записи.
/// Полное тело не возвращается — сознательное сужение: возврат больших
/// тел убивал бы экономию секционного пути; порча ловится точечным
/// перечитыванием task_get по подозрению. Длины было/стало по телу и по
/// секции — контракт #1409: сверка без второго GET.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionEditReport {
    pub action: SectionAction,
    /// снятые одноимённые дубли на уровне пина (хилинг).
    pub duplicates_removed: usize,
    /// контент секции был пуст — секция очищена (заголовок жив).
    pub cleared: bool,
    /// размер тела до записи, байт.
    pub body_was: usize,
    /// размер тела после записи, байт.
    pub body_size: usize,
    /// размер контента секции до записи, байт (0 — append).
    pub section_was: usize,
    /// размер контента секции после записи, байт (0 — очистка).
    pub section_now: usize,
    /// запись прошла через confirm гарда сжатия (осознанное сокращение).
    pub confirmed: bool,
    /// FNV-1a 64 тела после записи (как записано в GitHub).
    pub body_hash: String,
    /// тело перечитано с носителя после записи (честная квитанция #1331).
    pub verified: bool,
}

#[async_trait]
pub trait TaskWriter: Send + Sync {
    /// Создание: Analys (create-дефолт #1135), лейбл proj:<project>,
    /// fm-скелет; дубль тайтла — ошибка.
    async fn create(&self, project: &str, spec: NewTask) -> UsecaseResult<u64>;
    /// Правка: статус/ветка/PR/приоритет/тайтл/тип (статус — поле доски,
    /// ветка/PR/приоритет — нативные поля issue, тип — нативный issue
    /// Type #1271, тайтл — rename). Статусная запись возвращает честную
    /// квитанцию подтверждения носителем (#1437).
    async fn edit(
        &self,
        project: &str,
        number: u64,
        patch: TaskPatch,
    ) -> UsecaseResult<EditReceipt>;
    /// Заявка сессии в слот (dev — по умолчанию).
    async fn claim(&self, number: u64, session: &str, slot: Option<Slot>) -> UsecaseResult<()>;
    /// Коммент (reply — цитатный якорь).
    async fn comment(&self, number: u64, text: &str, reply_to: Option<&str>) -> UsecaseResult<()>;
    /// Закрытие: Status=Done + issue closed (reason: completed|not_planned).
    /// Доска строки резолвится по всему конфигу (мультиборд, #1135).
    async fn close(&self, number: u64, not_planned: bool) -> UsecaseResult<()>;
    /// Полная замена тела (fm и дисплей-шапка task_get на границе
    /// срезаются — тела без fm, поля нативные и от тела не зависят);
    /// возвращает честную квитанцию: контрольное чтение носителя после
    /// записи (#1331) — hash/size от прочитанного, либо verified=false.
    /// None — параметр body отсутствует: громкий отказ с длиной текущего
    /// тела и рецептом хода (гард пустой записи #1572); явная пустая
    /// строка — осознанная очистка носителя.
    async fn set_body(
        &self,
        project: &str,
        number: u64,
        body_md: Option<&str>,
    ) -> UsecaseResult<BodyWriteReceipt>;
    /// Секционная правка тела (#1152): агент передаёт дельту секции, сервер
    /// собирает тело сам — большое тело не гоняется через контекст агента.
    /// Свежесть: base_hash (body_hash из fm task_get/квитанции) против
    /// текущего тела; мимо — громкий отказ с актуальным хэшем, записи нет
    /// (lost-update вместо молчаливого затирания; TOCTOU-окно сверка→PATCH
    /// неустранимо на GitHub Issues API — гран владельца). content None —
    /// громкий отказ с мерой секции (гард пустой записи #1572); явная
    /// пустая строка — осознанная очистка секции. Радикальное сжатие —
    /// отказ, осознанное сокращение — confirm (#1409).
    async fn set_body_section(
        &self,
        project: &str,
        number: u64,
        write: SectionWrite<'_>,
    ) -> UsecaseResult<SectionEditReport>;
}

/// Payload секционной записи: дельта + свежесть + осознанное преодоление
/// гарда сжатия (#1572). Один семантический юнит — сигнатура юзкейса
/// остаётся плоской (identity → payload → квитанция).
#[derive(Debug, Clone, Copy)]
pub struct SectionWrite<'a> {
    /// заголовок секции; ATX-префикс пинит уровень.
    pub section: &'a str,
    /// новый контент секции; None — запись без контента (гард #1572),
    /// Some("") — осознанная очистка.
    pub content: Option<&'a str>,
    /// body_hash тела на момент чтения (fm task_get / квитанция).
    pub base_hash: &'a str,
    /// осознанное проведение радикального сжатия (#1409).
    pub confirm: bool,
}

/// Чтение current для канон-гарда edit(): кеш-вид (wt/items — fast
/// path, fresh-рид не оплачен) или свежий носитель (рид уже
/// выполнен: идемпотентная ветка #1437, нечитаемый кеш-вид —
/// Low дельта-раунда #1615). Различие несёт квоту гарда: Fresh не
/// доплачивает повторный GraphQL-рид на отказной ветке.
enum GuardView {
    /// кеш-вид носителя; может отстать от носителя (REST items-индекс
    /// пере-фетчится в wt-окне и при лаге листа несёт до-записный
    /// статус минуты — инстанс #1612)
    Cached(Status),
    /// свежий носитель; повторная fresh-сверка не нужна
    Fresh(Status),
}

/// Option<Status> с носителя → верифицируемый current гарда: строки
/// нет и значение непарсящееся — громкие отказы (контракт #1235/#1236),
/// запись без верифицированного current запрещена. Оба чтения гарда —
/// wt и свежее (#1437) — отвечают одной семантикой.
fn verifiable_current(board: &str, number: u64, status: Option<Status>) -> UsecaseResult<Status> {
    match status {
        None => Err(UsecaseError::Board(format!(
            "row #{number} is not on board «{board}» (no status on the carrier)"
        ))),
        Some(Status::Empty) => Err(UsecaseError::Board(format!(
            "the carrier status of #{number} is unreadable — writing without a verified \
             current is forbidden (the class of the triaged bug #1235), owner's triage"
        ))),
        Some(current) => Ok(current),
    }
}

/// Точечная правка ячеек: None — не трогаем.
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub status: Option<Status>,
    pub branch: Option<String>,
    pub pr: Option<String>,
    pub priority: Option<String>,
    pub title: Option<String>,
    /// Автомерж on/off (#1232): до-пометка старых строк, выключение.
    pub automerge: Option<bool>,
    /// Тип задачи (#1271): нативный issue Type, PATCH {"type": имя}.
    /// None — не трогать (снос значения патчем не предусмотрен).
    pub kind: Option<TaskType>,
    /// Дословный ответ владельца (#1619): разрешает возврат из Needs
    /// attention в стадию-источник. Сам по себе не мутация.
    pub owner_answer: Option<String>,
}

impl TaskPatch {
    /// Патч без полей менять нечего — presentation отказывает громко.
    pub fn is_empty(&self) -> bool {
        matches!(
            self,
            Self {
                status: None,
                branch: None,
                pr: None,
                priority: None,
                title: None,
                automerge: None,
                kind: None,
                owner_answer: None,
            }
        )
    }
}

pub struct TaskWriteUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    /// Нативные поля issue (ветка/PR/приоритет/сессии).
    pub fields: Arc<dyn IssueFieldStore>,
    /// карта проект → доска (мультипроектный конфиг).
    pub boards: Vec<(String, String)>,
    /// Штамп времени для реестра серверных постов (GC по возрасту).
    pub clock: Arc<dyn Clock>,
    /// Реестр «своих» комментов (self-эхо-фильтр #1204): MCP-посты маркируются.
    pub state: Arc<dyn TickStateStore>,
}

impl TaskWriteUseCase {
    fn board_of(&self, project: &str) -> String {
        self.boards
            .iter()
            .find(|(p, _)| p == project)
            .map(|(_, b)| b.clone())
            .unwrap_or_default()
    }
}

impl TaskWriteUseCase {
    fn dup_check(&self, rows: &[Task], title: &str) -> UsecaseResult<()> {
        if rows.iter().any(|t| t.title() == title) {
            return Err(UsecaseError::Board(format!(
                "title «{title}» already exists on the board — duplicates forbidden"
            )));
        }
        Ok(())
    }

    /// Гейт полноты обязательных полей на выходе из Analys (#1280).
    /// Эффективное значение = патч, иначе носитель (одно-вызовное
    /// «заполни и перейди» легально); мусорный приоритет (не
    /// high/medium/low) = пустота — не явное решение. Отказ с перечнем
    /// возвращает строку на дозаполнение: не стоп-кран, владельца
    /// не спрашивает (ADR-074).
    async fn ensure_analys_exit_complete(
        &self,
        number: u64,
        patch: &TaskPatch,
    ) -> UsecaseResult<()> {
        // патч несёт оба значения и приоритет распознан — носитель не
        // читается: результат гейта известен, GitHub round-trip не нужен
        // (Low ревью #1280). Мусорный патч-приоритет ранний выход не
        // получает — идёт через общую ветку и отклоняется как пустота
        // (Common ревью-раунда 2: семантика гейта не зависит от формы
        // вызова; иначе «urgent» с automerge просачивался бы на носитель,
        // CUP ранжировал бы его ниже low)
        if let (Some(raw), Some(_)) = (patch.priority.as_deref(), patch.automerge) {
            if naming::parse_priority(raw) != uwuwu_domain::Priority::Empty {
                return Ok(());
            }
        }
        let carrier = self.fields.load(number).await?;
        let priority = match patch.priority.as_deref() {
            Some(raw) => naming::parse_priority(raw),
            None => carrier
                .get(naming::F_PRIORITY)
                .map(|raw| naming::parse_priority(raw))
                .unwrap_or(uwuwu_domain::Priority::Empty),
        };
        let mut empty: Vec<&str> = Vec::new();
        if priority == uwuwu_domain::Priority::Empty {
            empty.push(naming::F_PRIORITY);
        }
        let automerge_explicit =
            patch.automerge.is_some() || carrier.contains_key(naming::F_AUTOMERGE);
        if !automerge_explicit {
            empty.push(naming::F_AUTOMERGE);
        }
        if empty.is_empty() {
            return Ok(());
        }
        Err(UsecaseError::Board(format!(
            "Analys exit #{number}: required fields not set — {}; \
             fill them in (task_edit priority / automerge) and retry the transition",
            empty.join(", ")
        )))
    }
}

#[async_trait]
impl TaskWriter for TaskWriteUseCase {
    async fn create(&self, project: &str, spec: NewTask) -> UsecaseResult<u64> {
        if spec.title.trim().is_empty() {
            return Err(UsecaseError::Board("title is empty".into()));
        }
        let board = self.board_of(project);
        if board.is_empty() {
            return Err(UsecaseError::Board(format!(
                "project «{project}» is not in the config"
            )));
        }
        let rows = self.board.read_board(&board).await?;
        self.dup_check(&rows, spec.title.trim())?;
        // холодный ящик (#1293): закрытые строки покидают снапшот, и
        // снапшот-гард слепнет к старым тайтлам — точное равенство
        // добивается поиском issues (REST, вне GraphQL-бюджета):
        // инвариант «тайтл уникален» не сужается вместе с доской.
        // Транзиентный сбой поиска (Low-2 ревью) — fail-open с warn:
        // снапшот-гард остаётся первичным рубежом, create не валим
        let cold = match self.board.issues_with_title(spec.title.trim()).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(
                    event = "dup_check.search_failed",
                    title = %spec.title.trim(),
                    error = %e,
                    "the REST title search is unavailable — the cold guard is skipped (the primary snapshot guard already ran)"
                );
                Vec::new()
            }
        };
        if !cold.is_empty() {
            return Err(UsecaseError::Board(format!(
                "title «{}» is already taken by issue #{} — duplicates forbidden (the row is outside the snapshot: cold box)",
                spec.title.trim(),
                cold[0]
            )));
        }
        // direct (#1357) и fact_track (#1631) — взаимоисключающие классы:
        // direct минует Result review, fact_track его требует; строка не
        // может жить обоими пайпами сразу — отказ громкий, носитель чист
        if spec.direct && spec.fact_track {
            return Err(UsecaseError::Board(
                "direct and fact_track are mutually exclusive — a row cannot bypass and require Result review at once".into(),
            ));
        }
        let number = self
            .board
            .create_issue(
                &board,
                project,
                spec.title.trim(),
                &render_create_body(&spec),
                Some(naming::task_type_native_name(
                    spec.kind.unwrap_or(uwuwu_domain::TaskType::Feature),
                )),
            )
            .await?;
        // тип уехал в теле POST create (нативный issue Type, #1271):
        // атомарно, дрейф между issue и полем невозможен; тихий дроп
        // без push-доступа ловит сверка эха на границе носителя
        // Приоритет/Automerge/Direct/FactTrack — ячейки снапшота доски:
        // единая точка записи board-адаптера с кеш-гигиеной (#1258; add_to_board
        // инвалидирует ДО записи полей — окно кеша без полей закрывается
        // адаптером). Автомерж пишется всегда — состояние на носителе явно
        // (канон #1232); direct и fact_track тоже всегда (#1357, #1631:
        // on/off, не умолчание чтения).
        let priority = if spec.priority.is_empty() {
            // direct-строке поля разбора пишет create (развилка 2 плана):
            // пустой priority → medium (дефолт владельца 13.11); обычная
            // create-строка — None (гейт выхода из Analys требует явности)
            if spec.direct {
                Some("medium")
            } else {
                None
            }
        } else {
            Some(spec.priority.as_str())
        };
        self.board
            .set_board_cells(
                &board,
                number,
                priority,
                Some(spec.automerge),
                Some(spec.direct),
                Some(spec.fact_track),
            )
            .await?;
        // direct (#1357): строка рождается в In Progress — фабрика по ней
        // не спавнит (decide гасит спавн-ветку), работу ведёт сессия
        // поручения. Аналитик-гейт не ветвится: это не edit-переход из
        // Analys; окно Analys — запросы одного usecase-вызова (риски плана)
        if spec.direct {
            self.board
                .set_status(&board, number, Status::InProgress)
                .await?;
        }
        Ok(number)
    }

    async fn edit(
        &self,
        project: &str,
        number: u64,
        patch: TaskPatch,
    ) -> UsecaseResult<EditReceipt> {
        let board = self.board_of(project);
        if board.is_empty() {
            return Err(UsecaseError::Board(format!(
                "project «{project}» is not in the config"
            )));
        }
        let need_rows = patch.title.is_some();
        let rows = if need_rows {
            self.board.read_board(&board).await?
        } else {
            Vec::new()
        };
        // Ожидания контрольного чтения носителя (честная квитанция #1331):
        // после всех записей один read_issue сверяет всё изменённое.
        let mut want_title: Option<String> = None;
        let mut want_branch: Option<String> = None;
        let mut want_pr: Option<String> = None;
        let mut want_priority: Option<String> = None;
        let mut want_automerge: Option<bool> = None;
        let mut status_delivery: Option<StatusDelivery> = None;
        let mut owner_answer_ignored = false;
        if let Some(status) = patch.status {
            let answer = patch.owner_answer.as_deref();
            let (current, answer_used) = self.guard_canon(&board, number, status, answer).await?;
            owner_answer_ignored = answer.is_some() && !answer_used;
            // Гейт полноты полей на выходе из Analys (#1280): волна
            // аналитики обязана выпускать строки с явными Приоритетом
            // и Automerge — решение разбора, не пустота носителя.
            // Идемпотентная переустановка (current == status) — тоже выход
            // из Analys под гейтом; снос (Deleted) мимо — task_close
            // дубликатов полей не требует.
            if current == Status::Analys && status != Status::Deleted {
                self.ensure_analys_exit_complete(number, &patch).await?;
            }
            tracing::info!(
                event = "board.status_transition",
                number,
                from = %naming::status_name(current),
                to = %naming::status_name(status),
                "Гард канона пропустил переход"
            );
            status_delivery = Some(self.board.set_status(&board, number, status).await?);
        }
        if let Some(title) = &patch.title {
            if rows
                .iter()
                .any(|t| t.number() != number && t.title() == title)
            {
                return Err(UsecaseError::Board(format!(
                    "title «{title}» is already taken by another row"
                )));
            }
            // холодный ящик (#1293, Low дельта-раунда 2): rename-гард
            // сверяется и с закрытыми вне снапшота строками — инвариант
            // «тайтл уникален» единый с create (issues_with_title, REST);
            // сбой поиска — fail-open (первичный снапшот-гард отработал)
            let cold = match self.board.issues_with_title(title).await {
                Ok(found) => found,
                Err(e) => {
                    tracing::warn!(
                        event = "dup_check.search_failed",
                        title = %title,
                        error = %e,
                        "the REST title search is unavailable during rename — the cold guard is skipped"
                    );
                    Vec::new()
                }
            };
            if let Some(other) = cold.iter().find(|n| **n != number) {
                return Err(UsecaseError::Board(format!(
                    "title «{title}» is already taken by issue #{other} — duplicates forbidden (the row is outside the snapshot: cold box)"
                )));
            }
            self.board.rename_issue(number, title).await?;
            want_title = Some(title.clone());
        }
        // ветка/PR — нативные поля issue (в снапшоте не живут)
        let mut field_patch: Vec<(&str, Option<String>)> = Vec::new();
        if let Some(b) = &patch.branch {
            let name = uwuwu_domain::BranchName::parse(b).ok_or_else(|| {
                UsecaseError::Board(format!("branch «{b}» is invalid (spaces/glob characters)"))
            })?;
            field_patch.push(("Branch", Some(name.as_str().to_string())));
            want_branch = Some(name.as_str().to_string());
        }
        if let Some(p) = &patch.pr {
            let url = uwuwu_domain::PrUrl::parse(p)
                .ok_or_else(|| UsecaseError::Board(format!("PR «{p}» is not an http(s) URL")))?;
            field_patch.push(("PR", Some(url.as_str().to_string())));
            want_pr = Some(url.as_str().to_string());
        }
        if !field_patch.is_empty() {
            self.fields.store(number, &field_patch).await?;
        }
        // Приоритет/Automerge — ячейки снапшота доски: единая точка записи
        // board-адаптера с кеш-гигиеной (read-your-writes #1258); мимо неё
        // значения протухали в снапшоте до TTL (проба на проде 18.11)
        if patch.priority.is_some() || patch.automerge.is_some() {
            self.board
                .set_board_cells(
                    &board,
                    number,
                    patch.priority.as_deref(),
                    patch.automerge,
                    None,
                    None,
                )
                .await?;
            want_priority = patch.priority.clone();
            want_automerge = patch.automerge;
        }
        // тип — нативный issue Type (#1271): PATCH {"type": имя}, мимо
        // снапшота доски; контрольное чтение ниже сверяет парс носителя
        let mut want_kind: Option<uwuwu_domain::TaskType> = None;
        if let Some(kind) = patch.kind {
            self.board
                .set_issue_type(number, naming::task_type_native_name(kind))
                .await?;
            want_kind = Some(kind);
        }
        self.verify_edit_delivery(
            &board,
            number,
            EditExpectations {
                title: want_title,
                branch: want_branch,
                pr: want_pr,
                priority: want_priority,
                automerge: want_automerge,
                kind: want_kind,
            },
        )
        .await?;
        Ok(EditReceipt {
            status: status_delivery,
            owner_answer_ignored,
        })
    }

    async fn claim(&self, number: u64, session: &str, slot: Option<Slot>) -> UsecaseResult<()> {
        let resolved = slot.unwrap_or(uwuwu_domain::Slot::Dev);
        self.board.set_session("", number, resolved, session).await
    }

    async fn comment(&self, number: u64, text: &str, reply_to: Option<&str>) -> UsecaseResult<()> {
        if text.trim().is_empty() {
            return Err(UsecaseError::Board("the comment is empty".into()));
        }
        let body = match reply_to {
            Some(anchor) => format!("> {anchor}\n\n{text}"),
            None => text.to_string(),
        };
        let id = self.board.post_comment(number, &body).await?;
        // Пустой id — нестандартный ответ носителя: маркировка и
        // контрольное чтение невозможны, fail-open (возможна разовая
        // эхо-будка) — поведение маркировки сохранено (#1204).
        if id.is_empty() {
            tracing::warn!(
                event = "write.delivery_unverified",
                kind = "comment",
                number,
                "node_id пуст — верификация доставки невозможна (fail-open)"
            );
            return Ok(());
        }
        // Контрольное чтение ленты: коммент присутствует = факт доставки.
        let repo = self.board.clone();
        let needle = id.clone();
        match verify_carrier(
            self.clock.as_ref(),
            move || {
                let repo = repo.clone();
                async move { repo.latest_comments(number).await }
            },
            move |cs: &Vec<CommentRef>| cs.iter().any(|c| c.id == needle),
        )
        .await
        {
            Delivery::Verified(_) => {
                self.state.mark_own_comment(&id, self.clock.now_ms());
                Ok(())
            }
            Delivery::Stale => {
                tracing::error!(
                    event = "write.delivery_mismatch",
                    kind = "comment",
                    number,
                    "Пост коммента не подтвердился чтением ленты носителя"
                );
                Err(UsecaseError::Board(format!(
                    "comment #{number}: the post was not confirmed by reading the carrier feed — \
                     do not retry blindly (duplicate risk), verify via task_get"
                )))
            }
            Delivery::Blind => {
                tracing::warn!(
                    event = "write.delivery_unverified",
                    kind = "comment",
                    number,
                    "Чтение ленты носителя не удалось — мутация принята, маркировка сохранена"
                );
                self.state.mark_own_comment(&id, self.clock.now_ms());
                Ok(())
            }
        }
    }

    async fn close(&self, number: u64, not_planned: bool) -> UsecaseResult<()> {
        // доска строки резолвится по всему конфигу (мультиборд, #1135):
        // вердикт аналитика «дубль/мусор → close» обязан работать на любой
        // доске волны, не только на первой (пин живого прогона).
        // Поиск — row_exists (носитель), не read_board (#1236): close
        // не тянет GraphQL-снапшот при исчерпанном GraphQL-бакете.
        // Предикат присутствия строки, не читаемости статуса (ревью 1
        // Common-2): строка без Status-поля/в лаге закрывается как прежде.
        for (_, board) in &self.boards {
            if self.board.row_exists(board, number).await? {
                self.board.set_status(board, number, Status::Done).await?;
                return self.board.close_issue_ext(number, not_planned).await;
            }
        }
        Err(UsecaseError::Board(format!(
            "row #{number} is not on the configured boards"
        )))
    }

    async fn set_body(
        &self,
        project: &str,
        number: u64,
        body_md: Option<&str>,
    ) -> UsecaseResult<BodyWriteReceipt> {
        let board = self.board_of(project);
        // project вне конфига — громкий отказ (паритет task_body_section):
        // мусорный project не пишет «мимо доски» молча (ревью #1331 Low)
        if board.is_empty() {
            return Err(UsecaseError::Board(format!(
                "project «{project}» is not in the config"
            )));
        }
        // Гард пустой записи (#1572, сценарий 1): «параметр отсутствует» ≠
        // «явно пустая строка» (Option vs Some("")) — мутация без контента
        // молча стирала носитель (инстансы foilcap #31, 22–23.11: босый
        // «empty body» дважды прочитан знающим агентом как успех). Отказ
        // несёт длину текущего тела и рецепт хода (ADR-065: отказ с
        // рецептом вместо тихой деградации); явная пустая строка —
        // осознанная очистка (транспорт доносит её присутствующим
        // значением — живая проба 24.11, task_clean ack:"").
        let Some(input) = body_md else {
            let current = self.board.read_issue(&board, number).await?;
            return Err(UsecaseError::Board(format!(
                "body of #{number}: write refused — the `body` parameter is absent or not a \
                 string; task_body REPLACES the whole carrier body (write, not read; current \
                 body: {} bytes). Read the body via task_get; to clear deliberately pass \
                 body: \"\"",
                current.body.len()
            )));
        };
        // явная пустая/whitespace строка — осознанная очистка (до стрипа:
        // whitespace не даёт эффективного контента и не маскирует мусор);
        // пишется пустое тело, whitespace-мусор носителем не становится
        let explicit_clear = input.trim().is_empty();
        // инвариант на границе: тело без fm и без дисплей-шапки task_get
        // (агенты могут копировать старый канон — сервер держит «fm снесён»
        // сам, #1151; fm-стек инцидента #1331)
        let clean: String = if explicit_clear {
            String::new()
        } else {
            strip_comments_tail(crate::fm_lite::strip(strip_task_get_header(input)))
        };
        // Гард пустой записи судит по ЭФФЕКТИВНОМУ контенту — тому, что
        // уйдёт в replace_body (ревью #1572 Common-1): fm-блок или шапка
        // task_get непусты как строка, но strip срезает их в пустоту —
        // иначе стирание носителя под квитанцию «replaced; 0 bytes». Такой
        // вход — не осознанная очистка, а ошибка копирования: отказ с
        // рецептом (ADR-065), очистка — только явной пустой строкой
        // (задокументирована в дескрипторе, whitespace ей равна).
        if !explicit_clear && clean.trim().is_empty() {
            let current = self.board.read_issue(&board, number).await?;
            return Err(UsecaseError::Board(format!(
                "body of #{number}: write refused — the `body` parameter carries no effective \
                 content (only fm/task_get-header wrapping or a comments tail from a copied \
                 card, stripped on the boundary); \
                 task_body REPLACES the whole carrier body (write, not read; current body: \
                 {} bytes). Read the body via task_get; to clear deliberately pass \
                 body: \"\"",
                current.body.len()
            )));
        }
        self.board.replace_body(number, &clean).await?;
        // контрольное чтение носителя: квитанция = факт доставки (#1331)
        let repo = self.board.clone();
        let bd = board.clone();
        let expected = clean.to_string();
        // future владеет копиями захваченного — ссылки из тела FnMut-
        // замыкания не ускользают (lifetime-гард компилятора)
        match verify_carrier(
            self.clock.as_ref(),
            move || {
                let repo = repo.clone();
                let bd = bd.clone();
                async move { repo.read_issue(&bd, number).await }
            },
            move |v: &crate::ports::IssueView| v.body == expected,
        )
        .await
        {
            Delivery::Verified(view) => Ok(BodyWriteReceipt {
                body_hash: md_section::body_hash(&view.body),
                body_size: view.body.len(),
                verified: true,
            }),
            Delivery::Stale => {
                tracing::error!(
                    event = "write.delivery_mismatch",
                    kind = "body",
                    number,
                    "Запись тела не подтвердилась контрольным чтением носителя"
                );
                Err(UsecaseError::Board(format!(
                    "body #{number}: the write was not confirmed by reading the carrier — \
                     GitHub returns something other than what was written; do not retry blindly, verify via task_get"
                )))
            }
            Delivery::Blind => {
                tracing::warn!(
                    event = "write.delivery_unverified",
                    kind = "body",
                    number,
                    "Контрольное чтение носителя не удалось — квитанция от отправленного тела"
                );
                Ok(BodyWriteReceipt {
                    body_hash: md_section::body_hash(&clean),
                    body_size: clean.len(),
                    verified: false,
                })
            }
        }
    }

    async fn set_body_section(
        &self,
        project: &str,
        number: u64,
        write: SectionWrite<'_>,
    ) -> UsecaseResult<SectionEditReport> {
        let SectionWrite {
            section,
            content,
            base_hash,
            confirm,
        } = write;
        if section.trim().is_empty() {
            return Err(UsecaseError::Board(
                "the section is empty — provide the heading".into(),
            ));
        }
        if base_hash.trim().is_empty() {
            return Err(UsecaseError::Board(
                "base_hash is required (body_hash from the fm of task_get)".into(),
            ));
        }
        let board = self.board_of(project);
        if board.is_empty() {
            return Err(UsecaseError::Board(format!(
                "project «{project}» is not in the config"
            )));
        }
        let current = self.board.read_issue(&board, number).await?;
        let actual = md_section::body_hash(&current.body);
        if actual != base_hash {
            return Err(UsecaseError::Board(format!(
                "body of #{number} changed since the read (stale base_hash); \
                 current body_hash: {actual} — re-read task_get and retry"
            )));
        }
        // Гард пустой записи (#1572, сценарий 3): content отсутствует —
        // громкий отказ с мерой текущей секции и рецептом; тихая деградация
        // до очистки стирала секции под квитанции «cleared» (кластеры
        // 23–24.11: find/replace молча strip'нулись).
        let Some(content) = content else {
            let now = match md_section::section_content_len(&current.body, section) {
                Some(was) => format!(
                    "section «{section}» carries {was} bytes now (namesake duplicates \
                     included in the measure)"
                ),
                None => format!("section «{section}» is not in the body"),
            };
            return Err(UsecaseError::Board(format!(
                "section «{section}» of #{number}: write refused — the `content` parameter is \
                 absent or not a string; task_body_section REPLACES the section content \
                 (write, not read; {now}). Read via task_get; to clear deliberately pass \
                 content: \"\""
            )));
        };
        let (next, edit) = md_section::replace_md_section(&current.body, section, content)
            .map_err(UsecaseError::Board)?;
        // Гард радикального сжатия (#1409-класс, #1572 срез 3): секционная
        // дельта не срезает треть носителя / большую секцию; ошибочный
        // короткий контент стирал отчёты (инстанс #1277, 19.11: 19К → 1).
        // Явная очистка (пустой/whitespace content) — осознанный акт, гард
        // не трогает (рецепт отказа полного task_body на неё опирается);
        // append растит тело — мера секции 0, оба плеча молчат.
        let body_was = current.body.len();
        let explicit_clear = content.trim().is_empty();
        let section_was = match edit {
            md_section::SectionEdit::Replaced {
                old_content_len, ..
            } => old_content_len,
            md_section::SectionEdit::Appended => 0,
        };
        // «стало» — той же мерой, что «было» (байты строк без переводов,
        // ревью #1572 Low): числа квитанции сопоставимы
        let section_now = md_section::section_content_metric(content);
        let confirmed = if explicit_clear {
            false
        } else {
            shrink_guard(
                section,
                number,
                ShrinkMeasures {
                    body_was,
                    body_now: next.len(),
                    section_was,
                    section_now,
                    confirm,
                },
            )?
        };
        let receipt = self.set_body(project, number, Some(&next)).await?;
        let (action, duplicates_removed) = match edit {
            md_section::SectionEdit::Replaced {
                duplicates_removed, ..
            } => (SectionAction::Replaced, duplicates_removed),
            md_section::SectionEdit::Appended => (SectionAction::Appended, 0),
        };
        Ok(SectionEditReport {
            cleared: action == SectionAction::Replaced && explicit_clear,
            action,
            duplicates_removed,
            body_was,
            body_size: receipt.body_size,
            section_was,
            section_now,
            confirmed,
            body_hash: receipt.body_hash,
            verified: receipt.verified,
        })
    }
}

/// Меры гарда сжатия: объёмы было/стало по телу и по секции + флаг
/// осознанного преодоления (#1409/#1572).
struct ShrinkMeasures {
    body_was: usize,
    body_now: usize,
    section_was: usize,
    section_now: usize,
    confirm: bool,
}

/// Гард радикального сжатия (#1409-класс, #1572 срез 3): Err — громкий
/// отказ с длинами было→стало и рецептом; Ok(true) — осознанное
/// сокращение проведено (confirm); Ok(false) — гард молчит (дельта в
/// норме, либо плечи не сработали: секция ниже пола, append). Причина
/// отказа называет фактическое плечо (ревью #1572 Low): правило тела и
/// правило 1 KiB-секции — разные правила, текст не путает.
fn shrink_guard(section: &str, number: u64, m: ShrinkMeasures) -> UsecaseResult<bool> {
    let body_shrunk = radical_shrink(m.body_was, m.body_now);
    let section_shrunk =
        m.section_was >= SECTION_GUARD_FLOOR && radical_shrink(m.section_was, m.section_now);
    if !body_shrunk && !section_shrunk {
        return Ok(false);
    }
    if m.confirm {
        return Ok(true);
    }
    let reason = match (body_shrunk, section_shrunk) {
        (true, true) => {
            "the body lost a third of its size and a 1 KiB+ section was cut by \
                         70%+"
        }
        (true, false) => "the body lost a third of its size",
        (false, true) => "a 1 KiB+ section was cut by 70%+",
        (false, false) => unreachable!("заход в отказ без сработавшего плеча"),
    };
    Err(UsecaseError::Board(format!(
        "section edit «{section}» of #{number} refused: radical shrink ({reason}) — \
         body {} → {} bytes, section {} → {} bytes; check the content, deliberate shrink — \
         repeat with confirm: true",
        m.body_was, m.body_now, m.section_was, m.section_now
    )))
}

/// Порог радикального сжатия (#1409, развилка А): новое < 70% старого —
/// разовая секционная дельта не должна срезать треть носителя (инстанс
/// #1277: −40%); целочисленная арифметика вместо плавающей.
fn radical_shrink(old: usize, new: usize) -> bool {
    new * 10 < old * 7
}

/// Пол секции для секционного плеча гарда сжатия (#1572 срез 3): секции
/// меньше пола полностью редактируемы без confirm — короткая секция не
/// несёт потерь data-loss-класса, чистое соотношение стреляло бы по
/// каждой правке однострочника. Тело-плечо без пола: срез трети носителя
/// подозрителен при любом размере (одна константа #1409).
const SECTION_GUARD_FLOOR: usize = 1024;

/// Контрольное чтение носителя после успешной мутации (честная квитанция
/// #1331, ADR-093): мутация отвечает успехом только как факт доставки.
/// В аномальных окнах носителя (18.44–18.52Z 18.11) эхо-PATCH проходило
/// при неприменившейся записи — контрольное чтение ловит то, что эхо
/// пропускает; сильнее на Issues API ничего нет (If-Match не поддерживается).
const VERIFY_ATTEMPTS: usize = 3;
const VERIFY_PAUSE_MS: u64 = 1000;

enum Delivery<T> {
    /// Носитель прочитан и содержит ожидаемое — факт доставки.
    Verified(T),
    /// Носитель читается, но ожидаемого нет ни в одной из попыток —
    /// фантом либо экстремальный лаг; квитанция громко отказывает
    /// (агенту: не ретраить слепо — сверить task_get).
    Stale,
    /// Носитель не прочитан ни разу (сеть, GraphQL-бэкофф) — верификация
    /// невозможна; квитанция строится от отправленного + warning
    /// (канон ADR-093: применённую мутацию не ретраят).
    Blind,
}

/// До VERIFY_ATTEMPTS попыток чтения носителя с паузой VERIFY_PAUSE_MS
/// между ними (план-контракт 3×1с, прецедент verify_status): пауза
/// смягчает ложные Stale-отказы на секундном лаге видимости носителя —
/// минутные аномальные окна инцидента #1331 не покрываются ни паузой,
/// ни ретраями, но и не осложняются ими. Последняя попытка решает исход:
/// Ok-мимо — Stale, Err — Blind.
async fn verify_carrier<T, Fut, F, M>(clock: &dyn Clock, mut read: F, matches: M) -> Delivery<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, UsecaseError>>,
    M: Fn(&T) -> bool,
{
    let mut stale = false;
    for attempt in 0..VERIFY_ATTEMPTS {
        if attempt > 0 {
            clock.sleep_ms(VERIFY_PAUSE_MS);
        }
        match read().await {
            Ok(v) if matches(&v) => return Delivery::Verified(v),
            Ok(_) => stale = true,
            Err(_) => stale = false,
        }
    }
    if stale {
        Delivery::Stale
    } else {
        Delivery::Blind
    }
}

/// Хвост ленты комментов из полной копии карточки `task_get`.
///
/// Граница записи держит инвариант ADR-105: тело не несёт композит чтения.
/// Срез ограничен записями рендера, поэтому секции, дописанные после дампа,
/// остаются на месте. Возвращает владельца: префикс и сохранённый хвост
/// не образуют одного заимствованного среза.
fn strip_comments_tail(input: &str) -> String {
    let mut current = input.to_string();
    while let Some((from, to)) = comments_tail_cut(&current) {
        current = format!("{}{}", &current[..from], &current[to..]);
    }
    current
}

/// Диапазон байт одной распознанной ленты: от пары переводов перед маркером
/// до конца последней записи (или до пустой строки перед H2).
fn comments_tail_cut(input: &str) -> Option<(usize, usize)> {
    let mut lines = Vec::new();
    let mut at = 0;
    for piece in input.split_inclusive('\n') {
        let body = piece.strip_suffix('\n').unwrap_or(piece);
        let body = body.strip_suffix('\r').unwrap_or(body);
        lines.push((at, body));
        at += piece.len();
    }

    let mut fence = false;
    for index in 0..lines.len() {
        let (start, line) = lines[index];
        if !fence && line.starts_with("--- comments (latest") && line.ends_with(") ---") {
            if let Some(end) = comments_tail_end(input.len(), &lines, index + 1) {
                let from = if input[..start].ends_with("\r\n\r\n") {
                    start - 4
                } else if input[..start].ends_with("\n\n") {
                    start - 2
                } else {
                    start
                };
                return Some((from, end));
            }
        }
        if md_section::toggles_fence(line) {
            fence = !fence;
        }
    }
    None
}

/// Конец ленты: последняя запись идёт до конца входа или до пустой строки
/// перед H2. Состояние блока кода не наследуется от предыдущего коммента.
fn comments_tail_end(len: usize, lines: &[(usize, &str)], from: usize) -> Option<usize> {
    let mut index = from;
    let mut found = false;
    while entry_starts_at(lines, index) {
        found = true;
        if let Some(end) = scan_entry(lines, index + 2) {
            return Some(end);
        }
        index += 2;
        while index < lines.len() && !entry_starts_at(lines, index) {
            index += 1;
        }
    }
    if found {
        Some(len)
    } else {
        None
    }
}

/// Тело записи заканчивается на пустой строке перед H2 вне блока кода.
fn scan_entry(lines: &[(usize, &str)], from: usize) -> Option<usize> {
    let mut fence = false;
    for index in from..lines.len() {
        let line = lines[index].1;
        if section_starts_at(lines, index, fence) {
            return Some(lines[index].0);
        }
        if entry_starts_at(lines, index) {
            return None;
        }
        if md_section::toggles_fence(line) {
            fence = !fence;
        }
    }
    None
}

fn entry_starts_at(lines: &[(usize, &str)], index: usize) -> bool {
    index + 1 < lines.len() && lines[index].1 == "---" && is_comment_stamp(lines[index + 1].1)
}

fn section_starts_at(lines: &[(usize, &str)], index: usize, fence: bool) -> bool {
    index + 1 < lines.len()
        && !fence
        && lines[index].1.is_empty()
        && lines[index + 1].1.starts_with("## ")
}

/// Штамп коммента рендера: `YYYY-MM-DDTHH:MM:SSZ` (UTC).
fn is_comment_stamp(line: &str) -> bool {
    let bytes = line.as_bytes();
    if bytes.len() != 20 {
        return false;
    }
    let separators = matches!(
        (bytes[4], bytes[7], bytes[10], bytes[13], bytes[16], bytes[19]),
        (b'-', b'-', b'T', b':', b':', b'Z')
    );
    if !separators {
        return false;
    }
    [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
        .iter()
        .all(|&i| bytes[i].is_ascii_digit())
}

/// Дисплей-шапка task_get — рендер MCP-карточки, не носитель: агенты
/// копируют вывод task_get назад в task_body, и шапка наслаивалась в
/// теле (fm-стек инцидента #1331, инстансы #1526). Состав рендера
/// (api/tools.rs task_get): тайтл-строка «#N title — status», пустая
/// строка, fm-блок с ключами одного из рендеров ниже. Срезается на
/// границе записи: инвариант ADR-105 «тело — чистый markdown» держит
/// сервер, не дисциплина агента (прецедент #1151). Консервативно:
/// строго набор/порядок ключей и форма карточки; срезаются все
/// подряд идущие слои (стек, #1632); любой посторонний контент
/// («--- fm ---» внутри текста) не трогается. Зеркало рендера: рендер
/// меняется — менять формы здесь; пин в tools_tests собран из
/// реального рендера и краснеет при расхождении (#1604 — body_bytes;
/// #1631 — fact_track).
fn strip_task_get_header(md: &str) -> &str {
    // Актуальный рендер (#1604+#1631): branch/pr/automerge/direct/
    // fact_track/type/body_hash/body_bytes
    const KEYS: [&str; 8] = [
        "branch: ",
        "pr: ",
        "automerge: ",
        "direct: ",
        "fact_track: ",
        "type: ",
        "body_hash: ",
        "body_bytes: ",
    ];
    // Легаси-рендер #1631..#1604: fact_track без body_bytes
    const LEGACY_KEYS: [&str; 7] = [
        "branch: ",
        "pr: ",
        "automerge: ",
        "direct: ",
        "fact_track: ",
        "type: ",
        "body_hash: ",
    ];
    // Легаси-рендер #1271..#1631: type без fact_track/body_bytes
    const LEGACY_KEYS_6: [&str; 6] = [
        "branch: ",
        "pr: ",
        "automerge: ",
        "direct: ",
        "type: ",
        "body_hash: ",
    ];
    // Легаси-рендер до #1271: без type (старые выводы копируются
    // агентами из старых сессий и статей опыта — срезаются тоже)
    const LEGACY_KEYS_NO_TYPE: [&str; 5] =
        ["branch: ", "pr: ", "automerge: ", "direct: ", "body_hash: "];
    let mut pos = md;
    while let Some(rest) = strip_header_layer(
        pos,
        &[
            &KEYS[..],
            &LEGACY_KEYS[..],
            &LEGACY_KEYS_6[..],
            &LEGACY_KEYS_NO_TYPE[..],
        ],
    ) {
        pos = rest;
    }
    pos
}

/// Один слой шапки в начале входа: fm-блок (форма A) либо полная
/// карточка рендера — тайтл-строка + пустая строка + fm-блок (форма B,
/// #1632). Ключи сверяются со всеми наборами рендеров (актуальный +
/// легаси). None — не слой шапки, вход не трогается.
fn strip_header_layer<'a>(md: &'a str, key_sets: &[&[&str]]) -> Option<&'a str> {
    if let Some(rest) = strip_fm_block(md, key_sets) {
        return Some(rest);
    }
    let (title_line, after_title) = md.split_once('\n')?;
    if !is_render_title_line(title_line) {
        return None;
    }
    let after_blank = after_title.strip_prefix('\n')?;
    strip_fm_block(after_blank, key_sets)
}

/// fm-блок рендера: маркер + ключи одного из наборов строго по
/// порядку; возврат — вход после шапки без ведущих пустых строк.
fn strip_fm_block<'a>(md: &'a str, key_sets: &[&[&str]]) -> Option<&'a str> {
    let rest = md.strip_prefix("--- fm ---\n")?;
    key_sets
        .iter()
        .find_map(|keys| strip_header_keys(rest, keys))
}

/// Строгое совпадение набора/порядка ключей одного рендера; частичные
/// шапки и посторонний контент — None. У последнего ключа хвостовой
/// перенос не обязателен: копия шапки проходит через .trim() скрипта
/// потребителя — без этого KEYS (актуальная форма) не матчится, а
/// короткая легаси-форма матчится на префиксе, и остаток последней
/// строки («body_bytes: N») уезжает эффективным контентом (#1604,
/// ревью раунд 2).
fn strip_header_keys<'a>(rest: &'a str, keys: &[&str]) -> Option<&'a str> {
    let mut pos = rest;
    for (index, key) in keys.iter().enumerate() {
        let line_end = match pos.find('\n') {
            Some(at) => at,
            None if index + 1 == keys.len() => pos.len(),
            None => return None,
        };
        if !pos[..line_end].starts_with(key) {
            return None;
        }
        pos = pos[line_end..].strip_prefix('\n').unwrap_or("");
    }
    Some(pos.trim_start_matches('\n'))
}

/// Тайтл-строка рендера карточки: «#<номер> <тайтл> — <статус>» —
/// решётка, цифры, пробел; разделитель « — » из рендера (статусы не
/// захардкожены — дрейф доски гард не ломает).
fn is_render_title_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('#') else {
        return false;
    };
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && rest[digits..].starts_with(' ') && line.contains(" — ")
}

/// Ожидания контрольного чтения edit: Some — значение менялось и должно
/// присутствовать на носителе; None — не менялось и не сверяется.
#[derive(Clone)]
struct EditExpectations {
    title: Option<String>,
    branch: Option<String>,
    pr: Option<String>,
    priority: Option<String>,
    automerge: Option<bool>,
    kind: Option<TaskType>,
}

impl EditExpectations {
    fn is_empty(&self) -> bool {
        matches!(
            (
                &self.title,
                &self.branch,
                &self.pr,
                &self.priority,
                self.automerge,
                self.kind,
            ),
            (None, None, None, None, None, None)
        )
    }

    /// Несошедшие ключи последнего чтения — в текст громкой ошибки.
    fn mismatches(&self, view: &crate::ports::IssueView) -> Vec<&'static str> {
        let mut out = Vec::new();
        if let Some(t) = &self.title {
            if view.task.title() != t {
                out.push("title");
            }
        }
        if let Some(b) = &self.branch {
            if view.task.branch().map(|x| x.as_str()) != Some(b.as_str()) {
                out.push("branch");
            }
        }
        if let Some(u) = &self.pr {
            if view.task.pr().map(|x| x.as_str()) != Some(u.as_str()) {
                out.push("PR");
            }
        }
        if let Some(p) = &self.priority {
            if view.task.priority() != crate::naming::parse_priority(p) {
                out.push("priority");
            }
        }
        if let Some(a) = self.automerge {
            if view.task.automerge() != a {
                out.push("automerge");
            }
        }
        if self.kind.is_some_and(|k| view.task.kind() != Some(k)) {
            out.push("type");
        }
        out
    }
}

impl TaskWriteUseCase {
    /// Чтение current для канон-гарда edit(): wt-чтение носителя (свежее
    /// против снапшота, #1235). Идемпотентная переустановка (current ==
    /// target) доверяет только свежему носителю (#1437, симптом 2) —
    /// стейл-писец (повтор старого вердикта пробуждённой сессией)
    /// разваливается на fresh-риде. Нечитаемый кеш-вид (строки нет в
    /// лагающем REST-листе — свежесозданная строка, или поле пусто)
    /// различается fresh-ридом: носитель читается — ребро идёт по нему,
    /// отказ по читаемости честен (Low дельта-раунда #1615: прежний
    /// «row #N is not on board» винил носитель за кеш-миссу). Кеш-вид,
    /// отличный от цели, возвращается как Cached — свежую сверку на
    /// отказной ветке делает guard_canon. Громкие отказы None/Empty —
    /// только при подтверждённом fresh (verifiable_current,
    /// контракт #1235/#1236).
    async fn guard_current(
        &self,
        board: &str,
        number: u64,
        target: Status,
    ) -> UsecaseResult<GuardView> {
        let cached = match self.board.row_status(board, number).await? {
            Some(s) if s != Status::Empty => s,
            _ => {
                return Ok(GuardView::Fresh(verifiable_current(
                    board,
                    number,
                    self.board.fresh_row_status(board, number).await?,
                )?));
            }
        };
        if cached != target {
            return Ok(GuardView::Cached(cached));
        }
        Ok(GuardView::Fresh(verifiable_current(
            board,
            number,
            self.board.fresh_row_status(board, number).await?,
        )?))
    }

    /// Гард канона перехода edit(): попытка по кеш-виду (wt-щит #1346,
    /// fast path — fresh-рид не тратится на легальные переходы);
    /// грядущий отказ сверяется со свежим носителем (#1615): кеш-вид за
    /// пределами wt-окна мог отстать от носителя — REST items-индекс
    /// пере-фетчится внутри wt-окна и при read-лаге листа минутного
    /// масштаба несёт до-записный статус ещё минуты (инстанс #1612:
    /// честный носитель «Review Analys», кеш показывал «Analys»,
    /// легальное ребро Review Analys → Todo трижды умерло отказом
    /// «вне канона», вердикт ревьювера висел в воздухе). Отказ
    /// конвейерной стадии дорог (ложная эскалация живой строки).
    /// Квотная дисциплина ADR-113/#1437: точечный GraphQL-рид только
    /// на редких ветках — отказная, идемпотентная, нечитаемый кеш-вид
    /// (один рид на вызов, без повторов).
    async fn guard_canon(
        &self,
        board: &str,
        number: u64,
        status: Status,
        owner_answer: Option<&str>,
    ) -> UsecaseResult<(Status, bool)> {
        // bool — ответ владельца реально понадобился для ребра; квитанция
        // обязана отличать его от ключа, переданного по привычке (#1619).
        match self.guard_current(board, number, status).await? {
            // Свежий носитель уже оплачен (рид #1437/нечитаемый кеш-вид):
            // повторной fresh-сверки нет — канон-проверка по нему
            GuardView::Fresh(current) => {
                let (direct, fact_track) = self.carrier_flags(number, current, status).await;
                if uwuwu_domain::Status::can_transition_with(current, status, direct, fact_track) {
                    return Ok((current, false));
                }
                if uwuwu_domain::Status::can_transition_with_answer(
                    current,
                    status,
                    direct,
                    fact_track,
                    owner_answer,
                ) {
                    return Ok((current, true));
                }
                // отказ: кеш-вида как источника здесь нет (рид был свежим
                // или кеш нечитаем) — расхождению докладывать нечего
                Err(Self::reject_canon(
                    number,
                    current,
                    None,
                    status,
                    owner_answer,
                ))
            }
            GuardView::Cached(cached) => {
                let (direct, fact_track) = self.carrier_flags(number, cached, status).await;
                if uwuwu_domain::Status::can_transition_with(cached, status, direct, fact_track) {
                    return Ok((cached, false));
                }
                if uwuwu_domain::Status::can_transition_with_answer(
                    cached,
                    status,
                    direct,
                    fact_track,
                    owner_answer,
                ) {
                    return Ok((cached, true));
                }
                let fresh = verifiable_current(
                    board,
                    number,
                    self.board.fresh_row_status(board, number).await?,
                )?;
                let (direct, fact_track) = self.carrier_flags(number, fresh, status).await;
                let answer_used =
                    !uwuwu_domain::Status::can_transition_with(fresh, status, direct, fact_track)
                        && uwuwu_domain::Status::can_transition_with_answer(
                            fresh,
                            status,
                            direct,
                            fact_track,
                            owner_answer,
                        );
                if !uwuwu_domain::Status::can_transition_with_answer(
                    fresh,
                    status,
                    direct,
                    fact_track,
                    owner_answer,
                ) {
                    return Err(Self::reject_canon(
                        number,
                        fresh,
                        Some(cached),
                        status,
                        owner_answer,
                    ));
                }
                tracing::warn!(
                    event = "board.guard_stale_cache",
                    number,
                    cached = %naming::status_name(cached),
                    fresh = %naming::status_name(fresh),
                    "Кеш-вид гарда отстал от носителя — переход проведён по свежему чтению (#1615)"
                );
                Ok((fresh, answer_used))
            }
        }
    }

    /// Честный отказ канона: текст по свежему значению носителя, а не по
    /// кеш-виду; расхождение кеш-вида — в событие лога (догфуд, Low
    /// дельта-раунда #1615: в квадранте «расходятся, носитель тоже
    /// запрещает» кеш-видимое значение было невидимо).
    fn reject_canon(
        number: u64,
        fresh: Status,
        cached: Option<Status>,
        to: Status,
        owner_answer: Option<&str>,
    ) -> UsecaseError {
        match cached {
            Some(cached) => tracing::warn!(
                event = "board.guard_rejected",
                number,
                cached = %naming::status_name(cached),
                from = %naming::status_name(fresh),
                to = %naming::status_name(to),
                "Гард канона: переход запрещён (свежий носитель подтвердил отказ)"
            ),
            None => tracing::warn!(
                event = "board.guard_rejected",
                number,
                from = %naming::status_name(fresh),
                to = %naming::status_name(to),
                "Гард канона: переход запрещён (свежий носитель подтвердил отказ)"
            ),
        }
        let hint = if fresh == Status::NeedsAttention
            && !uwuwu_domain::Status::can_transition(fresh, to)
            && owner_answer.is_none_or(|answer| answer.trim().is_empty())
        {
            // #1619: самообучающий отказ доезжает до живого агента, даже
            // если его сессия заспавнена до деплоя новой клаузы промпта.
            "; Needs attention → source stage requires owner_answer (the verbatim owner reply, fixed in the body/thread first)"
        } else {
            ""
        };
        UsecaseError::Board(format!(
            "transition {:?} → {:?} outside the pipeline canon{}",
            fresh, to, hint
        ))
    }

    /// Флаги классов строк с носителя (#1357, #1631): direct гвардит
    /// ребро In Progress → Review, fact_track — ReviewResult →
    /// UserReview и запреты тест-контура. На этих рёбрах (и только на
    /// них) флаги читаются свежим fields.load, на остальных (false,
    /// false). Отдельный метод: edit не несёт редкой читающей ветки
    /// (smell-гигиена дельты).
    async fn carrier_flags(&self, number: u64, current: Status, to: Status) -> (bool, bool) {
        use Status as S;
        let relevant = matches!(
            (current, to),
            (S::InProgress, S::UserReview)
                | (S::ReviewResult, S::UserReview)
                | (S::InProgress, S::Testing)
                | (S::ReviewResult, S::Testing)
        );
        if !relevant {
            return (false, false);
        }
        match self.fields.load(number).await {
            Ok(fields) => (
                fields
                    .get(naming::F_DIRECT)
                    .map(|v| v == "on")
                    .unwrap_or(false),
                fields
                    .get(naming::F_FACT_TRACK)
                    .map(|v| v == "on")
                    .unwrap_or(false),
            ),
            // чтение полей упало — флаги неизвестны: классовые рёбра
            // закрыты (консервативно), отказ громкий на следующем
            // гарде канона
            Err(e) => {
                tracing::warn!(
                    event = "board.class_flags_unreadable",
                    number,
                    error = %e,
                    "Флаги классов не прочитаны — классовые рёбра закрыты"
                );
                (false, false)
            }
        }
    }

    /// Контрольное чтение носителя после успешного edit (#1331): одно
    /// read_issue сверяет все записанные значения (тайтл/ветка/PR/приоритет/
    /// automerge). Stale — громкая ошибка с перечнем несошедших; Blind —
    /// warning (канон ADR-093: применённую мутацию не ретраят).
    async fn verify_edit_delivery(
        &self,
        board: &str,
        number: u64,
        want: EditExpectations,
    ) -> UsecaseResult<()> {
        if want.is_empty() {
            return Ok(());
        }
        let repo = self.board.clone();
        let bd = board.to_string();
        let matches_want = want.clone();
        let matches = move |v: &crate::ports::IssueView| matches_want.mismatches(v).is_empty();
        match verify_carrier(
            self.clock.as_ref(),
            move || {
                let repo = repo.clone();
                let bd = bd.clone();
                async move { repo.read_issue(&bd, number).await }
            },
            matches,
        )
        .await
        {
            Delivery::Verified(_) => Ok(()),
            Delivery::Stale => {
                // перечень несошедших — контрольное перечитывание носителя:
                // редкая ветка, точный текст громкой ошибки стоит чтения
                let detail = match self.board.read_issue(board, number).await {
                    Ok(view) => {
                        let m = want.mismatches(&view);
                        if m.is_empty() {
                            String::new()
                        } else {
                            format!("mismatched: {} — ", m.join(", "))
                        }
                    }
                    Err(_) => String::new(),
                };
                tracing::error!(
                    event = "write.delivery_mismatch",
                    kind = "edit",
                    number,
                    "Запись edit не подтвердилась контрольным чтением носителя"
                );
                Err(UsecaseError::Board(format!(
                    "edit #{number}: the write was not confirmed by reading the carrier ({detail}\
                     title/branch/PR/priority/automerge) — do not retry blindly, verify via task_get"
                )))
            }
            Delivery::Blind => {
                tracing::warn!(
                    event = "write.delivery_unverified",
                    kind = "edit",
                    number,
                    "Контрольное чтение носителя не удалось — значения не верифицированы"
                );
                Ok(())
            }
        }
    }
}

/// Тело новой задачи: чистый markdown (fm снесён; приоритет/тип — нативные поля).
/// Инвариант держит граница: fm-шапку (агенты копируют старый канон)
/// вырезаем из любого входа — тело не несёт fm независимо от дисциплины
/// вызывающего (инцидент #1151: дример завёл задачу с fm в теле).
fn render_create_body(spec: &NewTask) -> String {
    let safe_title: String = spec.title.chars().take(120).collect();
    let content = if spec.body_md.is_empty() {
        safe_title
    } else {
        // fm и дисплей-шапка task_get — на границе (инвариант сервера,
        // #1151; fm-стек инцидента #1331)
        strip_comments_tail(crate::fm_lite::strip(strip_task_get_header(&spec.body_md)))
    };
    // тело уже в канон-форме (агенты шлют «## Постановка…») — не дублируем
    // H2 (инцидент #1149: скелет prepend'ался к готовому телу)
    if content.trim_start().starts_with('#') {
        format!("{content}\n")
    } else {
        format!("## Постановка\n\n{content}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{CommentRef, FmMutator, IssueView};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn spec(body: &str) -> NewTask {
        NewTask {
            title: "t".into(),
            body_md: body.into(),
            kind: None,
            priority: String::new(),
            automerge: true,
            direct: false,
            fact_track: false,
        }
    }

    /// Мок доски: доска пуста, issue создаётся с номером 1; вызовы
    /// set_board_cells логируются — юзкейс обязан маршрутизировать
    /// Приоритет/Automerge в board-порт (read-your-writes #1258),
    /// а не в fields.store (там эти ячейки снапшота протухали).
    /// Носитель (carrier) — честный read-after-write: мутации обновляют,
    /// read_issue/latest_comments читают записанное; phantom — режим
    /// «мутация отвечает успехом, носитель не меняется» (инцидент #1331,
    /// красный тест дыры контракта); read_err — носитель не читается
    /// (Blind-ветка, ADR-093).
    #[derive(Default)]
    struct FakeBoard {
        cells: std::sync::Mutex<Vec<CellWrite>>,
        /// Номера, которые issues_with_title «находит» в репо — вход
        /// холодного ящика (#1293): create с таким тайтлом обязан падать.
        cold_hits: Vec<u64>,
        /// Транзиентный сбой REST-поиска (Low-2 ревью): create fail-open.
        search_err: bool,
        carrier: std::sync::Mutex<Carrier>,
        phantom: bool,
        read_err: bool,
    }

    /// Носитель контрольного чтения фейка: мутации пишут, чтения отдают.
    /// Default вручную: Status без derive Default — Empty как «не ставился».
    #[derive(Clone)]
    struct Carrier {
        title: String,
        body: String,
        comments: Vec<CommentRef>,
        branch: Option<String>,
        pr: Option<String>,
        priority: Option<String>,
        automerge: bool,
        direct: bool,
        fact_track: bool,
        kind: Option<TaskType>,
        status: Status,
    }

    impl Default for Carrier {
        fn default() -> Self {
            Self {
                title: String::new(),
                body: String::new(),
                comments: Vec::new(),
                branch: None,
                pr: None,
                priority: None,
                automerge: false,
                direct: false,
                fact_track: false,
                kind: None,
                status: Status::Empty,
            }
        }
    }

    impl Carrier {
        /// Агрегат для IssueView: поля собираются так же, как адаптер
        /// собирает их из issue-носителя (map_task_fields-семантика).
        fn view(&self, number: u64) -> IssueView {
            let mut task = Task::new(number, self.title.clone(), self.status);
            let branch = self
                .branch
                .as_deref()
                .and_then(uwuwu_domain::BranchName::parse);
            let pr = self.pr.as_deref().and_then(uwuwu_domain::PrUrl::parse);
            let vcs = match (branch, pr) {
                (Some(branch), Some(url)) => uwuwu_domain::Vcs::PullRequest { branch, url },
                (Some(branch), None) => uwuwu_domain::Vcs::Branch(branch),
                (None, Some(url)) => uwuwu_domain::Vcs::Artifact { url },
                (None, None) => uwuwu_domain::Vcs::Local,
            };
            let priority = self
                .priority
                .as_deref()
                .map(crate::naming::parse_priority)
                .unwrap_or(uwuwu_domain::Priority::Empty);
            task.apply_fields(uwuwu_domain::TaskFields::new(
                Default::default(),
                vcs,
                priority,
                false,
            ));
            task.set_kind(self.kind);
            task.set_automerge(self.automerge);
            task.set_direct(self.direct);
            task.set_fact_track(self.fact_track);
            IssueView {
                task,
                body: self.body.clone(),
                comments: self.comments.clone(),
            }
        }
    }

    /// Запись ячеек снапшота: номер + приоритет + automerge + direct +
    /// fact_track (#1258, #1631).
    type CellWrite = (
        u64,
        Option<String>,
        Option<bool>,
        Option<bool>,
        Option<bool>,
    );

    #[async_trait]
    impl TaskBoardRepo for FakeBoard {
        async fn row_status(&self, _board: &str, _number: u64) -> UsecaseResult<Option<Status>> {
            Ok(None)
        }

        async fn row_exists(&self, _board: &str, _number: u64) -> UsecaseResult<bool> {
            Ok(false)
        }

        async fn read_board(&self, _board: &str) -> UsecaseResult<Vec<Task>> {
            Ok(Vec::new())
        }
        async fn set_board_cells(
            &self,
            _b: &str,
            n: u64,
            p: Option<&str>,
            a: Option<bool>,
            d: Option<bool>,
            f: Option<bool>,
        ) -> UsecaseResult<()> {
            self.cells
                .lock()
                .expect("fake board")
                .push((n, p.map(str::to_string), a, d, f));
            if !self.phantom {
                let mut c = self.carrier.lock().expect("fake carrier");
                c.priority = p.map(str::to_string);
                if let Some(a) = a {
                    c.automerge = a;
                }
                if let Some(d) = d {
                    c.direct = d;
                }
                if let Some(f) = f {
                    c.fact_track = f;
                }
            }
            Ok(())
        }
        async fn set_status(
            &self,
            _b: &str,
            _n: u64,
            s: Status,
        ) -> UsecaseResult<crate::ports::StatusDelivery> {
            self.carrier.lock().expect("fake carrier").status = s;
            Ok(crate::ports::StatusDelivery::Confirmed {
                actual: naming::status_name(s).to_string(),
            })
        }
        async fn set_session(&self, _b: &str, _n: u64, _slot: Slot, _s: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn clear_session(&self, _b: &str, _n: u64, _slot: Slot) -> UsecaseResult<()> {
            Ok(())
        }
        async fn set_branch(
            &self,
            _b: &str,
            _n: u64,
            branch: &uwuwu_domain::BranchName,
        ) -> UsecaseResult<()> {
            if !self.phantom {
                self.carrier.lock().expect("fake carrier").branch =
                    Some(branch.as_str().to_string());
            }
            Ok(())
        }
        async fn set_pr(
            &self,
            _b: &str,
            _n: u64,
            _pr_url: &uwuwu_domain::PrUrl,
        ) -> UsecaseResult<()> {
            Ok(())
        }
        async fn post_comment(&self, n: u64, body: &str) -> UsecaseResult<String> {
            // фантом возвращает стандартный id без записи носителя:
            // нестандартный (пустой) ответ — отдельная fail-open ветка
            if self.phantom {
                return Ok("phantom-comment".into());
            }
            let mut c = self.carrier.lock().expect("fake carrier");
            let id = format!("fakec-{}", c.comments.len() + 1);
            c.comments.push(CommentRef {
                id: id.clone(),
                created_ms: 0,
                body: body.to_string(),
            });
            let _ = n;
            Ok(id)
        }
        async fn close_issue(&self, _n: u64) -> UsecaseResult<()> {
            Ok(())
        }
        async fn latest_comments(&self, _n: u64) -> UsecaseResult<Vec<CommentRef>> {
            Ok(self.carrier.lock().expect("fake carrier").comments.clone())
        }
        async fn read_issue(&self, _b: &str, n: u64) -> UsecaseResult<IssueView> {
            if self.read_err {
                return Err(UsecaseError::Board("carrier read 503 (мок)".into()));
            }
            Ok(self.carrier.lock().expect("fake carrier").view(n))
        }
        async fn issue_fields(&self, _n: u64) -> UsecaseResult<uwuwu_domain::TaskFields> {
            panic!("create не читает поля доски");
        }
        async fn create_issue(
            &self,
            _board: &str,
            _project: &str,
            _title: &str,
            _body: &str,
            kind_name: Option<&str>,
        ) -> UsecaseResult<u64> {
            // POST с type (#1271): имя «принято» носителем — Carrier видит
            // парс (как живой GitHub: эхо ответа = состояние носителя)
            if !self.phantom {
                self.carrier.lock().expect("fake carrier").kind =
                    kind_name.and_then(crate::naming::parse_task_type);
            }
            Ok(1)
        }
        async fn set_issue_type(&self, _n: u64, kind_name: &str) -> UsecaseResult<()> {
            if !self.phantom {
                self.carrier.lock().expect("fake carrier").kind =
                    crate::naming::parse_task_type(kind_name);
            }
            Ok(())
        }
        async fn rename_issue(&self, _n: u64, t: &str) -> UsecaseResult<()> {
            if !self.phantom {
                self.carrier.lock().expect("fake carrier").title = t.to_string();
            }
            Ok(())
        }
        async fn patch_cells(&self, _n: u64, _m: FmMutator) -> UsecaseResult<()> {
            Ok(())
        }
        async fn close_issue_ext(&self, _n: u64, _np: bool) -> UsecaseResult<()> {
            Ok(())
        }
        async fn archive_row(&self, _b: &str, _n: u64) -> UsecaseResult<()> {
            Ok(())
        }
        async fn issues_with_title(&self, _title: &str) -> UsecaseResult<Vec<u64>> {
            if self.search_err {
                return Err(UsecaseError::Board("search 503 (мок)".into()));
            }
            Ok(self.cold_hits.clone())
        }
        async fn replace_body(&self, _n: u64, b: &str) -> UsecaseResult<()> {
            if !self.phantom {
                self.carrier.lock().expect("fake carrier").body = b.to_string();
            }
            Ok(())
        }
    }

    /// Мок полей issue: записанные патчи хранит, load читает записанное —
    /// read-after-write (ADR-093) прямо в тесте.
    #[derive(Default)]
    struct FakeFields {
        stored: std::sync::Mutex<HashMap<String, String>>,
    }

    #[async_trait]
    impl IssueFieldStore for FakeFields {
        async fn load(&self, _n: u64) -> UsecaseResult<HashMap<String, String>> {
            Ok(self.stored.lock().expect("fake fields").clone())
        }
        async fn store(&self, _n: u64, patch: &[(&str, Option<String>)]) -> UsecaseResult<()> {
            let mut map = self.stored.lock().expect("fake fields");
            for (name, value) in patch {
                match value {
                    Some(v) if !v.is_empty() => {
                        map.insert((*name).into(), v.clone());
                    }
                    _ => {
                        map.remove(*name);
                    }
                }
            }
            Ok(())
        }
    }

    fn writecase(fields: Arc<FakeFields>) -> (TaskWriteUseCase, Arc<FakeBoard>) {
        writecase_with_cold(fields, Vec::new())
    }

    /// Вариант с настроенным носителем: phantom («мутация отвечает успехом,
    /// носитель не меняется» — красный класс инцидента #1331) или read_err
    /// (носитель не читается ни разу — Blind-ветка ADR-093).
    fn writecase_carrier(
        fields: Arc<FakeFields>,
        phantom: bool,
        read_err: bool,
    ) -> TaskWriteUseCase {
        let board = Arc::new(FakeBoard {
            cells: std::sync::Mutex::default(),
            phantom,
            read_err,
            ..Default::default()
        });
        TaskWriteUseCase {
            board,
            fields,
            boards: vec![("uwuwu".into(), "Задачи uwuwu".into())],
            clock: Arc::new(crate::pipeline::tick_tests::FixedClock),
            state: Arc::new(crate::pipeline::tick_tests::MemState::empty()),
        }
    }

    /// Вариант с «холодными» номерами репо (клин #1293): issues_with_title
    /// фейка возвращает их для любого тайтла — create обязан отклонить.
    fn writecase_with_cold(
        fields: Arc<FakeFields>,
        cold_hits: Vec<u64>,
    ) -> (TaskWriteUseCase, Arc<FakeBoard>) {
        let board = Arc::new(FakeBoard {
            cells: std::sync::Mutex::default(),
            cold_hits,
            ..Default::default()
        });
        let u = TaskWriteUseCase {
            board: board.clone(),
            fields,
            boards: vec![("uwuwu".into(), "Задачи uwuwu".into())],
            clock: Arc::new(crate::pipeline::tick_tests::FixedClock),
            state: Arc::new(crate::pipeline::tick_tests::MemState::empty()),
        };
        (u, board)
    }

    /// Холодный ящик (#1293): create с тайтлом закрытой вне снапшота
    /// строки — громкая ошибка дубля (issues_with_title, REST search).
    /// Снапшот-гард (dup_check по read_board) слепнет к архивным тайтлам —
    /// контрмера восстанавливает инвариант «тайтл уникален».
    #[tokio::test]
    async fn create_rejects_title_of_cold_row() {
        let fields = Arc::new(FakeFields::default());
        let (u, _) = writecase_with_cold(fields, vec![1243]);
        let err = u
            .create("uwuwu", spec("тайтл архивной задачи"))
            .await
            .expect_err("холодный дубль обязан отклоняться");
        assert!(
            err.to_string().contains("1243"),
            "ошибка называет холодный номер: {err}"
        );
    }

    /// Сбой REST-поиска (Low-2 ревью #1293) — fail-open: create живёт
    /// (первичный снапшот-гард отработал), warn в логе.
    #[tokio::test]
    async fn create_cold_search_failure_fails_open() {
        let fields = Arc::new(FakeFields::default());
        let board = Arc::new(FakeBoard {
            cells: std::sync::Mutex::default(),
            cold_hits: Vec::new(),
            search_err: true,
            ..Default::default()
        });
        let u = TaskWriteUseCase {
            board: board.clone(),
            fields,
            boards: vec![("uwuwu".into(), "Задачи uwuwu".into())],
            clock: Arc::new(crate::pipeline::tick_tests::FixedClock),
            state: Arc::new(crate::pipeline::tick_tests::MemState::empty()),
        };
        let number = u
            .create("uwuwu", spec("тело против упавшего поиска"))
            .await
            .expect("сбой поиска не валит create");
        assert_eq!(number, 1, "issue создан — гард fail-open");
    }

    /// Холодный ящик (#1293, дельта-раунды 2–3): rename в тайтл закрытой
    /// вне снапшота строки — громкая ошибка (чужой номер в тексте);
    /// свой тайтл (тот же номер) холодным дублем не считается —
    /// идемпотентная правка легальна (обе ветки контракта пинятся).
    #[tokio::test]
    async fn rename_rejects_title_of_cold_row_but_allows_own() {
        // ветка отказа: чужой холодный номер — громкая ошибка с ним в тексте
        let fields = Arc::new(FakeFields::default());
        let board = Arc::new(FakeBoard {
            cells: std::sync::Mutex::default(),
            cold_hits: vec![1243],
            ..Default::default()
        });
        let u = TaskWriteUseCase {
            board: board.clone(),
            fields,
            boards: vec![("uwuwu".into(), "Задачи uwuwu".into())],
            clock: Arc::new(crate::pipeline::tick_tests::FixedClock),
            state: Arc::new(crate::pipeline::tick_tests::MemState::empty()),
        };
        let p = TaskPatch {
            title: Some("тайтл архивной задачи".into()),
            ..Default::default()
        };
        let err = u
            .edit("uwuwu", 5, p)
            .await
            .expect_err("rename в холодный тайтл обязан отклоняться");
        assert!(
            err.to_string().contains("1243"),
            "ошибка называет холодный номер: {err}"
        );

        // ветка освобождения: cold содержит СОБСТВЕННЫЙ номер строки —
        // ретрай edit-конверта с тем же тайтлом легален (регресс до
        // !cold.is_empty() ломал бы идемпотентный rename — Low ревью)
        let board = Arc::new(FakeBoard {
            cells: std::sync::Mutex::default(),
            cold_hits: vec![5],
            ..Default::default()
        });
        let u = TaskWriteUseCase {
            board: board.clone(),
            fields: Arc::new(FakeFields::default()),
            boards: vec![("uwuwu".into(), "Задачи uwuwu".into())],
            clock: Arc::new(crate::pipeline::tick_tests::FixedClock),
            state: Arc::new(crate::pipeline::tick_tests::MemState::empty()),
        };
        let p = TaskPatch {
            title: Some("тайтл архивной задачи".into()),
            ..Default::default()
        };
        u.edit("uwuwu", 5, p)
            .await
            .expect("свой номер в cold — rename проходит");
    }

    /// Красный тест бага #1232 → контракт #1258: create маршрутизирует
    /// automerge в board-порт (ячейки снапшота доски), не в fields.store —
    /// там она протухала до TTL (проба #1258: «обновлена», task_list
    /// показывает старое значение).
    #[tokio::test]
    async fn create_writes_automerge_value() {
        let fields = Arc::new(FakeFields::default());
        let (u, board) = writecase(fields.clone());
        u.create("uwuwu", spec("тело")).await.unwrap();
        let cells = board.cells.lock().expect("fake board").clone();
        assert_eq!(
            cells,
            vec![(1, None, Some(true), Some(false), Some(false))],
            "create потерял automerge (мимо board-порта)"
        );
        // fields.store больше не несёт ячейку снапшота (Приоритет/Automerge —
        // единая точка записи board-адаптера); Тип остаётся нативным полем
        assert!(!fields
            .stored
            .lock()
            .expect("fake fields")
            .contains_key("Automerge"));
    }

    /// Выключенный automerge пишет off явно (состояние на носителе, не умолчание).
    #[tokio::test]
    async fn create_writes_automerge_off_when_disabled() {
        let fields = Arc::new(FakeFields::default());
        let (u, board) = writecase(fields);
        let mut s = spec("тело");
        s.automerge = false;
        u.create("uwuwu", s).await.unwrap();
        assert_eq!(
            board.cells.lock().expect("fake board").clone(),
            vec![(1, None, Some(false), Some(false), Some(false))],
        );
    }

    /// Приоритет create — тот же board-порт (#1258).
    #[tokio::test]
    async fn create_writes_priority_to_board_port() {
        let fields = Arc::new(FakeFields::default());
        let (u, board) = writecase(fields);
        let mut s = spec("тело");
        s.priority = "low".into();
        u.create("uwuwu", s).await.unwrap();
        assert_eq!(
            board.cells.lock().expect("fake board").clone(),
            vec![(1, Some("low".into()), Some(true), Some(false), Some(false))],
        );
    }

    /// direct-create (#1357): строка рождается в In Progress, поля разбора
    /// create пишет явно (пустой priority → medium, automerge off — тул
    /// прокидывает), direct=on на board-порту (кеш-гигиена #1258).
    #[tokio::test]
    async fn create_direct_puts_row_in_progress_with_defaults() {
        let fields = Arc::new(FakeFields::default());
        let (u, board) = writecase(fields);
        let mut s = spec("прямое поручение");
        s.direct = true;
        s.automerge = false;
        u.create("uwuwu", s).await.unwrap();
        assert_eq!(
            board.cells.lock().expect("fake board").clone(),
            vec![(
                1,
                Some("medium".into()),
                Some(false),
                Some(true),
                Some(false)
            )],
            "direct: medium/off/on — решение разбора на носителе явно"
        );
        let carrier = board.carrier.lock().expect("fake carrier");
        assert_eq!(
            carrier.status,
            Status::InProgress,
            "direct-строка не застревает в Analys"
        );
        assert!(carrier.direct);
    }

    /// fact_track-create (#1631): флаг пишется на board-порт явно
    /// (on — не умолчание чтения); строка — обычная Analys (вход рукой,
    /// вердикт Б), set_status не зовётся.
    #[tokio::test]
    async fn create_fact_track_writes_flag_and_stays_in_analys() {
        let fields = Arc::new(FakeFields::default());
        let (u, board) = writecase(fields);
        let mut s = spec("тело");
        s.fact_track = true;
        u.create("uwuwu", s).await.unwrap();
        assert_eq!(
            board.cells.lock().expect("fake board").clone(),
            vec![(1, None, Some(true), Some(false), Some(true))],
            "fact_track on пишется явно — состояние на носителе"
        );
        let carrier = board.carrier.lock().expect("fake carrier");
        assert_eq!(
            carrier.status,
            Status::Empty,
            "fact_track-строка не уходит в In Progress сама (вердикт Б)"
        );
        assert!(carrier.fact_track);
    }

    /// direct + fact_track взаимоисключающи (#1631): отказ громкий,
    /// строка не рождается.
    #[tokio::test]
    async fn create_direct_and_fact_track_conflict() {
        let fields = Arc::new(FakeFields::default());
        let (u, _) = writecase(fields);
        let mut s = spec("тело");
        s.direct = true;
        s.fact_track = true;
        let res = u.create("uwuwu", s).await;
        assert!(res.is_err(), "оба флага — отказ");
    }

    /// Обычная create-строка — канон как был: Analys (set_status не звался),
    /// direct off на носителе, priority не пишется (гейт требует явности).
    #[tokio::test]
    async fn create_ordinary_row_stays_in_analys() {
        let fields = Arc::new(FakeFields::default());
        let (u, board) = writecase(fields);
        u.create("uwuwu", spec("тело")).await.unwrap();
        assert_eq!(
            board.cells.lock().expect("fake board").clone(),
            vec![(1, None, Some(true), Some(false), Some(false))],
            "direct off пишется явно — состояние на носителе"
        );
        assert_eq!(
            board.carrier.lock().expect("fake carrier").status,
            Status::Empty,
            "set_status для обычной строки не вызывается"
        );
    }

    /// Тип уезжает в POST create нативным именем (#1271): маппинг канона
    /// в словарь issue Type и дефолт Feature (канон create) — на носителе.
    /// (tester #1383: пины восстановлены из master — ребейз унёс их вместе
    /// с kind-капчей фейка; на ветке тот же контракт накрыт только на
    /// MCP-поверхности, usecase-уровень остался голым.)
    #[tokio::test]
    async fn create_carries_native_issue_type() {
        let fields = Arc::new(FakeFields::default());
        let (u, board) = writecase(fields.clone());
        let mut s = spec("тело");
        s.kind = Some(TaskType::Bug);
        u.create("uwuwu", s).await.unwrap();
        assert_eq!(
            board.carrier.lock().expect("fake carrier").kind,
            Some(TaskType::Bug),
            "Bug → нативный «Bug» в POST, носитель читает парс"
        );

        let (u, board) = writecase(fields.clone());
        u.create("uwuwu", spec("без типа")).await.unwrap();
        assert_eq!(
            board.carrier.lock().expect("fake carrier").kind,
            Some(TaskType::Feature),
            "None → дефолт feature канона create"
        );
    }

    /// edit(type): нативный issue Type записан и подтверждён контрольным
    /// чтением (#1271, честная квитанция #1331).
    #[tokio::test]
    async fn edit_kind_is_verified_by_carrier_read() {
        let fields = Arc::new(FakeFields::default());
        let (u, board) = writecase(fields);
        u.edit(
            "uwuwu",
            1,
            TaskPatch {
                kind: Some(TaskType::Refactoring),
                ..Default::default()
            },
        )
        .await
        .expect("edit type доставлен");
        assert_eq!(
            board.carrier.lock().expect("fake carrier").kind,
            Some(TaskType::Refactoring),
        );
    }

    /// Фантомный edit(type): PATCH «принят», носитель типа не видит —
    /// громкий отказ с вычисленным перечнем («mismatched: type»).
    #[tokio::test]
    async fn phantom_edit_kind_is_loud() {
        let fields = Arc::new(FakeFields::default());
        let u = writecase_carrier(fields, true, false);
        let err = u
            .edit(
                "uwuwu",
                1,
                TaskPatch {
                    kind: Some(TaskType::Docs),
                    ..Default::default()
                },
            )
            .await
            .expect_err("фантомный edit type — громкий отказ");
        let text = err.to_string();
        assert!(text.contains("was not confirmed"), "текст отказа: {text}");
        assert!(
            text.contains("mismatched: type"),
            "перечень несошедших ключей вычислен: {text}"
        );
    }

    /// task_edit(automerge) — инструмент до-пометки старых строк (#1232):
    /// маршрутизация в board-порт (#1258).
    #[tokio::test]
    async fn edit_patches_automerge() {
        let fields = Arc::new(FakeFields::default());
        let (u, board) = writecase(fields);
        u.edit(
            "uwuwu",
            1,
            TaskPatch {
                automerge: Some(false),
                ..TaskPatch::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            board.cells.lock().expect("fake board").clone(),
            vec![(1, None, Some(false), None, None)],
            "edit direct не пишет — флаг create-only (#1357)"
        );
    }

    /// task_edit(priority) — тот же порт (#1258).
    #[tokio::test]
    async fn edit_patches_priority() {
        let fields = Arc::new(FakeFields::default());
        let (u, board) = writecase(fields);
        u.edit(
            "uwuwu",
            1,
            TaskPatch {
                priority: Some("high".into()),
                ..TaskPatch::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            board.cells.lock().expect("fake board").clone(),
            vec![(1, Some("high".into()), None, None, None)],
        );
    }

    /// TaskPatch с одним automerge непуст — пустой патч не глотается.
    #[test]
    fn patch_with_automerge_is_not_empty() {
        let p = TaskPatch {
            automerge: Some(true),
            ..TaskPatch::default()
        };
        assert!(!p.is_empty());
    }

    #[test]
    fn patch_is_empty_only_when_no_field_is_set() {
        // дефолтный патч — менять нечего
        assert!(TaskPatch::default().is_empty());
        // каждое заполненное поле в отдельности делает патч непустым
        let cases = [
            TaskPatch {
                status: Some(Status::Todo),
                ..TaskPatch::default()
            },
            TaskPatch {
                branch: Some("work/x".into()),
                ..TaskPatch::default()
            },
            TaskPatch {
                pr: Some("https://example.test/pull/1".into()),
                ..TaskPatch::default()
            },
            TaskPatch {
                priority: Some("high".into()),
                ..TaskPatch::default()
            },
            TaskPatch {
                title: Some("новый тайтл".into()),
                ..TaskPatch::default()
            },
        ];
        for patch in &cases {
            assert!(!patch.is_empty(), "патч с {patch:?} не пуст");
        }
    }

    #[test]
    fn create_body_no_duplicate_h2() {
        // инцидент #1149: агент шлёт тело в канон-форме — скелет не дублируется
        let canon = "## Постановка\n\nТекст.";
        assert_eq!(
            render_create_body(&spec(canon)),
            "## Постановка\n\nТекст.\n"
        );
        assert_eq!(
            render_create_body(&spec(canon))
                .matches("## Постановка")
                .count(),
            1
        );
        // голый текст — скелет добавляется
        assert_eq!(
            render_create_body(&spec("просто текст")),
            "## Постановка\n\nпросто текст\n"
        );
        // пустое тело — тайтл как постановка
        assert_eq!(render_create_body(&spec("")), "## Постановка\n\nt\n");
    }

    #[test]
    fn create_body_strips_fm() {
        // инцидент #1151: дример скопировал fm-шапку старого канона —
        // граница вырезает fm независимо от дисциплины агента
        let with_fm = "---\nstatus: Draft\npriority: medium\n---\n## Постановка\n\nТекст.";
        let out = render_create_body(&spec(with_fm));
        assert!(!out.contains("status:"), "fm вырезан: {out}");
        assert_eq!(out.matches("## Постановка").count(), 1);
    }

    #[test]
    fn create_body_strips_full_card_header() {
        // #1632: task_create идёт через тот же гард — копия карточки
        // task_get (тайтл-строка + fm) в body_md срезается на границе
        let card_copy = "#1526 t — In Progress\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: h\n\n## Постановка\n\nТекст.";
        let out = render_create_body(&spec(card_copy));
        assert!(!out.contains("--- fm ---"), "шапка вырезана: {out}");
        assert!(!out.contains("#1526"), "тайтл-строка вырезана: {out}");
        assert_eq!(out, "## Постановка\n\nТекст.\n");
    }

    #[test]
    fn create_body_strips_task_get_header_without_trailing_newline() {
        let header = "--- fm ---\nbranch: x\npr: y\nautomerge: true\ndirect: false\nfact_track: false\ntype: feature\nbody_hash: h\nbody_bytes: 66";
        let out = render_create_body(&spec(header));
        assert!(
            !out.contains("body_bytes"),
            "остаток шапки не становится телом: {out}"
        );
    }

    /// Дисплей-шапка task_get срезается на границе записи (fm-стек
    /// инцидента #1331: агент копирует вывод task_get назад в task_body).
    /// Все четыре формы рендера — актуальная (#1604+#1631, fact_track и
    /// body_bytes) и три легаси (#1631+, #1271+, до #1271).
    #[test]
    fn task_get_display_header_is_stripped() {
        // актуальный рендер (#1604+#1631): 8 ключей, body_bytes последним
        let md = "--- fm ---\nbranch: work/task-1604\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: 9de430c691c92328\nbody_bytes: 36\n\n## Постановка\nтекст";
        assert_eq!(strip_task_get_header(md), "## Постановка\nтекст");
        // легаси #1631..#1604: 7 ключей, fact_track без body_bytes
        let md_legacy7 = "--- fm ---\nbranch: work/task-1331\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: —\nbody_hash: 9de430c691c92328\n\n## Постановка\nтекст";
        assert_eq!(strip_task_get_header(md_legacy7), "## Постановка\nтекст");
        // легаси #1271..#1631: 6 ключей, без fact_track/body_bytes
        let md_legacy6 = "--- fm ---\nbranch: x\npr: —\nautomerge: true\ndirect: false\ntype: bug\nbody_hash: h\n\n## Т";
        assert_eq!(strip_task_get_header(md_legacy6), "## Т");
        // легаси до #1271: 5 ключей, без type
        let md_legacy5 = "--- fm ---\nbranch: work/task-1331\npr: —\nautomerge: true\ndirect: false\nbody_hash: 9de430c691c92328\n\n## Постановка\nтекст";
        assert_eq!(strip_task_get_header(md_legacy5), "## Постановка\nтекст");
    }

    /// fm-стек из чистых fm-блоков, без тайтл-строк (#1331): форма A
    /// тоже применяется циклически — срезаются все слои, не только
    /// первый.
    #[test]
    fn task_get_stacked_fm_blocks_are_stripped() {
        let fm = "--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: h\n\n";
        let md = format!("{fm}{fm}## Постановка\nтело");
        assert_eq!(strip_task_get_header(&md), "## Постановка\nтело");
    }

    /// Полная копия карточки (#1632): тайтл-строка рендера + пустая
    /// строка + fm-блок — срезается целиком, на носителе тело.
    #[test]
    fn task_get_full_card_copy_is_stripped() {
        let md = "#1526 Bug: гардин молчит — In Progress\n\n--- fm ---\nbranch: work/task-1526\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: 9de430c691c92328\n\n## Постановка\nтекст";
        assert_eq!(strip_task_get_header(md), "## Постановка\nтекст");
    }

    /// Полная копия карточки без тела — срезается в пустоту (пустой
    /// эффективный контент → отказ #1572 на границе записи).
    #[test]
    fn task_get_full_card_without_body_strips_to_empty() {
        let md = "#1526 t — In Progress\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: h\n\n";
        assert_eq!(strip_task_get_header(md), "");
    }

    /// fm-стек (#1331): подряд идущие шапки карточек срезаются все.
    #[test]
    fn task_get_stacked_card_headers_are_stripped() {
        let layer = |title: &str| {
            format!(
                "{title}\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: \
                 false\nfact_track: false\ntype: bug\nbody_hash: h\n\n"
            )
        };
        let md = format!(
            "{}{}{}## Постановка\nтело",
            layer("#1 t — In Progress"),
            layer("#2 t — Design"),
            layer("#3 t — Todo"),
        );
        assert_eq!(strip_task_get_header(&md), "## Постановка\nтело");
    }

    /// Смешанный стек: внешний слой легаси-7 (fact_track без
    /// body_bytes) + внутренний легаси-6 (без fact_track) — срезаются
    /// ОБА: легаси-наборы приземлились с #506 (#1604 в мастере), страж
    /// границы перевёрнут (решение 6 плана #1632). Актуальная
    /// восьмиключевая форма — в пинах ниже.
    #[test]
    fn task_get_stacked_mixed_render_and_legacy_layers_are_stripped() {
        let fresh = "#1 t — In Progress\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: h\n\n";
        let legacy = "#2 t — Todo\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\ntype: bug\nbody_hash: h\n\n";
        let md = format!("{fresh}{legacy}## Тело");
        assert_eq!(strip_task_get_header(&md), "## Тело");
    }

    /// Карточка актуального рендера (#1604+#1631): восемь ключей,
    /// `body_bytes` последним. Прежние фикстуры формы B — легаси-7;
    /// без этого пина актуальная форма держится только tools-зеркалом.
    fn card_copy_fresh_with_title(title: &str) -> String {
        format!(
            "{title}\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: \
             false\nfact_track: false\ntype: bug\nbody_hash: h\nbody_bytes: 42\n\n## Постановка\nтекст"
        )
    }

    #[test]
    fn task_get_fresh_render_card_copy_is_stripped() {
        let md = card_copy_fresh_with_title("#1632 t — In Progress");
        assert_eq!(strip_task_get_header(&md), "## Постановка\nтекст");
    }

    /// Терминатор «конец входа» (#1604) в форме B: шапка актуального
    /// рендера без хвостового переноса срезается в пустоту — на границе
    /// записи это отказ #1572.
    #[test]
    fn task_get_fresh_render_card_without_newline_strips_to_empty() {
        let md = "#1632 t — In Progress\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: h\nbody_bytes: 42";
        assert_eq!(strip_task_get_header(md), "");
    }

    /// Стек: внешний слой актуального рендера (8 ключей) + внутренний
    /// легаси-6 — срез циклом, на носителе тело.
    #[test]
    fn task_get_stacked_fresh_render_and_legacy_layers_are_stripped() {
        let fresh = "#1 t — In Progress\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: h\nbody_bytes: 42\n\n";
        let legacy = "#2 t — Todo\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\ntype: bug\nbody_hash: h\n\n";
        let md = format!("{fresh}{legacy}## Тело");
        assert_eq!(strip_task_get_header(&md), "## Тело");
    }

    /// Шапка без хвостового переноса срезается: терминатор последнего
    /// ключа — конец входа (#1604), дельта приземлилась с #506.
    #[test]
    fn task_get_header_without_trailing_newline_is_stripped() {
        let md = "--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: h";
        assert_eq!(strip_task_get_header(md), "");
    }

    /// Карточка рендера с заданной тайтл-строкой и валидным fm-блоком:
    /// всё, кроме самой тайтл-строки, удовлетворяет форме B — падение
    /// теста однозначно называет сломанный барьер тайтла.
    fn card_copy_with_title(title: &str) -> String {
        format!(
            "{title}\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: \
             false\nfact_track: false\ntype: bug\nbody_hash: h\n\n## Постановка\nтекст"
        )
    }

    /// fm-блока за тайтлом нет — не карточка, вход цел.
    #[test]
    fn task_get_title_without_fm_block_is_kept() {
        let md = "#1526 t — In Progress\n\n## Постановка\nтекст";
        assert_eq!(strip_task_get_header(md), md);
    }

    /// Пустой строки перед fm нет — не карточка, вход цел.
    #[test]
    fn task_get_title_without_blank_line_before_fm_is_kept() {
        let md = "#1526 t — In Progress\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: h\n\nтекст";
        assert_eq!(strip_task_get_header(md), md);
    }

    /// CRLF-карточка — скоп #1637, здесь барьер не срабатывает (строгий
    /// LF-паттерн); тест изолирован, чтобы держатель #1637 перевернул
    /// ровно его, не задевая LF-барьеры.
    #[test]
    fn task_get_crlf_card_header_is_kept() {
        let md = "#1526 t — In Progress\r\n\r\n--- fm ---\r\nbranch: b\r\npr: —\r\nautomerge: true\r\ndirect: false\r\nfact_track: false\r\ntype: bug\r\nbody_hash: h\r\n\r\nтекст";
        assert_eq!(strip_task_get_header(md), md);
    }

    /// «Тайтл» не рендер-формы (## Постановка) — вход цел.
    #[test]
    fn task_get_non_render_title_is_kept() {
        let md = "## Постановка\n\n--- fm ---\nbranch: b\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: h\n\nтекст";
        assert_eq!(strip_task_get_header(md), md);
    }

    /// Барьер решётки: тайтл без `#` — не рендер; вход цел.
    #[test]
    fn task_get_title_without_hash_is_kept() {
        let md = card_copy_with_title("1526 t — In Progress");
        assert_eq!(strip_task_get_header(md.as_str()), md.as_str());
    }

    /// Барьер « — »: тайтл без разделителя статуса — не рендер.
    #[test]
    fn task_get_title_without_dash_separator_is_kept() {
        let md = card_copy_with_title("#1526 t In Progress");
        assert_eq!(strip_task_get_header(md.as_str()), md.as_str());
    }

    /// Барьер цифр: тайтл без номера — не рендер.
    #[test]
    fn task_get_title_without_digits_is_kept() {
        let md = card_copy_with_title("# — In Progress");
        assert_eq!(strip_task_get_header(md.as_str()), md.as_str());
    }

    /// Барьер пробела после номера: `#1526t` — не рендер.
    #[test]
    fn task_get_title_without_space_after_digits_is_kept() {
        let md = card_copy_with_title("#1526t — In Progress");
        assert_eq!(strip_task_get_header(md.as_str()), md.as_str());
    }

    #[test]
    fn task_get_header_in_middle_is_kept() {
        let md =
            "## Секция\n\n--- fm ---\nbranch: x\npr: y\nautomerge: true\ndirect: false\nfact_track: false\ntype: —\nbody_hash: h\n\nтекст";
        assert_eq!(
            strip_task_get_header(md),
            md,
            "«шапка» в середине тела — не наша граница"
        );
    }

    #[test]
    fn task_get_header_only_without_trailing_newline_strips_to_empty() {
        let h8 = "--- fm ---\nbranch: x\npr: y\nautomerge: true\ndirect: false\nfact_track: false\ntype: feature\nbody_hash: h\nbody_bytes: 66";
        assert_eq!(strip_task_get_header(h8), "");
        let h7 = "--- fm ---\nbranch: x\npr: y\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nbody_hash: h";
        assert_eq!(strip_task_get_header(h7), "");
        let h6 =
            "--- fm ---\nbranch: x\npr: y\nautomerge: true\ndirect: false\ntype: bug\nbody_hash: h";
        assert_eq!(strip_task_get_header(h6), "");
        let h5 = "--- fm ---\nbranch: x\npr: y\nautomerge: true\ndirect: false\nbody_hash: h";
        assert_eq!(strip_task_get_header(h5), "");
    }

    #[test]
    fn task_get_header_partial_or_foreign_is_kept() {
        // неполный набор ключей — не рендер task_get
        let md = "--- fm ---\nbranch: x\npr: y\n\n## Тело";
        assert_eq!(strip_task_get_header(md), md);
        // посторонняя строка в «шапке» — не трогаем
        let md2 = "--- fm ---\nbranch: x\nслучайная строка\nautomerge: true\nbody_hash: h\n\n## Т";
        assert_eq!(strip_task_get_header(md2), md2);
        // валидный fm старого канона — это fm_lite, не дисплей-шапка
        let md3 = "---\ntitle: x\n---\nтекст";
        assert_eq!(strip_task_get_header(md3), md3);
        // лишний посторонний ключ в наборе — не рендер (актуальная
        // 8-ключевая форма с чужой строкой между type и body_hash)
        let md4 = "--- fm ---\nbranch: x\npr: y\nautomerge: true\ndirect: false\nfact_track: false\ntype: bug\nслучайная строка\nbody_hash: h\nbody_bytes: 1\n\n## Т";
        assert_eq!(strip_task_get_header(md4), md4);
    }

    // --- Честные квитанции: контрольное чтение носителя (#1331) ---

    /// Красный класс инцидента #1331: мутация отвечает успехом, носитель
    /// не меняется — task_body обязан отказывать громко, а не рапортовать
    /// квитанцию о недоставленном теле.
    #[tokio::test]
    async fn phantom_body_write_is_loud() {
        let u = writecase_carrier(Arc::new(FakeFields::default()), true, false);
        let err = u
            .set_body("uwuwu", 1, Some("## Новое тело\n"))
            .await
            .expect_err("фантомная запись тела — громкий отказ");
        assert!(
            err.to_string().contains("was not confirmed"),
            "текст отказа: {err}"
        );
    }

    /// Коммент: пост «принят» с id, лента носителя его не содержит — громко.
    #[tokio::test]
    async fn phantom_comment_is_loud() {
        let u = writecase_carrier(Arc::new(FakeFields::default()), true, false);
        let err = u
            .comment(1, "probe-1331", None)
            .await
            .expect_err("фантомный коммент — громкий отказ");
        assert!(
            err.to_string().contains("was not confirmed"),
            "текст отказа: {err}"
        );
    }

    /// edit с полями: ветка записана, носитель её не видит — громко.
    /// Пин на вычисленный перечень («не сошлись: ветка») — регресс к
    /// статичному тексту без контрольного перечитывания ловится здесь
    /// (раунд 2 ревью #1331: вербикт-подстроки — живая практика репо).
    #[tokio::test]
    async fn phantom_edit_fields_are_loud() {
        let u = writecase_carrier(Arc::new(FakeFields::default()), true, false);
        let err = u
            .edit(
                "uwuwu",
                1,
                TaskPatch {
                    branch: Some("work/task-1".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("фантомный edit — громкий отказ");
        let text = err.to_string();
        assert!(text.contains("was not confirmed"), "текст отказа: {text}");
        assert!(
            text.contains("mismatched: branch"),
            "перечень несошедших ключей вычислен, не статичен: {text}"
        );
    }

    /// rename: тайтл записан, носитель отдаёт старый — громко (находка
    /// Common ревью плана: rename в скоупе честных квитанций).
    #[tokio::test]
    async fn phantom_rename_is_loud() {
        let u = writecase_carrier(Arc::new(FakeFields::default()), true, false);
        let err = u
            .edit(
                "uwuwu",
                1,
                TaskPatch {
                    title: Some("новый тайтл".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("фантомный rename — громкий отказ");
        assert!(
            err.to_string().contains("was not confirmed"),
            "текст отказа: {err}"
        );
    }

    /// Blind-ветка (ADR-093): носитель не читается — успех с непроверенной
    /// квитанцией от отправленного тела; ретрай мутации не навязывается.
    #[tokio::test]
    async fn blind_body_warns_and_reports_sent_body() {
        let u = writecase_carrier(Arc::new(FakeFields::default()), false, true);
        let r = u.set_body("uwuwu", 1, Some("## Тело\n")).await.unwrap();
        assert!(!r.verified, "непроверенная квитанция");
        assert_eq!(r.body_hash, md_section::body_hash("## Тело\n"));
        assert_eq!(r.body_size, "## Тело\n".len());
    }

    /// Честный носитель: квитанция от прочитанного с носителя.
    #[tokio::test]
    async fn honest_body_receipt_carries_carrier_body() {
        let u = writecase_carrier(Arc::new(FakeFields::default()), false, false);
        let r = u.set_body("uwuwu", 1, Some("## Тело\n")).await.unwrap();
        assert!(r.verified, "квитанция с контрольным чтением");
        assert_eq!(r.body_hash, md_section::body_hash("## Тело\n"));
    }

    /// Честный носитель: коммент найден в ленте — успех.
    #[tokio::test]
    async fn honest_comment_is_verified() {
        let u = writecase_carrier(Arc::new(FakeFields::default()), false, false);
        u.comment(1, "текст", None)
            .await
            .expect("доставленный коммент");
    }

    // ==== гарды write-path тел (#1572) — кромки юнит-уровня ====

    /// Порог сжатия: новое < 70% старого — отказ; ровно на пороге —
    /// запись (граница зафиксирована, целочисленная арифметика).
    #[test]
    fn shrink_threshold_boundary_is_pinned() {
        assert!(!radical_shrink(1000, 700), "ровно 70% — не сжатие");
        assert!(radical_shrink(1000, 699), "байт за порогом — сжатие");
        assert!(radical_shrink(1024, 716), "секция: 716/1024 за порогом");
        assert!(!radical_shrink(1024, 717), "секция: 717/1024 в норме");
    }

    /// Гард пустой записи (сценарий 1): set_body(None) — отказ с длиной
    /// текущего тела и рецептом; носитель не тронут.
    #[tokio::test]
    async fn set_body_without_content_refuses_with_carrier_len() {
        let (u, board) = writecase(Arc::new(FakeFields::default()));
        let body = "## А\nтело носителя\n";
        u.set_body("uwuwu", 1, Some(body)).await.unwrap();
        let err = u
            .set_body("uwuwu", 1, None)
            .await
            .expect_err("запись без body отказывает");
        let text = err.to_string();
        assert!(
            text.contains(&format!("{} bytes", body.len())),
            "длина текущего тела в отказе: {text}"
        );
        assert!(text.contains("task_get"), "рецепт чтения: {text}");
        assert!(text.contains("body: \"\""), "рецепт очистки: {text}");
        let stored = board.read_issue("Задачи uwuwu", 1).await.unwrap().body;
        assert_eq!(stored, body, "носитель цел");
    }

    /// Явная whitespace-строка телом — осознанная очистка: пишется пустое
    /// тело (whitespace-мусор носителем не становится).
    #[tokio::test]
    async fn set_body_whitespace_only_clears_deliberately() {
        let (u, board) = writecase(Arc::new(FakeFields::default()));
        u.set_body("uwuwu", 1, Some("  \n\t ")).await.unwrap();
        let stored = board.read_issue("Задачи uwuwu", 1).await.unwrap().body;
        assert!(stored.trim().is_empty(), "носитель очищен: {stored:?}");
    }

    /// Гард пустой записи (сценарий 3): set_body_section(None, …) — отказ
    /// с мерой текущей секции; секция цела.
    #[tokio::test]
    async fn set_body_section_without_content_refuses_with_section_len() {
        let (u, board) = writecase(Arc::new(FakeFields::default()));
        let body = "## А\n1234567\n\n## Б\nвторое\n";
        u.set_body("uwuwu", 1, Some(body)).await.unwrap();
        let hash = md_section::body_hash(body);
        let err = u
            .set_body_section(
                "uwuwu",
                1,
                SectionWrite {
                    section: "А",
                    content: None,
                    base_hash: &hash,
                    confirm: false,
                },
            )
            .await
            .expect_err("запись без content отказывает");
        let text = err.to_string();
        assert!(
            text.contains(&format!("{} bytes", "1234567".len())),
            "мера текущей секции в отказе: {text}"
        );
        assert!(text.contains("task_get"), "рецепт чтения: {text}");
        assert!(text.contains("content: \"\""), "рецепт очистки: {text}");
        let stored = board.read_issue("Задачи uwuwu", 1).await.unwrap().body;
        assert_eq!(stored, body, "носитель цел");
    }

    /// Секция ровно на полу (1024 байта) под гардом сжатия; байтом ниже —
    /// полностью редактируема без confirm (граница пола зафиксирована).
    /// Носитель утяжелён: телесное плечо молчит — проверяется именно пол
    /// секционного плеча.
    #[tokio::test]
    async fn section_shrink_floor_boundary_is_pinned() {
        let mk = |tail: usize| {
            format!(
                "## Head\nдержатель\n\n## Tail\n{}\n\n## More\n{}\n",
                "x".repeat(tail),
                "f".repeat(3072)
            )
        };
        // 1024: пол пройден, соотношение 1/1024 — отказ
        let (u, _) = writecase(Arc::new(FakeFields::default()));
        let body = mk(1024);
        u.set_body("uwuwu", 1, Some(&body)).await.unwrap();
        let hash = md_section::body_hash(&body);
        let err = u
            .set_body_section(
                "uwuwu",
                1,
                SectionWrite {
                    section: "Tail",
                    content: Some("y"),
                    base_hash: &hash,
                    confirm: false,
                },
            )
            .await
            .expect_err("секция на полу под гардом");
        assert!(err.to_string().contains("radical shrink"), "{err}");
        // 1023: ниже пола — редактирование свободное
        let (u, board) = writecase(Arc::new(FakeFields::default()));
        let body = mk(1023);
        u.set_body("uwuwu", 1, Some(&body)).await.unwrap();
        let hash = md_section::body_hash(&body);
        u.set_body_section(
            "uwuwu",
            1,
            SectionWrite {
                section: "Tail",
                content: Some("y"),
                base_hash: &hash,
                confirm: false,
            },
        )
        .await
        .expect("секция ниже пола свободно редактируема");
        let stored = board.read_issue("Задачи uwuwu", 1).await.unwrap().body;
        assert!(stored.contains("## Tail\ny"), "контент записан: {stored}");
    }

    /// Явная очистка большой секции мимо гарда сжатия: пустой content —
    /// осознанный акт (рецепт отказа полного task_body на неё опирается).
    #[tokio::test]
    async fn explicit_clear_of_fat_section_bypasses_shrink_guard() {
        let (u, board) = writecase(Arc::new(FakeFields::default()));
        let body = format!(
            "## Head\nдержатель\n\n## Tail\n{}\n\n## More\nнаполнитель\n",
            "x".repeat(2048)
        );
        u.set_body("uwuwu", 1, Some(&body)).await.unwrap();
        let hash = md_section::body_hash(&body);
        let report = u
            .set_body_section(
                "uwuwu",
                1,
                SectionWrite {
                    section: "Tail",
                    content: Some(""),
                    base_hash: &hash,
                    confirm: false,
                },
            )
            .await
            .expect("явная очистка без confirm");
        assert!(report.cleared, "секция очищена");
        assert_eq!(report.section_was, 2048, "мера снятой секции");
        assert_eq!(report.section_now, 0, "очищено");
        let stored = board.read_issue("Задачи uwuwu", 1).await.unwrap().body;
        assert!(
            stored.contains("## Tail") && !stored.contains("xxx"),
            "чисто"
        );
    }

    /// Плечо тела (класс #1277): маленькая секция (< пола), но тело
    /// срезано за треть — отказ; с confirm проходит с пометкой.
    #[tokio::test]
    async fn body_shoulder_fires_when_section_is_below_floor() {
        let (u, _) = writecase(Arc::new(FakeFields::default()));
        // тело ≈2КБ, секция «А» 800 байт (< пола 1024), замена на 10 байт:
        // секционное плечо молчит по полу, телесное — стреляет (>30%)
        let body = format!("## А\n{}\n\n## Б\n{}\n", "a".repeat(800), "b".repeat(1200));
        u.set_body("uwuwu", 1, Some(&body)).await.unwrap();
        let hash = md_section::body_hash(&body);
        let err = u
            .set_body_section(
                "uwuwu",
                1,
                SectionWrite {
                    section: "А",
                    content: Some("0123456789"),
                    base_hash: &hash,
                    confirm: false,
                },
            )
            .await
            .expect_err("срез трети тела — отказ");
        assert!(
            err.to_string().contains("radical shrink"),
            "телесное плечо: {err}"
        );
        // осознанное сокращение — confirm проводит
        let report = u
            .set_body_section(
                "uwuwu",
                1,
                SectionWrite {
                    section: "А",
                    content: Some("0123456789"),
                    base_hash: &hash,
                    confirm: true,
                },
            )
            .await
            .expect("confirm проводит");
        assert!(report.confirmed, "квитанция помечает confirm");
        assert_eq!(report.section_was, 800, "было по секции");
        assert_eq!(report.section_now, 10, "стало по секции");
    }

    /// Числа гарда объясняют отказ при хилинге дублей (ревью #1572 Low):
    /// контент дублей входит в меру «было» — отказ несёт весь снятый
    /// объём (секция+дубль), «ровных» чисел по секции не бывает; дубль —
    /// треть тела, правка секции без сжатия получает отказ с честными
    /// числами и проходит через confirm.
    #[tokio::test]
    async fn duplicate_healing_volume_is_counted_in_section_was() {
        let (u, _) = writecase(Arc::new(FakeFields::default()));
        // секция 1000Б + дубль 3500Б (треть тела) + наполнитель: обычная
        // правка секции 1000→950 стирает 4550Б (хилинг) — тело сжато за треть
        let body = format!(
            "## А\n{}\n\n## А\n{}\n\n## Б\n{}\n",
            "a".repeat(1000),
            "a".repeat(3500),
            "b".repeat(5500)
        );
        u.set_body("uwuwu", 1, Some(&body)).await.unwrap();
        let hash = md_section::body_hash(&body);
        let err = u
            .set_body_section(
                "uwuwu",
                1,
                SectionWrite {
                    section: "А",
                    content: Some(&"z".repeat(950)),
                    base_hash: &hash,
                    confirm: false,
                },
            )
            .await
            .expect_err("срез снятого объёма с дублем — отказ");
        let text = err.to_string();
        assert!(
            text.contains("section 4500 → 950 bytes"),
            "мера «было» включает дубль — числа объясняют отказ: {text}"
        );
        // осознанная правка с хилингом — confirm проводит
        let report = u
            .set_body_section(
                "uwuwu",
                1,
                SectionWrite {
                    section: "А",
                    content: Some(&"z".repeat(950)),
                    base_hash: &hash,
                    confirm: true,
                },
            )
            .await
            .expect("confirm проводит осознанную правку с хилингом");
        assert!(report.confirmed, "квитанция помечает confirm");
        assert_eq!(report.section_was, 4500, "весь снятый объём в квитанции");
    }

    /// «Было» и «стало» одной меры — байты строк без переводов строк
    /// (ревью #1572 Low): многострочная правка не показывает фальшивый
    /// рост на число строк.
    #[tokio::test]
    async fn receipt_measures_are_symmetric_on_multiline_section() {
        let (u, _) = writecase(Arc::new(FakeFields::default()));
        let body = "## А\naaa\nbbb\nccc\n\n## Б\nвторое\n";
        u.set_body("uwuwu", 1, Some(body)).await.unwrap();
        let hash = md_section::body_hash(body);
        let report = u
            .set_body_section(
                "uwuwu",
                1,
                SectionWrite {
                    section: "А",
                    content: Some("x\ny"),
                    base_hash: &hash,
                    confirm: false,
                },
            )
            .await
            .expect("многострочная дельта");
        assert_eq!(report.section_was, 9, "три строки по три байта");
        assert_eq!(report.section_now, 2, "две строки по одному байту");
        assert!(
            report.section_now < report.section_was,
            "без фальшивого роста"
        );
    }

    /// Квитанция несёт было/стало по телу и по секции (контракт #1409)
    /// и на обычной дельте без confirm.
    #[tokio::test]
    async fn receipt_carries_was_new_lengths_on_ordinary_delta() {
        let (u, _) = writecase(Arc::new(FakeFields::default()));
        let body = "## А\n12345\n\n## Б\nвторое\n";
        u.set_body("uwuwu", 1, Some(body)).await.unwrap();
        let hash = md_section::body_hash(body);
        let report = u
            .set_body_section(
                "uwuwu",
                1,
                SectionWrite {
                    section: "А",
                    content: Some("1234"),
                    base_hash: &hash,
                    confirm: false,
                },
            )
            .await
            .expect("обычная дельта");
        assert_eq!(report.body_was, body.len());
        assert_eq!(report.section_was, 5);
        assert_eq!(report.section_now, 4);
        assert!(!report.confirmed, "без confirm");
        assert!(report.verified, "честная квитанция");
    }

    fn comments_tail(marker: &str, entries: &[(&str, &str)]) -> String {
        let mut out = format!("\n\n{marker}\n");
        for (stamp, text) in entries {
            out.push_str(&format!("---\n{stamp}\n{text}\n"));
        }
        out
    }

    #[test]
    fn comments_tail_strip_keeps_body_bytes_with_and_without_trailing_newline() {
        for body in ["## П\n\nтело\n", "## П\n\nтело"] {
            let input = format!(
                "{body}{}",
                comments_tail(
                    "--- comments (latest 2 of 2) ---",
                    &[
                        ("2026-11-24T04:59:18Z", "первый"),
                        ("2026-11-24T05:00:00Z", "второй")
                    ]
                )
            );
            assert_eq!(strip_comments_tail(&input), body, "body: {body:?}");
        }
    }

    #[test]
    fn comments_tail_strip_recognises_legacy_marker() {
        let body = "## П\n\nтело\n";
        let input = format!(
            "{body}{}",
            comments_tail(
                "--- comments (latest) ---",
                &[("2026-11-24T04:59:18Z", "коммент")]
            )
        );
        assert_eq!(strip_comments_tail(&input), body);
    }

    #[test]
    fn comments_tail_strip_keeps_sections_appended_after_dump() {
        for base in ["## П\n\nтело\n", "## П\n\nтело"] {
            let dump = comments_tail(
                "--- comments (latest 1 of 1) ---",
                &[("2026-11-24T04:59:18Z", "коммент")],
            );
            let assembled = format!("{base}{dump}\n## План\n\nновая секция\n");
            let expected = format!("{base}\n## План\n\nновая секция\n");
            assert_eq!(strip_comments_tail(&assembled), expected, "base: {base:?}");
        }
    }

    #[test]
    fn comments_tail_strip_keeps_section_when_heading_inside_comment() {
        let input = "## П\n\nтело\n\n\n--- comments (latest 1 of 1) ---\n---\n2026-11-24T04:59:18Z\nтекст\n\n## Внутри коммента\n\nещё текст\n\n## План\n\nсекция\n";
        let out = strip_comments_tail(input);
        assert!(out.contains("## План\n\nсекция"), "секция цела: {out}");
        assert!(!out.contains("--- comments (latest"), "маркер ушёл: {out}");
    }

    #[test]
    fn comments_tail_strip_keeps_marker_inside_fence() {
        let body =
            "## П\n\n```\n--- comments (latest 1 of 1) ---\n---\n2026-11-24T04:59:18Z\nкод\n```\n";
        assert_eq!(strip_comments_tail(body), body);
    }

    #[test]
    fn comments_tail_strip_continues_after_false_marker() {
        let input = "## П\n\n--- comments (latest) ---\n\nтело\n\n\n--- comments (latest 1 of 1) ---\n---\n2026-11-24T04:59:18Z\nкоммент\n";
        assert_eq!(
            strip_comments_tail(input),
            "## П\n\n--- comments (latest) ---\n\nтело\n"
        );
    }

    #[test]
    fn comments_tail_strip_resets_fence_at_entry_start() {
        let input = "## П\n\nтело\n\n\n--- comments (latest 2 of 2) ---\n---\n2026-11-24T04:59:18Z\n```\nнезакрытый fence\n---\n2026-11-24T05:00:00Z\nвторой\n\n## План\n\nсекция\n";
        let out = strip_comments_tail(input);
        assert!(out.contains("## План\n\nсекция"), "секция цела: {out}");
        assert!(!out.contains("--- comments (latest"), "маркер ушёл: {out}");
    }

    #[test]
    fn comments_tail_strip_cycles_through_stacked_dumps() {
        let old = comments_tail(
            "--- comments (latest) ---",
            &[("2026-11-24T04:59:18Z", "старый")],
        );
        let current = comments_tail(
            "--- comments (latest 1 of 1) ---",
            &[("2026-11-25T04:59:18Z", "текущий")],
        );
        let input = format!("## П\n\nтело\n{old}\n## План\n\nсекция\n{current}");
        assert_eq!(
            strip_comments_tail(&input),
            "## П\n\nтело\n\n## План\n\nсекция\n"
        );
    }

    #[test]
    fn comments_tail_strip_accepts_crlf_entries() {
        let body = "## П\r\n\r\nтело\r\n";
        let input = format!(
            "{body}\r\n\r\n--- comments (latest 1 of 1) ---\r\n---\r\n2026-11-24T04:59:18Z\r\nкоммент\r\n"
        );
        assert_eq!(strip_comments_tail(&input), body);
    }

    #[test]
    fn comments_tail_marker_without_entries_is_kept() {
        let body = "## П\n\n--- comments (latest 1 of 1) ---\n\nцитата в теле\n";
        assert_eq!(strip_comments_tail(body), body);
    }

    #[test]
    fn comments_tail_strip_keeps_section_when_heading_in_first_of_two_entries() {
        let input = "## П\n\nтело\n\n\n--- comments (latest 2 of 2) ---\n---\n2026-11-24T04:59:18Z\nпервый\n\n## Внутри коммента\n\nещё\n---\n2026-11-24T05:00:00Z\nвторой\n\n## План\n\nсекция\n";
        let out = strip_comments_tail(input);
        assert!(out.contains("## План\n\nсекция"), "секция цела: {out}");
        assert!(!out.contains("--- comments (latest"), "маркер ушёл: {out}");
        assert!(
            !out.contains("2026-11-24T04:59:18Z"),
            "первая запись ушла: {out}"
        );
        assert!(!out.contains("первый"), "тело первой записи ушло: {out}");
    }

    #[test]
    fn comments_tail_strip_accepts_crlf_seam_before_section() {
        let body = "## П\r\n\r\nтело\r\n";
        let input = format!(
            "{body}\r\n\r\n--- comments (latest 1 of 1) ---\r\n---\r\n2026-11-24T04:59:18Z\r\nкоммент\r\n\r\n## План\r\n\r\nсекция\r\n"
        );
        let out = strip_comments_tail(&input);
        assert!(
            out.contains("## План\r\n\r\nсекция"),
            "CRLF-стык цел: {out}"
        );
        assert!(!out.contains("--- comments"), "маркер снят: {out}");
    }

    // ---- tester #1648: независимые пины главного сценария и стыка ----

    // На юнит-уровне проверяем сам хелпер: композицию трёх приватных срезов
    // из места вызова (set_body) закрывают интеграционные пины на MCP-поверхности.

    #[test]
    fn tester_copied_card_tail_strips_to_exact_body() {
        let body = "## Постановка\n\nтело для копии.\n";
        let tail = "\n\n--- comments (latest 2 of 2) ---\n---\n2026-11-24T04:59:18Z\nпервый\n\
                    ---\n2026-11-24T05:00:00Z\nвторой\n";
        let copied = format!("{body}{tail}");
        assert_eq!(
            strip_comments_tail(&copied),
            body,
            "копия карточки → тело байт-в-байт"
        );
    }

    #[test]
    fn tester_legacy_marker_is_stripped() {
        let body = "## Постановка\n\nтело.\n";
        let tail = "\n\n--- comments (latest) ---\n---\n2026-11-23T10:00:00Z\nлегаси\n";
        assert_eq!(strip_comments_tail(&format!("{body}{tail}")), body);
    }

    #[test]
    fn tester_seam_exact_bytes_lf_and_crlf() {
        let comment = "\n\n--- comments (latest 1 of 1) ---\n---\n2026-11-24T04:59:18Z\nкоммент\n";
        let comment_crlf = "\r\n\r\n--- comments (latest 1 of 1) ---\r\n---\r\n2026-11-24T04:59:18Z\r\nкоммент\r\n";
        for (base, tail, section, expected) in [
            (
                "## П\n\nтело\n",
                comment,
                "\n## План\n\nсекция\n",
                "## П\n\nтело\n\n## План\n\nсекция\n",
            ),
            (
                "## П\n\nтело",
                comment,
                "\n## План\n\nсекция\n",
                "## П\n\nтело\n## План\n\nсекция\n",
            ),
            (
                "## П\r\n\r\nтело\r\n",
                comment_crlf,
                "\r\n## План\r\n\r\nсекция\r\n",
                "## П\r\n\r\nтело\r\n\r\n## План\r\n\r\nсекция\r\n",
            ),
        ] {
            let assembled = format!("{base}{tail}{section}");
            assert_eq!(strip_comments_tail(&assembled), expected, "base: {base:?}");
        }
    }

    #[test]
    fn tester_clean_body_roundtrip_is_untouched() {
        for body in [
            "## Постановка\n\nчистое тело без ленты.\n",
            "--- comments (latest 1 of 1) ---\n\nцитата в прозе без записей\n",
        ] {
            assert_eq!(
                strip_comments_tail(body),
                body,
                "не трогаем без сигнатуры ленты"
            );
        }
    }
}
