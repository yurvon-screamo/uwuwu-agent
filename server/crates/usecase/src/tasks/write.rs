//! Write-юзкейсы задач: create/edit/claim/comment/close/body.
//! Канон cli: тайтл уникален на доске, create идемпотентен по тайтлу,
//! fm ≡ поля (двойная запись держится портом), комменты — issue comments.

use crate::error::{UsecaseError, UsecaseResult};
use crate::naming;
use crate::ports::{Clock, IssueFieldStore, TaskBoardRepo, TickStateStore};
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyWriteReceipt {
    pub body_hash: String,
    pub body_size: usize,
}

/// Квитанция секционной правки (#1152): исход + тело после записи.
/// Полное тело не возвращается — сознательное сужение: возврат больших
/// тел убивал бы экономию секционного пути; порча ловится точечным
/// перечитыванием task_get по подозрению.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionEditReport {
    pub action: SectionAction,
    /// снятые одноимённые дубли на уровне пина (хилинг).
    pub duplicates_removed: usize,
    /// контент секции был пуст — секция очищена (заголовок жив).
    pub cleared: bool,
    /// FNV-1a 64 тела после записи (как записано в GitHub).
    pub body_hash: String,
    /// размер тела после записи, байт.
    pub body_size: usize,
}

#[async_trait]
pub trait TaskWriter: Send + Sync {
    /// Создание: Analys (create-дефолт #1135), лейбл proj:<project>,
    /// fm-скелет; дубль тайтла — ошибка.
    async fn create(&self, project: &str, spec: NewTask) -> UsecaseResult<u64>;
    /// Правка: статус/ветка/PR/приоритет/тайтл (статус — поле доски,
    /// ветка/PR/приоритет — нативные поля issue, тайтл — rename).
    async fn edit(&self, project: &str, number: u64, patch: TaskPatch) -> UsecaseResult<()>;
    /// Заявка сессии в слот (dev — по умолчанию).
    async fn claim(&self, number: u64, session: &str, slot: Option<Slot>) -> UsecaseResult<()>;
    /// Коммент (reply — цитатный якорь).
    async fn comment(&self, number: u64, text: &str, reply_to: Option<&str>) -> UsecaseResult<()>;
    /// Закрытие: Status=Done + issue closed (reason: completed|not_planned).
    /// Доска строки резолвится по всему конфигу (мультиборд, #1135).
    async fn close(&self, number: u64, not_planned: bool) -> UsecaseResult<()>;
    /// Полная замена тела (fm на границе стрипается — тела без fm, поля
    /// нативные и от тела не зависят); возвращает квитанцию записанного.
    async fn set_body(&self, number: u64, body_md: &str) -> UsecaseResult<BodyWriteReceipt>;
    /// Секционная правка тела (#1152): агент передаёт дельту секции, сервер
    /// собирает тело сам — большое тело не гоняется через контекст агента.
    /// Свежесть: base_hash (body_hash из fm task_get/квитанции) против
    /// текущего тела; мимо — громкий отказ с актуальным хэшем, записи нет
    /// (lost-update вместо молчаливого затирания; TOCTOU-окно сверка→PATCH
    /// неустранимо на GitHub Issues API — гран владельца).
    async fn set_body_section(
        &self,
        project: &str,
        number: u64,
        section: &str,
        content: &str,
        base_hash: &str,
    ) -> UsecaseResult<SectionEditReport>;
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
                "тайтл «{title}» уже есть на доске — дубль запрещён"
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
            "переход из Analys #{number}: не заполнены обязательные поля — {}; \
             заполни (task_edit priority / automerge) и повтори переход",
            empty.join(", ")
        )))
    }
}

#[async_trait]
impl TaskWriter for TaskWriteUseCase {
    async fn create(&self, project: &str, spec: NewTask) -> UsecaseResult<u64> {
        if spec.title.trim().is_empty() {
            return Err(UsecaseError::Board("тайтл пуст".into()));
        }
        let board = self.board_of(project);
        if board.is_empty() {
            return Err(UsecaseError::Board(format!(
                "проект «{project}» вне конфига"
            )));
        }
        let rows = self.board.read_board(&board).await?;
        self.dup_check(&rows, spec.title.trim())?;
        let number = self
            .board
            .create_issue(
                &board,
                project,
                spec.title.trim(),
                &render_create_body(&spec),
            )
            .await?;
        // тип — нативное поле issue (fm снесён); Тип в снапшоте не живёт
        let kind = spec.kind.unwrap_or(uwuwu_domain::TaskType::Feature);
        self.fields
            .store(
                number,
                &[("Тип", Some(naming::task_type_name(kind).to_string()))],
            )
            .await?;
        // Приоритет/Automerge — ячейки снапшота доски: единая точка записи
        // board-адаптера с кеш-гигиеной (#1258; add_to_board инвалидирует
        // ДО записи полей — окно кеша без полей закрывается адаптером).
        // Автомерж пишется всегда — состояние на носителе явно (канон #1232)
        let priority = if spec.priority.is_empty() {
            None
        } else {
            Some(spec.priority.as_str())
        };
        self.board
            .set_board_cells(&board, number, priority, Some(spec.automerge))
            .await?;
        Ok(number)
    }

