//! Write-юзкейсы задач: create/edit/claim/comment/close/body.
//! Канон cli: тайтл уникален на доске, create идемпотентен по тайтлу,
//! fm ≡ поля (двойная запись держится портом), комменты — issue comments.

use crate::error::{UsecaseError, UsecaseResult};
use crate::ports::TaskBoardRepo;
use async_trait::async_trait;
use std::sync::Arc;
use uwuwu_domain::{Status, Task};

/// Параметры создания задачи (fm-канон).
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    pub title: String,
    pub body_md: String,
    pub kind: String,
    pub priority: String,
    pub autodone: bool,
}

#[async_trait]
pub trait TaskWriter: Send + Sync {
    /// Создание: Draft, лейбл proj:<project>, fm-скелет; дубль тайтла — ошибка.
    async fn create(&self, project: &str, spec: NewTask) -> UsecaseResult<u64>;
    /// Правка ячеек: статус/ветка/PR/приоритет/тайтл (двойная запись).
    async fn edit(&self, number: u64, patch: TaskPatch) -> UsecaseResult<()>;
    /// Заявка сессии в слот dev.
    async fn claim(&self, number: u64, session: &str) -> UsecaseResult<()>;
    /// Коммент (reply — цитатный якорь).
    async fn comment(&self, number: u64, text: &str, reply_to: Option<&str>) -> UsecaseResult<()>;
    /// Закрытие: Status=Done + issue closed (reason: completed|not_planned).
    async fn close(&self, number: u64, not_planned: bool) -> UsecaseResult<()>;
    /// Полная замена тела (fm-поля сохраняются/обновляются из нового fm).
    async fn set_body(&self, number: u64, body_md: &str) -> UsecaseResult<()>;
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
    pub board_name: Arc<String>,
}

impl TaskWriteUseCase {
    fn dup_check(&self, rows: &[Task], title: &str) -> UsecaseResult<()> {
        if rows.iter().any(|t| t.title == title) {
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
        let rows = self.board.read_board(&self.board_name).await?;
        self.dup_check(&rows, spec.title.trim())?;
        self.board
            .create_issue(
                &self.board_name,
                project,
                spec.title.trim(),
                &render_create_body(&spec),
            )
            .await
    }

    async fn edit(&self, number: u64, patch: TaskPatch) -> UsecaseResult<()> {
        let board = &self.board_name;
        if let Some(status) = patch.status {
            self.board.set_status(board, number, status).await?;
        }
        if let Some(title) = &patch.title {
            self.board.rename_issue(number, title).await?;
        }
        let (branch, pr, priority) = (
            patch.branch.clone(),
            patch.pr.clone(),
            patch.priority.clone(),
        );
        self.board
            .patch_cells(
                number,
                Box::new(move |fields| {
                    if let Some(b) = branch {
                        fields.insert("branch".into(), b);
                    }
                    if let Some(p) = pr {
                        fields.insert("pr".into(), p);
                    }
                    if let Some(pr) = priority {
                        fields.insert("priority".into(), pr);
                    }
                }),
            )
            .await
    }

    async fn claim(&self, number: u64, session: &str) -> UsecaseResult<()> {
        self.board
            .set_session(&self.board_name, number, uwuwu_domain::Slot::Dev, session)
            .await
    }

    async fn comment(&self, number: u64, text: &str, reply_to: Option<&str>) -> UsecaseResult<()> {
        if text.trim().is_empty() {
            return Err(UsecaseError::Board("коммент пуст".into()));
        }
        let body = match reply_to {
            Some(anchor) => format!("> {anchor}\n\n{text}"),
            None => text.to_string(),
        };
        self.board.post_comment(number, &body).await
    }

    async fn close(&self, number: u64, not_planned: bool) -> UsecaseResult<()> {
        self.board
            .set_status(&self.board_name, number, Status::Done)
            .await?;
        self.board.close_issue_ext(number, not_planned).await
    }

    async fn set_body(&self, number: u64, body_md: &str) -> UsecaseResult<()> {
        self.board.replace_body(number, body_md).await
    }
}

/// fm-скелет новой задачи (канон create: Draft + дефолты).
fn render_create_body(spec: &NewTask) -> String {
    let safe_title: String = spec.title.chars().take(120).collect();
    format!(
        "---\nstatus: Draft\npriority: {}\nautodone: {}\ntype: {}\n---\n\n## Постановка\n\n{}\n",
        if spec.priority.is_empty() {
            "medium"
        } else {
            &spec.priority
        },
        spec.autodone,
        if spec.kind.is_empty() {
            "feature"
        } else {
            &spec.kind
        },
        if spec.body_md.is_empty() {
            safe_title
        } else {
            spec.body_md.clone()
        },
    )
}
