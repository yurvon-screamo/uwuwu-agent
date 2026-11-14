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
    async fn body_replace(
        &self,
        _board: &TaskBoard,
        _task: &Task,
        _new_md: &str,
        _base: &str,
    ) -> Result<Task, anyhow::Error> {
        unreachable!("тест касается только default-методов")
    }
    async fn thread(
        &self,
        _board: &TaskBoard,
        _task: &Task,
    ) -> Result<Vec<crate::store::ThreadEntry>, anyhow::Error> {
        unreachable!("тест касается только default-методов")
    }
}

#[tokio::test]
async fn default_create_cron_run_is_loud() {
    let err = BareStore
        .create_cron_run(&board(), &task(), 0)
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("create_cron_run"));
}

/// Полнотекст-парсер GitHub DocStore: строки «path:line:text» → хиты.
#[test]
fn fulltext_parser_extracts_slug_and_snippet() {
    let stdout = "docs/uwuwu/README.md:12:легаси-бекенд нестабилен\ndocs/x.md:3:ещё\n";
    let hits = crate::store::github_doc_search::parse_line_pairs(stdout);
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].title, "README");
    assert!(hits[0].snippet.contains("нестабилен"));
}

/// Заголовочный поиск: AND-токены по путям, case-insensitive.
#[test]
fn by_title_matches_and_tokens_case_insensitive() {
    let tmp = tempfile::tempdir().unwrap();
    let docs = tmp.path().join("docs/Задачи uwuwu");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("Замена бекенда.md"), "x").unwrap();
    std::fs::write(docs.join("Другое.md"), "x").unwrap();
    let hits = crate::store::github_doc_search::by_title_sync(
        tmp.path().to_str().unwrap(),
        "замена бекенда",
        10,
    );
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].slug, "Задачи uwuwu/Замена бекенда");
}

/// Носитель клеток (ADR-098): convert читает клетки из payload
/// `issue_field_values` (норм-форма), Status — Pv2-джойн (параметр) с
/// проекцией closed→Done; fm-шапка тела мертва.
mod issue_fields_tests {
    use crate::store::github_task::GitHubStore;

    fn issue_with_fields() -> serde_json::Value {
        serde_json::json!({
            "number": 42,
            "title": "пробная задача",
            "state": "open",
            "type": { "name": "Bug" },
            "body": "чистый контент без шапки\n",
            "issue_field_values": [
                { "issue_field_name": "Приоритет", "value": 87264165,
                  "single_select_option": { "id": 87264165, "name": "high" } },
                { "issue_field_name": "Autodone", "value": 87264196,
                  "single_select_option": { "id": 87264196, "name": "on" } },
                { "issue_field_name": "Branch", "value": "work/task-42" },
                { "issue_field_name": "PR", "value": "https://github.com/x/y/pull/1" },
                { "issue_field_name": "Сессия dev", "value": "ses_1" },
                { "issue_field_name": "Расписание", "value": "30 5 * * *" },
                { "issue_field_name": "Запланировано", "value": "2026-11-20" }
            ]
        })
    }

    #[test]
    fn convert_читает_клетки_из_issue_field_values() {
        let store = GitHubStore::new("o/r".into());
        let t = store.convert(&issue_with_fields(), Some("In Progress"));
        assert_eq!(t.id, "42");
        assert_eq!(t.status, "In Progress");
        assert_eq!(t.priority, "high");
        assert!(t.autodone);
        assert_eq!(t.branch, "work/task-42");
        assert_eq!(t.pr, "https://github.com/x/y/pull/1");
        assert_eq!(t.dev_session, "ses_1");
        assert_eq!(t.schedule_cell, "30 5 * * *");
        assert_eq!(t.planned, "2026-11-20");
        assert!(t.planned_ms > 0);
    }

    #[test]
    fn convert_пустые_клетки_дефолты() {
        let store = GitHubStore::new("o/r".into());
        let issue = serde_json::json!({
            "number": 7, "title": "x", "state": "open",
            "body": "тело", "issue_field_values": []
        });
        let t = store.convert(&issue, Some(""));
        assert_eq!(t.status, "Todo", "нет Pv2-статуса → Todo-дефолт");
        assert_eq!(t.priority, "");
        assert!(!t.autodone);
        assert_eq!(t.branch, "");
        assert_eq!(t.planned, "");
        assert_eq!(t.planned_ms, 0);
    }

    #[test]
    fn convert_closed_проекция_done_поверх_всего() {
        let store = GitHubStore::new("o/r".into());
        let mut issue = issue_with_fields();
        issue["state"] = serde_json::json!("closed");
        let t = store.convert(&issue, Some("Ревью результата"));
        assert_eq!(t.status, "Done", "closed issue → Done независимо от Pv2");
    }
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
        priority: String::new(),
        autodone: false,
    }
}
