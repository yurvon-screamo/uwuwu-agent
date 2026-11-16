//! Интеграционные пробы секционного write-path (#1152) на MemoryHub:
//! поверхность MCP (дескриптор + диспетчер) против живого юзкейса —
//! happy-path, freshness-отказ, append, твин-гард, пин-h3 (кейс #1046),
//! квитанция полного task_body. Мок-hub — в памяти, без GitHub.

use super::tools::{tool_descriptors, ToolHub};
use crate::clock::SystemClock;
use crate::mockhub::MemoryHub;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use uwuwu_domain::{Slot, Status};
use uwuwu_usecase::{
    AgentSessionGateway, DocsView, IssueFieldStore, Liveness, PersonView, SearchUseCase,
    SessionInfo, SessionSpec, TaskBoardRepo, TaskView, TaskWriteUseCase, TickStateStore,
    UsecaseResult, WikiRepo,
};

struct StubWiki;

#[async_trait]
impl WikiRepo for StubWiki {
    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(vec![])
    }
    async fn read_doc(&self, _slug: &str) -> UsecaseResult<Option<String>> {
        Ok(None)
    }
    async fn list_docs(&self, _prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(vec![])
    }
    async fn write_doc(&self, _: &str, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn append_doc(&self, _: &str, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
}

struct StubSessions;

#[async_trait]
impl AgentSessionGateway for StubSessions {
    async fn create_session(&self, _: &SessionSpec) -> UsecaseResult<String> {
        Ok("ses_stub".into())
    }
    async fn prompt(&self, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn run_command(&self, _: &str, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, _: &str) -> Liveness {
        Liveness::Unknown
    }
    async fn delete_session(&self, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn last_activity_ms(&self, _: &str) -> UsecaseResult<Option<u64>> {
        Ok(None)
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        Ok(vec![])
    }
    async fn export_session(&self, _: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
    async fn last_assistant_reply(
        &self,
        _s: &str,
    ) -> UsecaseResult<Option<uwuwu_usecase::AssistantReply>> {
        Ok(None)
    }
}

/// No-op TickStateStore: писателю нужен факт вызова mark_own_comment,
/// состояние в пробе диспетчера не проверяется.
struct StubState;

impl TickStateStore for StubState {
    fn last_comment_id(&self, _: &str, _: u64) -> Option<String> {
        None
    }
    fn last_seen_status(&self, _: &str, _: u64) -> Option<Status> {
        None
    }
    fn set_last_seen_status(&self, _: &str, _: u64, _: Status) {}
    fn set_last_comment_id(&self, _: &str, _: u64, _: &str) {}
    fn dreamer_done(&self, _: &str) -> bool {
        false
    }
    fn mark_dreamer_done(&self, _: &str) {}
    fn dreamer_day_spawns(&self, _: &str) -> u32 {
        0
    }
    fn mark_dreamer_spawn(&self, _: &str) {}
    fn last_spawn_ms(&self, _: &str, _: u64, _: Slot) -> Option<u64> {
        None
    }
    fn mark_spawn(&self, _: &str, _: u64, _: Slot, _: u64) {}
    fn clear_spawn_ms(&self, _: &str, _: u64, _: Slot) {}
    fn last_analys_ms(&self, _: &str, _: u64) -> Option<u64> {
        None
    }
    fn mark_analys_spawn(&self, _: &str, _: u64, _: u64) {}
    fn last_analys_review_ms(&self, _: &str, _: u64) -> Option<u64> {
        None
    }
    fn mark_analys_review_spawn(&self, _: &str, _: u64, _: u64) {}
    fn analys_loop(&self, _: &str, _: u64) -> Option<(Status, u32)> {
        None
    }
    fn set_analys_loop(&self, _: &str, _: u64, _: Status, _: u32) {}
    fn clear_analys_loop(&self, _: &str, _: u64) {}
    fn analys_review_loop(&self, _: &str, _: u64) -> Option<(Status, u32)> {
        None
    }
    fn set_analys_review_loop(&self, _: &str, _: u64, _: Status, _: u32) {}
    fn clear_analys_review_loop(&self, _: &str, _: u64) {}
    fn prune_analys_guards(&self, _: &std::collections::HashSet<String>) {}
    fn stall_ping(&self, _: &str, _: u64) -> Option<(Status, u64, u32)> {
        None
    }
    fn mark_stall_ping(&self, _: &str, _: u64, _: Status, _: u64) {}
    fn spawn_loop(&self, _: &str, _: u64, _: Slot) -> Option<(Status, u32)> {
        None
    }
    fn set_spawn_loop(&self, _: &str, _: u64, _: Slot, _: Status, _: u32) {}
    fn reset_stale_spawn_loops(&self, _: &str, _: u64, _: Status) {}
    fn own_comment(&self, _: &str) -> bool {
        false
    }
    fn mark_own_comment(&self, _: &str, _: u64) {}
    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}

/// Фикстура поверхности: ToolHub целиком на MemoryHub + стабах вики/сессий.
/// Возвращает тот же hub, что сидит в писателе — тело читается напрямую.
fn hub() -> (Arc<ToolHub>, Arc<MemoryHub>) {
    let memory = Arc::new(MemoryHub::from_env());
    let board: Arc<dyn TaskBoardRepo> = memory.clone();
    let fields: Arc<dyn IssueFieldStore> = memory.clone();
    let wiki: Arc<dyn WikiRepo> = Arc::new(StubWiki);
    let sessions: Arc<dyn AgentSessionGateway> = Arc::new(StubSessions);
    let boards = vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())];
    let tools = Arc::new(ToolHub {
        tasks: Arc::new(TaskView::new(board.clone(), boards.clone())),
        docs: Arc::new(DocsView::new(wiki.clone())),
        person: Arc::new(PersonView::new(wiki.clone())),
        writer: Arc::new(TaskWriteUseCase {
            board: board.clone(),
            fields,
            boards,
            clock: Arc::new(SystemClock),
            state: Arc::new(StubState),
        }),
        wiki,
        sessions,
        search: Arc::new(SearchUseCase { semantic: None }),
        crons: Arc::new(uwuwu_usecase::CronUseCase {
            wiki: Arc::new(StubWiki),
            sessions: Arc::new(StubSessions),
            clock: Arc::new(SystemClock),
            spawn_model: None,
            checkout: String::new(),
        }),
    });
    (tools, memory)
}

async fn call(tools: &ToolHub, name: &str, args: Value) -> Result<String, String> {
    let msg = json!({ "params": { "name": name, "arguments": args } });
    match tools.call(&msg).await {
        Ok(v) => Ok(v[0]["text"].as_str().unwrap_or_default().to_string()),
        Err(e) => Err(e),
    }
}

/// Большое тело кейса #1046: канон-структура с H3-нутром отчёта и
/// наполнителем для массы (секционный путь бессилен только на пустых).
fn big_body() -> String {
    let mut filler = String::new();
    for i in 0..120 {
        filler.push_str(&format!(
            "строка наполнителя {i}: дельта секции вместо полного тела\n"
        ));
    }
    format!(
        "## Постановка\n\nНаблюдение: тело задачи 38КБ, каждая актуализация гонит полный body \
         через контекст модели.\n\n## План\n\nКонтракт секционного пути: ATX-таблица, гард \
         тёзки, freshness по body_hash.\n\n## Отчёт инженера\n\n### Суть изменений\n\nСерверный \
         write-path: агент передаёт дельту, сервер собирает тело сам.\n\n### Проверки\n\n| \
         Проверка | Статус |\n| --- | --- |\n| Tests | ✅ |\n\n{filler}\n## Смежное\n\nADR-048, \
         ADR-053.\n"
    )
}

/// body_hash из fm-блока вывода task_get.
fn fm_hash(task_get: &str) -> String {
    task_get
        .lines()
        .find_map(|l| l.strip_prefix("body_hash: "))
        .unwrap_or_default()
        .trim()
        .to_string()
}

async fn create_task(tools: &ToolHub, title: &str, body: &str) -> (u64, String) {
    let text = call(
        tools,
        "task_create",
        json!({ "title": title, "body": body }),
    )
    .await
    .expect("create");
    let number: u64 = text
        .trim_start_matches("создана задача #")
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .expect("number");
    let get = call(tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    (number, fm_hash(&get))
}

#[tokio::test]
async fn section_edit_replaces_h2_and_receipt_carries_fresh_hash() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: happy", &big_body()).await;

    let text = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "Постановка",
            "content": "Новое наблюдение: дельта секции вместо полного тела.\n",
            "base_hash": hash,
        }),
    )
    .await
    .expect("section edit");
    assert!(text.contains("заменена"), "квитанция: {text}");
    assert!(text.contains("body_hash: "), "хэш в квитанции: {text}");

    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(
        stored.contains("Новое наблюдение: дельта секции"),
        "новый контент в теле"
    );
    assert!(
        !stored.contains("гонит полный body"),
        "старый контент заменён"
    );
    assert!(stored.contains("## Отчёт инженера"), "соседние секции живы");
    // квитанция честна: хэш и размер — ровно по телу как записано
    assert!(
        text.contains(&uwuwu_usecase::body_hash(&stored)),
        "хэш квитанции = хэш записанного тела: {text}"
    );
    assert!(
        text.contains(&format!("{} байт", stored.len())),
        "размер: {text}"
    );
}

