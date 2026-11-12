//! Контрактные тесты store-слоя: default'ы трейта (операции этапа
//! коммутации) отказывают громко — тихих заглушек не существует.

use crate::store::task_store::{CreateTask, EditTask, TaskStore};
use crate::store::{StoreError, Task, TaskBoard};

/// Минимальный носитель default'ов: required-методы недостижимы — тест
/// касается только контрактных default'ов трейта.
struct BareStore;

#[async_trait::async_trait]
impl TaskStore for BareStore {
    async fn list(&self, _board: &TaskBoard) -> Result<Vec<Task>, anyhow::Error> {
        unreachable!("тест касается только default-методов")
    }
    async fn resolve(&self, _board: &TaskBoard, _needle: &str) -> Result<Task, StoreError> {
        unreachable!("тест касается только default-методов")
    }
    async fn body_get(&self, _board: &TaskBoard, _task: &Task) -> Result<String, anyhow::Error> {
        unreachable!("тест касается только default-методов")
    }
    async fn body_attrs(
        &self,
        _board: &TaskBoard,
        _task: &Task,
    ) -> Result<std::collections::HashMap<String, String>, anyhow::Error> {
        unreachable!("тест касается только default-методов")
    }
    async fn discover_boards(&self) -> Result<Vec<TaskBoard>, anyhow::Error> {
        unreachable!("тест касается только default-методов")
    }
    async fn create(&self, _board: &TaskBoard, _input: &CreateTask) -> Result<Task, StoreError> {
        unreachable!("тест касается только default-методов")
    }
    async fn edit(
        &self,
        _board: &TaskBoard,
        _task: &Task,
        _patch: &EditTask,
    ) -> Result<Task, anyhow::Error> {
        unreachable!("тест касается только default-методов")
    }
    async fn comment(
        &self,
        _board: &TaskBoard,
        _task: &Task,
        _text: &str,
        _reply_to: Option<&str>,
    ) -> Result<crate::store::CommentInsert, StoreError> {
        unreachable!("тест касается только default-методов")
    }
    async fn claim(
        &self,
        _board: &TaskBoard,
        _task: &Task,
        _session: &str,
    ) -> Result<Task, anyhow::Error> {
        unreachable!("тест касается только default-методов")
    }
    async fn close(
        &self,
        _board: &TaskBoard,
        _task: &Task,
        _note: Option<&str>,
    ) -> Result<Task, anyhow::Error> {
        unreachable!("тест касается только default-методов")
    }
    async fn set_body_attr(
        &self,
        _board: &TaskBoard,
        _task: &Task,
        _key: &str,
        _value: &str,
    ) -> Result<(), anyhow::Error> {
        unreachable!("тест касается только default-методов")
    }
}

#[tokio::test]
async fn default_body_replace_is_loud() {
    let err = BareStore
        .body_replace(&board(), &task(), "новое тело", "база")
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("body_replace"));
}

#[tokio::test]
async fn default_thread_is_loud() {
    let err = BareStore.thread(&board(), &task()).await.unwrap_err();
    assert!(format!("{err}").contains("thread"));
}

#[tokio::test]
async fn default_create_cron_run_is_loud() {
    let err = BareStore
        .create_cron_run(&board(), &task(), 0)
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("create_cron_run"));
}

fn board() -> TaskBoard {
    TaskBoard {
        project: "uwuwu".into(),
    }
}

fn task() -> Task {
    Task {
        id: "row-1".into(),
        title: "пробная".into(),
        status: "Todo".into(),
        task_type: crate::store::TaskType("bug".into()),
        branch: String::new(),
        pr: String::new(),
        dev_session: String::new(),
        planned: String::new(),
        planned_ms: 0,
        schedule_cell: String::new(),
    }
}
