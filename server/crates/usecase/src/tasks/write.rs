//! Write-юзкейсы задач: create/edit/claim/comment/close/body.
//! Канон cli: тайтл уникален на доске, create идемпотентен по тайтлу,
//! fm ≡ поля (двойная запись держится портом), комменты — issue comments.

use crate::error::{UsecaseError, UsecaseResult};
use crate::naming;
use crate::ports::{Clock, CommentRef, IssueFieldStore, TaskBoardRepo, TickStateStore};
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
/// Честная квитанция (#1331): verified=true — тело перечитано с носителя,
/// hash/size от прочитанного; verified=false — носитель не прочитан
/// (деградация контрольного чтения), hash/size от отправленного тела.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyWriteReceipt {
    pub body_hash: String,
    pub body_size: usize,
    pub verified: bool,
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
    /// тело перечитано с носителя после записи (честная квитанция #1331).
    pub verified: bool,
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
    /// Полная замена тела (fm и дисплей-шапка task_get на границе
    /// срезаются — тела без fm, поля нативные и от тела не зависят);
    /// возвращает честную квитанцию: контрольное чтение носителя после
    /// записи (#1331) — hash/size от прочитанного, либо verified=false.
    async fn set_body(
        &self,
        project: &str,
        number: u64,
        body_md: &str,
    ) -> UsecaseResult<BodyWriteReceipt>;
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
                    "REST-поиск тайтлов недоступен — холодный гард пропущен (первичный снапшот-гард отработал)"
                );
                Vec::new()
            }
        };
        if !cold.is_empty() {
            return Err(UsecaseError::Board(format!(
                "тайтл «{}» уже занят issue #{} — дубль запрещён (строка вне снапшота: холодный ящик)",
                spec.title.trim(),
                cold[0]
            )));
        }
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
        // Ожидания контрольного чтения носителя (честная квитанция #1331):
        // после всех записей один read_issue сверяет всё изменённое.
        let mut want_title: Option<String> = None;
        let mut want_branch: Option<String> = None;
        let mut want_pr: Option<String> = None;
        let mut want_priority: Option<String> = None;
        let mut want_automerge: Option<bool> = None;
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
                        "REST-поиск тайтлов недоступен при rename — холодный гард пропущен"
                    );
                    Vec::new()
                }
            };
            if let Some(other) = cold.iter().find(|n| **n != number) {
                return Err(UsecaseError::Board(format!(
                    "тайтл «{title}» уже занят issue #{other} — дубль запрещён (строка вне снапшота: холодный ящик)"
                )));
            }
            self.board.rename_issue(number, title).await?;
            want_title = Some(title.clone());
        }
        // ветка/PR — нативные поля issue (в снапшоте не живут)
        let mut field_patch: Vec<(&str, Option<String>)> = Vec::new();
        if let Some(b) = &patch.branch {
            let name = uwuwu_domain::BranchName::parse(b).ok_or_else(|| {
                UsecaseError::Board(format!("ветка «{b}» не валидна (пробелы/глоб-символы)"))
            })?;
            field_patch.push(("Branch", Some(name.as_str().to_string())));
            want_branch = Some(name.as_str().to_string());
        }
        if let Some(p) = &patch.pr {
            let url = uwuwu_domain::PrUrl::parse(p)
                .ok_or_else(|| UsecaseError::Board(format!("PR «{p}» не http(s)-URL")))?;
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
                .set_board_cells(&board, number, patch.priority.as_deref(), patch.automerge)
                .await?;
            want_priority = patch.priority.clone();
            want_automerge = patch.automerge;
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
            },
        )
        .await
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
                    "коммент #{number}: пост не подтвердился чтением ленты носителя — \
                     не ретраить слепо (риск дубля), сверь task_get"
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
        // доске волны, не только на первой (пин живого прогона)
        for (_, board) in &self.boards {
            let rows = self.board.read_board(board).await?;
            if rows.iter().any(|t| t.number() == number) {
                self.board.set_status(board, number, Status::Done).await?;
                return self.board.close_issue_ext(number, not_planned).await;
            }
        }
        Err(UsecaseError::Board(format!(
            "строка #{number} не на досках конфига"
        )))
    }

    async fn set_body(
        &self,
        project: &str,
        number: u64,
        body_md: &str,
    ) -> UsecaseResult<BodyWriteReceipt> {
        // инвариант на границе: тело без fm и без дисплей-шапки task_get
        // (агенты могут копировать старый канон — сервер держит «fm снесён»
        // сам, #1151; fm-стек инцидента #1331)
        let clean = crate::fm_lite::strip(strip_task_get_header(body_md));
        let board = self.board_of(project);
        self.board.replace_body(number, clean).await?;
        // контрольное чтение носителя: квитанция = факт доставки (#1331)
        let repo = self.board.clone();
        let bd = board.clone();
        let expected = clean.to_string();
        // future владеет копиями захваченного — ссылки из тела FnMut-
        // замыкания не ускользают (lifetime-гард компилятора)
        match verify_carrier(
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
                    "тело #{number}: запись не подтвердилась чтением носителя — \
                     GitHub отдаёт не записанное; не ретраить слепо, сверь task_get"
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
                    body_hash: md_section::body_hash(clean),
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
        let receipt = self.set_body(project, number, &next).await?;
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
            verified: receipt.verified,
        })
    }
}