#[tokio::test]
async fn section_edit_rejects_stale_base_hash_without_write() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: stale", &big_body()).await;
    // чужая правка между чтением и секционной записью
    call(
        &tools,
        "task_body",
        json!({ "number": number, "body": "## Постановка\n\nТело подменили.\n" }),
    )
    .await
    .expect("full replace");

    let err = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "Постановка",
            "content": "поздняя дельта\n",
            "base_hash": hash,
        }),
    )
    .await
    .expect_err("freshness-отказ");
    assert!(err.contains("base_hash мимо"), "отказ по свежести: {err}");
    assert!(
        err.contains("актуальный body_hash: "),
        "актуальный хэш в отказе: {err}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .unwrap()
        .body;
    assert!(
        stored.contains("Тело подменили"),
        "записи секционной правки нет"
    );
    assert!(!stored.contains("поздняя дельта"), "тело не тронуто");
}

#[tokio::test]
async fn section_edit_appends_missing_h2() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: append", "## А\nтело\n").await;
    let text = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "Новая секция",
            "content": "контент новой\n",
            "base_hash": hash,
        }),
    )
    .await
    .expect("append");
    assert!(
        text.contains("добавлена в конец"),
        "квитанция append: {text}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .unwrap()
        .body;
    assert!(
        stored.contains("## Новая секция\nконтент новой"),
        "append в тело: {stored}"
    );
    assert!(stored.contains("## А\nтело"), "старое тело не тронуто");
}

