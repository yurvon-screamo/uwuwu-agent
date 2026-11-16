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
    pub autodone: bool,
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
        // приоритет/тип — нативные поля issue (fm снесён)
        let kind = spec.kind.unwrap_or(uwuwu_domain::TaskType::Feature);
        let mut field_patch: Vec<(&str, Option<String>)> =
            vec![("Тип", Some(naming::task_type_name(kind).to_string()))];
        if !spec.priority.is_empty() {
            field_patch.push((naming::F_PRIORITY, Some(spec.priority.clone())));
        }
        self.fields.store(number, &field_patch).await?;
        Ok(number)
    }

    async fn edit(&self, project: &str, number: u64, patch: TaskPatch) -> UsecaseResult<()> {
        let board = self.board_of(project);
        if board.is_empty() {
            return Err(UsecaseError::Board(format!(
                "проект «{project}» вне конфига"
            )));
        }
        let need_rows = patch.status.is_some() || patch.title.is_some();
        let rows = if need_rows {
            self.board.read_board(&board).await?
        } else {
            Vec::new()
        };
        if let Some(status) = patch.status {
            // канон переходов — домен: запрещённое ребро = громкая ошибка
            let current = rows
                .iter()
                .find(|t| t.number() == number)
                .map(|t| t.status())
                .ok_or_else(|| {
                    UsecaseError::Board(format!("строка #{number} не на доске «{board}»"))
                })?;
            if !uwuwu_domain::Status::can_transition(current, status) {
                return Err(UsecaseError::Board(format!(
                    "переход {:?} → {:?} вне канона конвейера",
                    current, status
                )));
            }
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
        // ветка/PR/приоритет — нативные поля issue (value-объекты валидируют)
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
        if let Some(pr) = &patch.priority {
            field_patch.push((naming::F_PRIORITY, Some(pr.clone())));
        }
        if !field_patch.is_empty() {
            self.fields.store(number, &field_patch).await?;
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

    fn spec(body: &str) -> NewTask {
        NewTask {
            title: "t".into(),
            body_md: body.into(),
            kind: None,
            priority: String::new(),
            autodone: true,
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
