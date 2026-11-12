//! SiYuanStore: реализация TaskStore над существующим доменным слоем
//! (siyuan_task.rs). Поведение — дословно текущий контур; store-слой только
//! переводит SyTask/SyBoard ↔ нейтральные типы. Операции, чья логика живёт
//! в cmd-слое (body_replace, thread, create_cron_run) — доезжают этапом
//! коммутации (после нарезки sy_dispatch, задача 20261112141437-y772wfx);
//! контрактные default'ы громко отказывают, тихих заглушек нет.
//!
//! Write-side операции (create/edit/comment/claim/close/attrs) — делегация
//! к обычным методам из siyuan_task_store_write.rs: trait impl ровно один
//! (несколько impl-блоков одного трейта — конфликт E0119).

use super::task_store::{CommentInsert, CreateTask, EditTask, StoreResult, TaskStore};
use super::{StoreError, Task, TaskBoard, TaskType};
use crate::siyuan::SiYuan;
use crate::siyuan_task as sy_task;
use crate::siyuan_task::{SyBoard, SyTask};

pub struct SiYuanStore {
    pub(super) sy: SiYuan,
    /// Пейсинг вставки строк: константы прод-контура, тесты перекрывают env.
    pub(super) pacing: sy_task::RowInsertPacing,
    /// Автор комментов системного канала (AGENTS) — паритет текущего.
    pub(super) author: String,
}

impl SiYuanStore {
    pub fn new(sy: SiYuan) -> Self {
        Self {
            sy,
            pacing: sy_task::RowInsertPacing::production(),
            author: "AGENTS".to_string(),
        }
    }

    pub(super) async fn board(&self, board: &TaskBoard) -> anyhow::Result<SyBoard> {
        sy_task::find_board(&self.sy, &board.project).await
    }

    pub(super) fn convert(row: &SyTask) -> Task {
        Task {
            id: row.item_id.clone(),
            title: row.title.clone(),
            status: row.status.clone(),
            task_type: TaskType(row.task_type.clone()),
            branch: row.branch.clone(),
            pr: row.pr.clone(),
            dev_session: row.dev_session.clone(),
            planned: row.planned.clone(),
            planned_ms: row.planned_ms,
            schedule_cell: row.schedule_cell.clone(),
        }
    }

    /// Строковая проекция → SyTask доменного слоя (обратная конвертация;
    /// body_doc не несёт контракта — резолв тела идёт по тайтлу/привязке).
    pub(super) async fn row_of(&self, board: &SyBoard, task: &Task) -> anyhow::Result<SyTask> {
        let rows = sy_task::read_rows(&self.sy, board).await?;
        Ok(rows
            .into_iter()
            .find(|r| r.item_id == task.id)
            .ok_or_else(|| StoreError::NotFound {
                needle: task.id.clone(),
            })?)
    }

    /// docId тела задачи (трейтовый body_get/body_attrs/set_body_attr).
    pub(super) async fn body_doc_of(
        &self,
        board: &SyBoard,
        task: &Task,
    ) -> Result<String, StoreError> {
        let row = self.row_of(board, task).await?;
        sy_task::task_body_doc(&self.sy, board, &row)
            .await?
            .ok_or_else(|| StoreError::NotFound {
                needle: format!("тело «{}»", task.title),
            })
    }
}

/// Классификация доменного отказа: «не найдено» → NotFound (фоллбеки
/// резолва), остальное — Other с контекстом.
pub(super) fn not_found_or_other(e: anyhow::Error, needle: &str) -> StoreError {
    let text = format!("{e:#}");
    if text.contains("не найдена") || text.contains("не найдено") {
        StoreError::NotFound {
            needle: needle.to_string(),
        }
    } else {
        StoreError::Other(e.context(text))
    }
}

pub(super) fn dup_error(title: &str) -> StoreError {
    StoreError::DuplicateTitle {
        title: title.to_string(),
        id: String::new(),
    }
}

#[async_trait::async_trait]
impl TaskStore for SiYuanStore {
    async fn list(&self, board: &TaskBoard) -> StoreResult<Vec<Task>> {
        let board = self.board(board).await?;
        let rows = sy_task::read_rows(&self.sy, &board).await?;
        Ok(rows.iter().map(Self::convert).collect())
    }

    async fn resolve(&self, board: &TaskBoard, needle: &str) -> Result<Task, StoreError> {
        sy_task::resolve_on_project(&self.sy, &board.project, needle)
            .await
            .map(|(_, row)| Self::convert(&row))
            .map_err(|e| not_found_or_other(e, needle))
    }

    async fn body_get(&self, board: &TaskBoard, task: &Task) -> StoreResult<String> {
        let board = self.board(board).await?;
        let doc_id = self.body_doc_of(&board, task).await?;
        Ok(self.sy.export_md_by_id(&doc_id).await?)
    }

    async fn body_attrs(
        &self,
        board: &TaskBoard,
        task: &Task,
    ) -> StoreResult<std::collections::HashMap<String, String>> {
        let board = self.board(board).await?;
        let doc_id = self.body_doc_of(&board, task).await?;
        Ok(self.sy.get_block_attrs(&doc_id).await?)
    }

    async fn discover_boards(&self) -> StoreResult<Vec<TaskBoard>> {
        let boards = sy_task::discover_task_boards(&self.sy).await?;
        Ok(boards
            .iter()
            .map(|b| TaskBoard {
                project: sy_task::board_project(b),
            })
            .collect())
    }

    async fn create(&self, board: &TaskBoard, input: &CreateTask) -> Result<Task, StoreError> {
        self.create_impl(board, input).await
    }

    async fn edit(&self, board: &TaskBoard, task: &Task, patch: &EditTask) -> StoreResult<Task> {
        self.edit_impl(board, task, patch).await
    }

    async fn comment(
        &self,
        board: &TaskBoard,
        task: &Task,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<CommentInsert, StoreError> {
        self.comment_impl(board, task, text, reply_to).await
    }

    async fn claim(&self, board: &TaskBoard, task: &Task, session: &str) -> StoreResult<Task> {
        self.claim_impl(board, task, session).await
    }

    async fn close(&self, board: &TaskBoard, task: &Task, note: Option<&str>) -> StoreResult<Task> {
        self.close_impl(board, task, note).await
    }

    async fn set_body_attr(
        &self,
        board: &TaskBoard,
        task: &Task,
        key: &str,
        value: &str,
    ) -> StoreResult<()> {
        self.set_body_attr_impl(board, task, key, value).await
    }
}