#[tokio::test]
async fn section_edit_guards_twin_of_other_level() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: twin", &big_body()).await;
    let err = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "Суть изменений",
            "content": "не должно попасть в тело\n",
            "base_hash": hash,
        }),
    )
    .await
    .expect_err("твин-гард");
    assert!(err.contains("тёзка на уровне h3"), "гард: {err}");
    assert!(
        err.contains("пином `### Суть изменений`"),
        "подсказка пина: {err}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .unwrap()
        .body;
    assert!(!stored.contains("не должно попасть"), "записи нет");
}

#[tokio::test]
async fn pinned_h3_edits_report_inner_section_case_1046() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: 1046", &big_body()).await;
    // одна дельта H3-нутра «Отчёта инженера» за вызов — кейс #1046
    let text = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "### Суть изменений",
            "content": "Кейс #1046: актуализация большого тела одной дельтой секции.\n",
            "base_hash": hash,
        }),
    )
    .await
    .expect("pin-h3");
    assert!(text.contains("заменена"), "квитанция: {text}");
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .unwrap()
        .body;
    assert!(
        stored.contains("### Суть изменений\nКейс #1046"),
        "h3-нутро заменено: {}",
        &stored[..400.min(stored.len())]
    );
    assert!(
        !stored.contains("агент передаёт дельту, сервер собирает тело сам."),
        "старый контент h3 удалён"
    );
    assert!(stored.contains("### Проверки"), "соседняя h3 жива");
    assert!(stored.contains("## Отчёт инженера"), "родительская h2 жива");
    assert!(stored.contains("строка наполнителя 119"), "наполнитель жив");
}

#[tokio::test]
async fn clear_of_absent_section_refuses_at_surface() {
    // находка дельта-ревью #1152: очистка отсутствующей секции — громкий
    // отказ на границе тула, пустая секция не создаётся
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: clear-miss", "## А\nтело\n").await;
    let err = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "Новая",
            "content": "",
            "base_hash": hash,
        }),
    )
    .await
    .expect_err("очищать нечего");
    assert!(err.contains("очищать нечего"), "{err}");
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .unwrap()
        .body;
    assert!(
        stored.contains("## А\nтело"),
        "исходное тело живо: {stored:?}"
    );
    assert!(
        !stored.contains("Новая"),
        "пустая секция не создана: {stored:?}"
    );
}

#[tokio::test]
async fn task_body_receipt_keeps_leading_hashes() {
    let (tools, memory) = hub();
    let (number, _) = create_task(&tools, "t: full", "## Постановка\n\nЧерновик.\n").await;
    let text = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": "## Постановка\n\nПерезапись целиком.\n" }),
    )
    .await
    .expect("full body");
    assert!(text.contains("body_hash: "), "квитанция с хэшем: {text}");
    assert!(text.contains("байт"), "размер в квитанции: {text}");
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .unwrap()
        .body;
    assert!(
        stored.starts_with("## Постановка"),
        "хэши первого заголовка живы (fm-сборка с mangeling-trim снесена): {stored}"
    );
}

