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

/// Дефолты priority и autodone при create (задачи 1052 и 1050):
/// fm-конструирование новой задачи — чистая функция write-path.
mod create_frontmatter_tests {
    use crate::store::github_task_write::create_frontmatter;

    #[test]
    fn create_without_priority_flag_defaults_to_medium() {
        let out = create_frontmatter("# заголовок\n\nтело", "Todo", None, None);
        assert!(
            out.starts_with("---\nstatus: Todo\npriority: medium\n"),
            "стабильный порядок + дефолт medium, got: {out}"
        );
        assert!(out.ends_with("# заголовок\n\nтело"));
    }

    #[test]
    fn explicit_priority_flag_wins_over_default() {
        let out = create_frontmatter("тело", "Draft", Some("high"), None);
        assert!(
            out.starts_with("---\nstatus: Draft\npriority: high\n"),
            "got: {out}"
        );
        assert!(!out.contains("medium"));
    }

    #[test]
    fn authored_priority_in_body_frontmatter_survives() {
        let body = "---\npriority: low\n---\nтело с авторским fm";
        let out = create_frontmatter(body, "Todo", None, None);
        assert!(out.contains("priority: low"), "got: {out}");
        assert!(
            !out.contains("medium"),
            "дефолт не подставляется поверх авторского"
        );
        assert!(out.contains("status: Todo"));
    }

    #[test]
    fn explicit_flag_overrides_authored_frontmatter_field() {
        let body = "---\npriority: low\n---\nrest";
        let out = create_frontmatter(body, "Todo", Some("high"), None);
        assert!(out.contains("priority: high"), "got: {out}");
        assert!(!out.contains("priority: low"));
    }

    #[test]
    fn autodone_cell_keeps_bool_parity_and_order() {
        let out = create_frontmatter("тело", "Draft", Some("low"), Some(true));
        assert!(
            out.starts_with("---\nstatus: Draft\npriority: low\nautodone: true\n"),
            "got: {out}"
        );
        let off = create_frontmatter("тело", "Draft", None, Some(false));
        assert!(
            off.starts_with("---\nstatus: Draft\npriority: medium\nautodone: false\n"),
            "got: {off}"
        );
    }

    #[test]
    fn create_without_autodone_flag_defaults_to_true() {
        let out = create_frontmatter("# заголовок\n\nтело", "Todo", None, None);
        assert!(
            out.starts_with("---\nstatus: Todo\npriority: medium\nautodone: true\n"),
            "стабильный порядок + дефолт true (решение владельца 14.11), got: {out}"
        );
        assert!(out.ends_with("# заголовок\n\nтело"));
    }

    #[test]
    fn authored_autodone_in_body_frontmatter_survives() {
        let body = "---\nautodone: false\n---\nтело с авторским fm";
        let out = create_frontmatter(body, "Todo", None, None);
        assert!(out.contains("autodone: false"), "got: {out}");
        assert!(
            !out.contains("autodone: true"),
            "дефолт не подставляется поверх авторского"
        );
        assert!(out.contains("status: Todo"));
    }

    #[test]
    fn explicit_autodone_off_overrides_authored_frontmatter() {
        let body = "---\nautodone: true\n---\nrest";
        let out = create_frontmatter(body, "Todo", None, Some(false));
        assert!(out.contains("autodone: false"), "got: {out}");
        assert!(
            !out.contains("autodone: true"),
            "явный off перекрывает авторский fm"
        );
    }
}

/// Reply-маркер (задача 61): comment_impl пишет «> ответ на <значение>»,
/// thread при чтении распознаёт его первой строкой; чужая цитата — не
/// маркер. Формат един: write↔read через REPLY_MARKER_PREFIX.
mod reply_marker_tests {
    use crate::store::task_store::{parse_reply_marker, REPLY_MARKER_PREFIX};

    #[test]
    fn marker_first_line_parses_anchor() {
        let body = format!("{REPLY_MARKER_PREFIX}6855341686\n\nтекст ответа");
        assert_eq!(parse_reply_marker(&body).as_deref(), Some("6855341686"));
    }

    #[test]
    fn plain_comment_has_no_anchor() {
        assert_eq!(parse_reply_marker("обычный коммент"), None);
        assert_eq!(parse_reply_marker(""), None);
    }

    #[test]
    fn foreign_quote_is_not_marker() {
        // юзер-цитата в UI ("> @author …") и цитаты в середине текста —
        // не reply-маркер: парсим только точный префикс первой строки
        assert_eq!(parse_reply_marker("> @author цитата чужого коммента"), None);
        assert_eq!(
            parse_reply_marker("текст\n> ответ на в-середине-не-якорь"),
            None
        );
    }

    #[test]
    fn only_first_line_is_anchor_value() {
        let body = format!("{REPLY_MARKER_PREFIX}якорь\nещё строка");
        assert_eq!(parse_reply_marker(&body).as_deref(), Some("якорь"));
    }

    #[test]
    fn empty_anchor_is_none() {
        assert_eq!(parse_reply_marker("> ответ на \n\nтекст"), None);
    }

    /// Write-read круг: comment_impl-формат читается парсером тем же
    /// значением (контракт единого формата).
    #[test]
    fn write_read_roundtrip() {
        let anchor = "6869829500";
        let text = "ответ тестировщику";
        let body = format!("{REPLY_MARKER_PREFIX}{anchor}\n\n{text}");
        assert_eq!(parse_reply_marker(&body).as_deref(), Some(anchor));
        assert!(body.contains(text));
    }
}