/// Контрольное чтение носителя после успешной мутации (честная квитанция
/// #1331, ADR-093): мутация отвечает успехом только как факт доставки.
/// В аномальных окнах носителя (18.44–18.52Z 18.11) эхо-PATCH проходило
/// при неприменившейся записи — контрольное чтение ловит то, что эхо
/// пропускает; сильнее на Issues API ничего нет (If-Match не поддерживается).
const VERIFY_ATTEMPTS: usize = 3;

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

/// До VERIFY_ATTEMPTS попыток чтения носителя. Без пауз между попытками:
/// аномальные окна носителя имеют минутный масштаб (секундная пауза их не
/// покрывает — прецедент verify_status с паузой 1с обслуживает лаг Pv2-
/// снапшота в адаптере, здесь чтения issue-поверхности REST).
/// Последняя попытка решает исход: Ok-мимо — Stale, Err — Blind.
async fn verify_carrier<T, Fut, F, M>(mut read: F, matches: M) -> Delivery<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, UsecaseError>>,
    M: Fn(&T) -> bool,
{
    let mut stale = false;
    for _ in 0..VERIFY_ATTEMPTS {
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

/// Дисплей-шапка task_get («--- fm ---» + branch/pr/automerge/body_hash)
/// — рендер MCP-карточки, не носитель: агенты копируют вывод task_get
/// назад в task_body, и шапка наслаивалась в теле (fm-стек инцидента
/// #1331). Срезается на границе записи: инвариант ADR-105 «тело — чистый
/// markdown» держит сервер, не дисциплина агента (прецедент #1151).
/// Консервативно: строго набор/порядок ключей рендера, любой посторонний
/// контент («--- fm ---» внутри текста) не трогается.
fn strip_task_get_header(md: &str) -> &str {
    const KEYS: [&str; 4] = ["branch: ", "pr: ", "automerge: ", "body_hash: "];
    let Some(rest) = md.strip_prefix("--- fm ---\n") else {
        return md;
    };
    let mut pos = rest;
    for key in KEYS {
        let Some(line_end) = pos.find('\n') else {
            return md;
        };
        if !pos[..line_end].starts_with(key) {
            return md;
        }
        pos = &pos[line_end + 1..];
    }
    pos.trim_start_matches('\n')
}

/// Ожидания контрольного чтения edit: Some — значение менялось и должно
/// присутствовать на носителе; None — не менялось и не сверяется.
struct EditExpectations {
    title: Option<String>,
    branch: Option<String>,
    pr: Option<String>,
    priority: Option<String>,
    automerge: Option<bool>,
}

impl EditExpectations {
    fn is_empty(&self) -> bool {
        matches!(
            (&self.title, &self.branch, &self.pr, &self.priority, self.automerge),
            (None, None, None, None, None)
        )
    }

    /// Несошедшие ключи последнего чтения — в текст громкой ошибки.
    fn mismatches(&self, view: &crate::ports::IssueView) -> Vec<&'static str> {
        let mut out = Vec::new();
        if let Some(t) = &self.title {
            if view.task.title() != t {
                out.push("тайтл");
            }
        }
        if let Some(b) = &self.branch {
            if view.task.branch().map(|x| x.as_str()) != Some(b.as_str()) {
                out.push("ветка");
            }
        }
        if let Some(u) = &self.pr {
            if view.task.pr().map(|x| x.as_str()) != Some(u.as_str()) {
                out.push("PR");
            }
        }
        if let Some(p) = &self.priority {
            if view.task.priority() != crate::naming::parse_priority(p) {
                out.push("приоритет");
            }
        }
        if let Some(a) = self.automerge {
            if view.task.automerge() != a {
                out.push("automerge");
            }
        }
        out
    }
}

impl TaskWriteUseCase {
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
        let matches = move |v: &crate::ports::IssueView| want.mismatches(v).is_empty();
        match verify_carrier(
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
                tracing::error!(
                    event = "write.delivery_mismatch",
                    kind = "edit",
                    number,
                    "Запись edit не подтвердилась контрольным чтением носителя"
                );
                Err(UsecaseError::Board(format!(
                    "edit #{number}: запись не подтвердилась чтением носителя \
                     (тайтл/ветка/PR/приоритет/automerge) — не ретраить слепо, сверь task_get"
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
        crate::fm_lite::strip(strip_task_get_header(&spec.body_md)).to_string()
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
    #[derive(Default, Clone)]
    struct Carrier {
        title: String,
        body: String,
        comments: Vec<CommentRef>,
        branch: Option<String>,
        pr: Option<String>,
        priority: Option<String>,
        automerge: bool,
    }

    impl Carrier {
        /// Агрегат для IssueView: поля собираются так же, как адаптер
        /// собирает их из issue-носителя (map_task_fields-семантика).
        fn view(&self, number: u64) -> IssueView {
            let mut task = Task::new(number, self.title.clone(), Status::Empty);
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
            ));
            task.set_automerge(self.automerge);
            IssueView {
                task,
                body: self.body.clone(),
                comments: self.comments.clone(),
            }
        }
    }

    /// Запись ячеек снапшота: номер + приоритет + automerge (#1258).
    type CellWrite = (u64, Option<String>, Option<bool>);

    #[async_trait]
    impl TaskBoardRepo for FakeBoard {
        async fn row_status(&self, _board: &str, _number: u64) -> UsecaseResult<Option<Status>> {
            Ok(None)
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
            if !self.phantom {
                let mut c = self.carrier.lock().expect("fake carrier");
                c.priority = p.map(str::to_string);
                if let Some(a) = a {
                    c.automerge = a;
                }
            }
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
            branch: &uwuwu_domain::BranchName,
        ) -> UsecaseResult<()> {
            if !self.phantom {
                self.carrier.lock().expect("fake carrier").branch =
                    Some(branch.as_str().to_string());
            }
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
        ) -> UsecaseResult<u64> {
            Ok(1)
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

    /// Дисплей-шапка task_get срезается на границе записи (fm-стек
    /// инцидента #1331: агент копирует вывод task_get назад в task_body).
    #[test]
    fn task_get_display_header_is_stripped() {
        let md = "--- fm ---\nbranch: work/task-1331\npr: —\nautomerge: true\nbody_hash: 9de430c691c92328\n\n## Постановка\nтекст";
        assert_eq!(strip_task_get_header(md), "## Постановка\nтекст");
    }

    #[test]
    fn task_get_header_in_middle_is_kept() {
        let md =
            "## Секция\n\n--- fm ---\nbranch: x\npr: y\nautomerge: true\nbody_hash: h\n\nтекст";
        assert_eq!(
            strip_task_get_header(md),
            md,
            "«шапка» в середине тела — не наша граница"
        );
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
    }

    // --- Честные квитанции: контрольное чтение носителя (#1331) ---

    /// Красный класс инцидента #1331: мутация отвечает успехом, носитель
    /// не меняется — task_body обязан отказывать громко, а не рапортовать
    /// квитанцию о недоставленном теле.
    #[tokio::test]
    async fn phantom_body_write_is_loud() {
        let u = writecase_carrier(Arc::new(FakeFields::default()), true, false);
        let err = u
            .set_body("uwuwu", 1, "## Новое тело\n")
            .await
            .expect_err("фантомная запись тела — громкий отказ");
        assert!(
            err.to_string().contains("не подтвердилась"),
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
            err.to_string().contains("не подтвердился"),
            "текст отказа: {err}"
        );
    }

    /// edit с полями: ветка записана, носитель её не видит — громко.
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
        assert!(
            err.to_string().contains("не подтвердилась"),
            "текст отказа: {err}"
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
            err.to_string().contains("не подтвердилась"),
            "текст отказа: {err}"
        );
    }

    /// Blind-ветка (ADR-093): носитель не читается — успех с непроверенной
    /// квитанцией от отправленного тела; ретрай мутации не навязывается.
    #[tokio::test]
    async fn blind_body_warns_and_reports_sent_body() {
        let u = writecase_carrier(Arc::new(FakeFields::default()), false, true);
        let r = u.set_body("uwuwu", 1, "## Тело\n").await.unwrap();
        assert!(!r.verified, "непроверенная квитанция");
        assert_eq!(r.body_hash, md_section::body_hash("## Тело\n"));
        assert_eq!(r.body_size, "## Тело\n".len());
    }

    /// Честный носитель: квитанция от прочитанного с носителя.
    #[tokio::test]
    async fn honest_body_receipt_carries_carrier_body() {
        let u = writecase_carrier(Arc::new(FakeFields::default()), false, false);
        let r = u.set_body("uwuwu", 1, "## Тело\n").await.unwrap();
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
}
