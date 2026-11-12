//! SiYuanStore: write-side операции (create/edit/comment/claim/close/attrs)
//! как обычные методы impl — единый трейт-impl с делегацией живёт в
//! siyuan_task_store.rs (E0119: два impl-блока одного трейта запрещены).

use super::siyuan_task_store::{dup_error, not_found_or_other, SiYuanStore};
use super::task_store::{CommentInsert, CreateTask, EditTask};
use super::task_store::StoreResult;
use super::{StoreError, Task, TaskBoard};
use crate::siyuan_task as sy_task;

impl SiYuanStore {
    pub(super) async fn create_impl(
        &self,
        board: &TaskBoard,
        input: &CreateTask,
    ) -> Result<Task, StoreError> {
        let board = self.board(board).await?;
        let status = input.status.clone().unwrap_or_else(|| "Draft".into());
        let body_md = match &input.body_md {
            Some(md) => md.clone(),
            None => sy_task::create_body_md(&input.title, &input.task_type.0, ""),
        };
        let ty = input
            .task_type
            .is_set()
            .then_some(input.task_type.0.as_str());
        let (row_id, _doc_id) = sy_task::create_row_with_body(
            &self.sy,
            &board,
            &input.title,
            &status,
            &body_md,
            ty,
            &self.pacing,
        )
        .await
        .map_err(|e| {
            let text = format!("{e:#}");
            if text.contains("уже есть на доске") {
                dup_error(&input.title)
            } else {
                StoreError::Other(e.context(text))
            }
        })?;
        Ok(Task {
            id: row_id,
            title: input.title.clone(),
            status,
            task_type: input.task_type.clone(),
            branch: String::new(),
            pr: String::new(),
            dev_session: String::new(),
            planned: String::new(),
            planned_ms: 0,
            schedule_cell: String::new(),
        })
    }

    pub(super) async fn edit_impl(
        &self,
        board: &TaskBoard,
        task: &Task,
        patch: &EditTask,
    ) -> StoreResult<Task> {
        let board = self.board(board).await?;
        let row = self.row_of(&board, task).await?;
        sy_task::edit(
            &self.sy,
            &board,
            &row,
            patch.status.as_deref(),
            patch.branch.as_deref(),
            patch.pr.as_deref(),
            patch.task_type.as_ref().map(|t| t.0.as_str()),
        )
        .await?;
        if let Some(title) = &patch.title {
            sy_task::edit_title(&self.sy, &board, &row, title).await?;
        }
        if let Some((planned, _ms)) = &patch.planned {
            sy_task::set_planned(&self.sy, &board, &row, planned).await?;
        }
        let fresh = self.row_of(&board, task).await?;
        Ok(Self::convert(&fresh))
    }

    pub(super) async fn comment_impl(
        &self,
        board: &TaskBoard,
        task: &Task,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<CommentInsert, StoreError> {
        let board = self.board(board).await?;
        let row = self.row_of(&board, task).await?;
        sy_task::comment(&self.sy, &board, &row, &self.author.clone(), text, reply_to)
            .await
            .map_err(|e| not_found_or_other(e, &task.id))?;
        Ok(CommentInsert {
            id: format!("{}:{}", row.item_id, chrono::Utc::now().timestamp_millis()),
        })
    }

    pub(super) async fn claim_impl(
        &self,
        board: &TaskBoard,
        task: &Task,
        session: &str,
    ) -> StoreResult<Task> {
        let board = self.board(board).await?;
        let row = self.row_of(&board, task).await?;
        sy_task::claim(&self.sy, &board, &row, session).await?;
        let fresh = self.row_of(&board, task).await?;
        Ok(Self::convert(&fresh))
    }

    pub(super) async fn close_impl(
        &self,
        board: &TaskBoard,
        task: &Task,
        note: Option<&str>,
    ) -> StoreResult<Task> {
        let board = self.board(board).await?;
        let row = self.row_of(&board, task).await?;
        sy_task::close(&self.sy, &board, &row, note).await?;
        let fresh = self.row_of(&board, task).await?;
        Ok(Self::convert(&fresh))
    }

    pub(super) async fn set_body_attr_impl(
        &self,
        board: &TaskBoard,
        task: &Task,
        key: &str,
        value: &str,
    ) -> StoreResult<()> {
        let board = self.board(board).await?;
        let row = self.row_of(&board, task).await?;
        if key == "custom-schedule" {
            sy_task::sync_schedule_cell(&self.sy, &board, &row.item_id, value).await?;
        }
        let doc_id = self.body_doc_of(&board, task).await?;
        self.sy.set_block_attr(&doc_id, key, value).await
    }
}
