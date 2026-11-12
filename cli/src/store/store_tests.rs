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

/// Frontmatter GitHubStore: parse из тела issue, мульти-правка без потери
/// rest-md и порядка полей (контракт миграции: Branch/PR/Сессия dev —
/// frontmatter, тело не трогается).
mod frontmatter_tests {
    use crate::store::github_task::Frontmatter;

    #[test]
    fn parse_without_frontmatter_rests_whole_body() {
        let f = Frontmatter::parse("тело без блока");
        assert!(f.get("branch").is_none());
        assert_eq!(f.rest_md, "тело без блока");
    }

    #[test]
    fn parse_extracts_fields_and_rest() {
        let f = Frontmatter::parse("---\nbranch: work/x\nstatus: Todo\n---\n\n# тело\n");
        assert_eq!(f.get("branch").as_deref(), Some("work/x"));
        assert_eq!(f.get("status").as_deref(), Some("Todo"));
        assert!(f.rest_md.contains("# тело"));
    }

    #[test]
    fn with_fields_replaces_keeps_order_and_rest() {
        let f = Frontmatter::parse("---\nbranch: old\npr: https://x/1\n---\nтекст");
        let out = f.with_fields(&[("branch", "new"), ("dev-session", "ses_1")]);
        assert!(out.contains("branch: new"));
        assert!(out.contains("pr: https://x/1"));
        assert!(out.contains("dev-session: ses_1"));
        assert!(out.starts_with("---\n"));
        assert!(out.ends_with("текст"));
        assert!(!out.contains("branch: old"));
    }

    #[test]
    fn with_field_creates_block_on_plain_body() {
        let f = Frontmatter::parse("просто тело");
        let out = f.with_field("schedule", "6h");
        assert!(out.starts_with("---\nschedule: 6h\n---\n"));
        assert!(out.ends_with("просто тело"));
    }
}