    async fn edit(&self, project: &str, number: u64, patch: TaskPatch) -> UsecaseResult<()> {
        let board = self.board_of(project);
        if board.is_empty() {
            return Err(UsecaseError::Board(format!(
                "проект «{project}» вне конфига"
            )));
        }
        let need_rows = patch.title.is_some();
        let rows = if need_rows {
            self.board.read_board(&board).await?
        } else {
            Vec::new()
        };
        if let Some(status) = patch.status {
            // канон переходов — домен: запрещённое ребро = громкая ошибка.
            // current — свежий статус носителя, не снапшот доски: стэйл-
            // снапшот пропускал нелегальную перезапись как легальную,
            // строка уезжала в Analys посреди конвейера (#1235)
            let current = match self.board.row_status(&board, number).await? {
                None => {
                    return Err(UsecaseError::Board(format!(
                        "строка #{number} не на доске «{board}» \
                         (статус на носителе отсутствует)"
                    )))
                }
                Some(Status::Empty) => {
                    return Err(UsecaseError::Board(format!(
                        "статус носителя #{number} не читается — запись без \
                         верифицированного current запрещена (класс разбираемого \
                         бага #1235), разбор владельца"
                    )))
                }
                Some(current) => current,
            };
            if !uwuwu_domain::Status::can_transition(current, status) {
                tracing::warn!(
                    event = "board.guard_rejected",
                    number,
                    from = %naming::status_name(current),
                    to = %naming::status_name(status),
                    "Гард канона: переход запрещён (fresh row_status)"
                );
                return Err(UsecaseError::Board(format!(
                    "переход {:?} → {:?} вне канона конвейера",
                    current, status
                )));
            }
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
            self.board.set_status(&board, number, status).await?;
        }
        if let Some(title) = &patch.title {
            if rows
                .iter()
                .any(|t| t.number() != number && t.title() == title)
            {
                return Err(UsecaseError::Board(format!(
                    "тайтл «{title}» уже занят другой строкой"
                )));
            }
            self.board.rename_issue(number, title).await?;
        }
        // ветка/PR — нативные поля issue (в снапшоте не живут)
        let mut field_patch: Vec<(&str, Option<String>)> = Vec::new();
        if let Some(b) = &patch.branch {
            let name = uwuwu_domain::BranchName::parse(b).ok_or_else(|| {
                UsecaseError::Board(format!("ветка «{b}» не валидна (пробелы/глоб-символы)"))
            })?;
            field_patch.push(("Branch", Some(name.as_str().to_string())));
        }
        if let Some(p) = &patch.pr {
            let url = uwuwu_domain::PrUrl::parse(p)
                .ok_or_else(|| UsecaseError::Board(format!("PR «{p}» не http(s)-URL")))?;
            field_patch.push(("PR", Some(url.as_str().to_string())));
        }
        if !field_patch.is_empty() {
            self.fields.store(number, &field_patch).await?;
        }
        // Приоритет/Automerge — ячейки снапшота доски: единая точка записи
        // board-адаптера с кеш-гигиеной (read-your-writes #1258); мимо неё
        // значения протухали в снапшоте до TTL (проба на проде 18.11)
        if patch.priority.is_some() || patch.automerge.is_some() {
            self.board
                .set_board_cells(&board, number, patch.priority.as_deref(), patch.automerge)
                .await?;
        }
        Ok(())
    }

    async fn claim(&self, number: u64, session: &str, slot: Option<Slot>) -> UsecaseResult<()> {
        let resolved = slot.unwrap_or(uwuwu_domain::Slot::Dev);
        self.board.set_session("", number, resolved, session).await
    }