#[tokio::test]
async fn section_edit_requires_section_and_base_hash() {
    let (tools, _) = hub();
    let (number, _) = create_task(&tools, "t: args", "## А\nтело\n").await;
    let err = call(
        &tools,
        "task_body_section",
        json!({ "number": number, "section": "А", "content": "x", "base_hash": "" }),
    )
    .await
    .expect_err("base_hash обязателен");
    assert!(err.contains("base_hash обязателен"), "{err}");
    let err = call(
        &tools,
        "task_body_section",
        json!({ "number": number, "section": "", "content": "x", "base_hash": "ab" }),
    )
    .await
    .expect_err("section обязателен");
    assert!(err.contains("section обязателен"), "{err}");
}

#[test]
fn descriptors_expose_task_body_section() {
    let list = tool_descriptors();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"task_body_section"),
        "секционный путь на поверхности: {names:?}"
    );
}

// ===== Аналитика #1135: create-дефолт, пайп-рёбра, круги волн на mockhub =====

#[tokio::test]
async fn task_create_defaults_to_analys() {
    let (tools, memory) = hub();
    let n = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "заявка агента", "body": "## Постановка\n\nХ" }),
    )
    .await
    .expect("create ок");
    let number: u64 = n
        .trim()
        .split('#')
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .expect("номер в ответе");
    assert!(n.contains("(Analys)"), "дефолт create: {n}");
    let rows = memory.read_board("Задачи uwuwu").await.unwrap();
    let row = rows.iter().find(|t| t.number() == number).unwrap();
    assert_eq!(row.status(), uwuwu_domain::Status::Analys);
    // idea/analys — неизвестные типы: громкий отказ create
    let err = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "старый тип", "type": "idea" }),
    )
    .await
    .expect_err("idea выведен");
    assert!(err.contains("неизвестный тип"), "{err}");
}

#[tokio::test]
async fn analysis_pipeline_edges_on_mcp_surface() {
    let (tools, _memory) = hub();
    let n = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "круг волн" }),
    )
    .await
    .unwrap();
    let number: u64 = n
        .trim()
        .split('#')
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .unwrap();
    // волна аналитика: Analys → Review Analys
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": number, "status": "Review Analys" }),
    )
    .await
    .expect("ребро Analys → Review Analys");
    // approve ревьювера: Review Analys → Todo
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": number, "status": "Todo" }),
    )
    .await
    .expect("ребро Review Analys → Todo");
    // владелец вернул с замечаниями: Todo → Analys
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": number, "status": "Analys" }),
    )
    .await
    .expect("ребро Todo → Analys");
    // fast-track (развилка 6): Analys → In Progress
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": number, "status": "In Progress" }),
    )
    .await
    .expect("fast-track Analys → In Progress");
    // запреты канона
    let err = call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": number, "status": "Todo" }),
    )
    .await
    .expect_err("In Progress → Todo вне канона");
    assert!(err.contains("вне канона"), "{err}");
}

#[tokio::test]
async fn fast_track_from_draft_and_todo() {
    // Draft (черновик владельца) → In Progress напрямую (создание через сид:
    // Draft — владелец в UI, MCP-ребро тоже должно работать)
    let memory = Arc::new(MemoryHub::with_seed(Some((1, uwuwu_domain::Status::Draft))));
    let board: Arc<dyn uwuwu_usecase::TaskBoardRepo> = memory.clone();
    let fields: Arc<dyn uwuwu_usecase::IssueFieldStore> = memory.clone();
    let wiki: Arc<dyn uwuwu_usecase::WikiRepo> = Arc::new(StubWiki);
    let sessions: Arc<dyn uwuwu_usecase::AgentSessionGateway> = Arc::new(StubSessions);
    let boards = vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())];
    let tools = Arc::new(ToolHub {
        tasks: Arc::new(TaskView::new(board.clone(), boards.clone())),
        docs: Arc::new(DocsView::new(wiki.clone())),
        person: Arc::new(PersonView::new(wiki.clone())),
        writer: Arc::new(TaskWriteUseCase {
            board: board.clone(),
            fields,
            boards,
            clock: Arc::new(SystemClock),
            state: Arc::new(StubState),
        }),
        wiki,
        sessions,
        search: Arc::new(SearchUseCase { semantic: None }),
        crons: Arc::new(uwuwu_usecase::CronUseCase {
            wiki: Arc::new(StubWiki),
            sessions: Arc::new(StubSessions),
            clock: Arc::new(SystemClock),
            spawn_model: None,
            checkout: String::new(),
        }),
    });
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": 1, "status": "In Progress" }),
    )
    .await
    .expect("fast-track Draft → In Progress");
    // Todo → In Progress (ребро существовало и до реформы)
    memory
        .set_status("Задачи uwuwu", 1, uwuwu_domain::Status::Todo)
        .await
        .unwrap();
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": 1, "status": "In Progress" }),
    )
    .await
    .expect("fast-track Todo → In Progress");
}