    async fn comment(&self, number: u64, text: &str, reply_to: Option<&str>) -> UsecaseResult<()> {
        if text.trim().is_empty() {
            return Err(UsecaseError::Board("коммент пуст".into()));
        }
        let body = match reply_to {
            Some(anchor) => format!("> {anchor}\n\n{text}"),
            None => text.to_string(),
        };
        let id = self.board.post_comment(number, &body).await?;
        // Единственная точка маркировки реестра серверных постов (#1204):
        // агентские комменты не будят держащую сессию. Фабричные посты
        // (сталл-пинг, bounce, эскалация) идут мимо — всегда foreign.
        // Пустой id — маркировки нет (fail-open: возможна разовая эхо-будка).
        if !id.is_empty() {
            self.state.mark_own_comment(&id, self.clock.now_ms());
        }
        Ok(())
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
            "строка #{number} не на досках конфига"
        )))
    }

    async fn set_body(&self, number: u64, body_md: &str) -> UsecaseResult<BodyWriteReceipt> {
        // инвариант на границе: тело без fm (агенты могут копировать
        // старый канон — сервер держит «fm снесён» сам, #1151)
        let clean = crate::fm_lite::strip(body_md);
        self.board.replace_body(number, clean).await?;
        Ok(BodyWriteReceipt {
            body_hash: md_section::body_hash(clean),
            body_size: clean.len(),
        })
    }

    async fn set_body_section(
        &self,
        project: &str,
        number: u64,
        section: &str,
        content: &str,
        base_hash: &str,
    ) -> UsecaseResult<SectionEditReport> {
        if section.trim().is_empty() {
            return Err(UsecaseError::Board("секция пуста — укажи заголовок".into()));
        }
        if base_hash.trim().is_empty() {
            return Err(UsecaseError::Board(
                "base_hash обязателен (body_hash из fm task_get)".into(),
            ));
        }
        let board = self.board_of(project);
        if board.is_empty() {
            return Err(UsecaseError::Board(format!(
                "проект «{project}» вне конфига"
            )));
        }
        let current = self.board.read_issue(&board, number).await?;
        let actual = md_section::body_hash(&current.body);
        if actual != base_hash {
            return Err(UsecaseError::Board(format!(
                "тело #{number} изменилось с момента чтения (base_hash мимо); \
                 актуальный body_hash: {actual} — перечитай task_get и повтори"
            )));
        }
        let (next, edit) = md_section::replace_md_section(&current.body, section, content)
            .map_err(UsecaseError::Board)?;
        let receipt = self.set_body(number, &next).await?;
        let (action, duplicates_removed) = match edit {
            md_section::SectionEdit::Replaced { duplicates_removed } => {
                (SectionAction::Replaced, duplicates_removed)
            }
            md_section::SectionEdit::Appended => (SectionAction::Appended, 0),
        };
        Ok(SectionEditReport {
            cleared: action == SectionAction::Replaced && content.trim().is_empty(),
            action,
            duplicates_removed,
            body_hash: receipt.body_hash,
            body_size: receipt.body_size,
        })
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
        crate::fm_lite::strip(&spec.body_md).to_string()
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
        }
    }

    /// Мок доски: доска пуста, issue создаётся с номером 1; вызовы
    /// set_board_cells логируются — юзкейс обязан маршрутизировать
    /// Приоритет/Automerge в board-порт (read-your-writes #1258),
    /// а не в fields.store (там эти ячейки снапшота протухали).
    #[derive(Default)]
    struct FakeBoard {
        cells: std::sync::Mutex<Vec<CellWrite>>,
    }

    /// Запись ячеек снапшота: номер + приоритет + automerge (#1258).
    type CellWrite = (u64, Option<String>, Option<bool>);

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
        ) -> UsecaseResult<()> {
            self.cells
                .lock()
                .expect("fake board")
                .push((n, p.map(str::to_string), a));
            Ok(())
        }
        async fn set_status(&self, _b: &str, _n: u64, _s: Status) -> UsecaseResult<()> {
            Ok(())
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
            _branch: &uwuwu_domain::BranchName,
        ) -> UsecaseResult<()> {
            Ok(())
        }
        async fn post_comment(&self, _n: u64, _body: &str) -> UsecaseResult<String> {
            Ok(String::new())
        }
        async fn close_issue(&self, _n: u64) -> UsecaseResult<()> {
            Ok(())
        }
        async fn latest_comments(&self, _n: u64) -> UsecaseResult<Vec<CommentRef>> {
            Ok(Vec::new())
        }
        async fn read_issue(&self, _b: &str, _n: u64) -> UsecaseResult<IssueView> {
            panic!("create не читает issue");
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
        ) -> UsecaseResult<u64> {
            Ok(1)
        }
        async fn rename_issue(&self, _n: u64, _t: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn patch_cells(&self, _n: u64, _m: FmMutator) -> UsecaseResult<()> {
            Ok(())
        }
        async fn close_issue_ext(&self, _n: u64, _np: bool) -> UsecaseResult<()> {
            Ok(())
        }
        async fn replace_body(&self, _n: u64, _b: &str) -> UsecaseResult<()> {
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
        let board = Arc::new(FakeBoard::default());
        let u = TaskWriteUseCase {
            board: board.clone(),
            fields,
            boards: vec![("uwuwu".into(), "Задачи uwuwu".into())],
            clock: Arc::new(crate::pipeline::tick_tests::FixedClock),
            state: Arc::new(crate::pipeline::tick_tests::MemState::empty()),
        };
        (u, board)
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
            vec![(1, None, Some(true))],
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
            vec![(1, None, Some(false))],
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
            vec![(1, Some("low".into()), Some(true))],
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
            vec![(1, None, Some(false))],
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
            vec![(1, Some("high".into()), None)],
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
}
