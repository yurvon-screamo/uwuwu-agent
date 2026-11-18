//! Интеграционные пробы секционного write-path (#1152) на MemoryHub:
//! поверхность MCP (дескриптор + диспетчер) против живого юзкейса —
//! happy-path, freshness-отказ, append, твин-гард, пин-h3 (кейс #1046),
//! квитанция полного task_body. Мок-hub — в памяти, без GitHub.

use super::test_support::{StubSessions, StubState, StubWiki};
use super::tools::{tool_descriptors, ToolHub};
use crate::clock::SystemClock;
use crate::mockhub::MemoryHub;
use serde_json::{json, Value};
use std::sync::Arc;
use uwuwu_usecase::{
    DocsView, IssueFieldStore, PersonView, SearchUseCase, TaskBoardRepo, TaskView,
    TaskWriteUseCase, WikiRepo,
};

/// Фикстура поверхности: ToolHub целиком на MemoryHub + стабах вики/сессий.
/// Возвращает тот же hub, что сидит в писателе — тело читается напрямую.
fn hub() -> (Arc<ToolHub>, Arc<MemoryHub>) {
    let memory = Arc::new(MemoryHub::from_env());
    let board: Arc<dyn TaskBoardRepo> = memory.clone();
    let fields: Arc<dyn IssueFieldStore> = memory.clone();
    let wiki: Arc<dyn WikiRepo> = Arc::new(StubWiki);
    let sessions = Arc::new(uwuwu_usecase::SessionRouter::uniform(Arc::new(
        StubSessions,
    )));
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
            sessions: Arc::new(uwuwu_usecase::SessionRouter::uniform(Arc::new(
                StubSessions,
            ))),
            clock: Arc::new(SystemClock),
            models: Arc::new(uwuwu_usecase::SpawnModels::canonical()),
            checkout: String::new(),
            cup: 7,
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

/// Проба #397 (16.11) дословно: полный task_body съедал титульную H1-секцию
/// с контентом до следующего H1 (mangeling-trim диспетчера). Пин сценария:
/// H1-старт — raw-тело на носителе ≡ отправленному (все секции и хвост).
#[tokio::test]
async fn full_body_replace_keeps_h1_title_section_verbatim() {
    let (tools, memory) = hub();
    let (number, _) = create_task(&tools, "t: h1", "## Постановка\n\nЧерновик.\n").await;
    let sent = "# Заголовок задачи\n\nТитульная секция с контентом до следующего H1.\n\n## Постановка\n\nТело после H1-тайтла.\n\n## Хвост\n\nХвостовая секция.\n";
    call(
        &tools,
        "task_body",
        json!({ "number": number, "body": sent }),
    )
    .await
    .expect("full body");
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert_eq!(
        stored, sent,
        "raw-тело ≡ отправленному — H1-тайтл, секции и хвост на месте"
    );
}

/// Проба #397 (16.11) дословно: секционная замена дублировала хвост тела
/// после правимой секции (байт-повтор секций, две «Комменты» по несколько КБ).
/// Пин BDD-сценария: замена на месте, каждая секция хвоста — ровно один раз.
#[tokio::test]
async fn section_edit_replaces_in_place_without_tail_duplication() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: tail", &big_body()).await;
    call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "План",
            "content": "Замена секции на месте; хвост не дублируется.\n",
            "base_hash": hash,
        }),
    )
    .await
    .expect("section edit");
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(
        stored.contains("Замена секции на месте"),
        "новый контент секции на месте: {stored}"
    );
    assert!(
        !stored.contains("Контракт секционного пути"),
        "старый контент секции «План» удалён, не дописан: {stored}"
    );
    for marker in [
        "## План",
        "## Постановка",
        "## Отчёт инженера",
        "## Смежное",
    ] {
        assert_eq!(
            stored.matches(marker).count(),
            1,
            "«{marker}» встречается ровно один раз — заголовки и хвост не задублированы"
        );
    }
}

/// Профилактика #1256 (класс «чтение врёт после записи», ADR-093): запись
/// тела → немедленное чтение того же номера возвращает записанное целиком,
/// fm body_hash сходится с квитанцией. Граница покрытия: пин — серверная
/// сторона (MemoryHub-носитель); GitHub-консистентность REST-чтений
/// закрывает живая проба против прода.
#[tokio::test]
async fn immediate_read_after_body_write_returns_written_body() {
    let (tools, _) = hub();
    let (number, _) = create_task(&tools, "t: rar", "## Постановка\n\nЧерновик.\n").await;
    let sent =
        "# Тайтл записи\n\nКонтент записи: immediate read-after-write.\n\n## Хвост\n\nодин раз\n";
    let receipt = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": sent }),
    )
    .await
    .expect("write");
    let get = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("read");
    assert!(
        get.contains(sent),
        "немедленное чтение возвращает записанное целиком: {get}"
    );
    let receipt_hash = receipt
        .split("body_hash: ")
        .nth(1)
        .unwrap_or_default()
        .trim();
    assert!(
        !receipt_hash.is_empty(),
        "квитанция несёт body_hash: {receipt}"
    );
    assert_eq!(
        fm_hash(&get),
        receipt_hash,
        "fm body_hash немедленного чтения ≡ квитанции записи"
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
    // гейт полноты #1280: разбор заполняет поля явно до выхода из Analys
    // (Automerge create пишет сам, Приоритет — решение аналитика)
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": number, "priority": "medium" }),
    )
    .await
    .expect("заполнение Приоритета решением разбора");
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

/// Гейт полноты полей на MCP-поверхности (#1280): выход из Analys с пустым
/// Приоритетом — отказ с перечнем; дозаполнение → переход проходит.
#[tokio::test]
async fn analys_exit_gate_on_mcp_surface() {
    let (tools, _memory) = hub();
    let n = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "гейт полноты" }),
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
    // create пишет Automerge сам — пуст только Приоритет: перечень
    // называет его и не зовёт заполненное
    let err = call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": number, "status": "Review Analys" }),
    )
    .await
    .expect_err("пустой Приоритет не выпускает строку из Analys");
    assert!(err.contains("Приоритет"), "перечень без Приоритета: {err}");
    assert!(
        !err.contains("Automerge"),
        "заполненное поле в перечне: {err}"
    );
    // дозаполнение одним вызовом с переходом тоже легально —
    // здесь раздельно: сначала решение, потом выход
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": number, "priority": "high" }),
    )
    .await
    .expect("дозаполнение Приоритета");
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": number, "status": "Review Analys" }),
    )
    .await
    .expect("после дозаполнения переход проходит");
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
        sessions: Arc::new(uwuwu_usecase::SessionRouter::uniform(sessions)),
        search: Arc::new(SearchUseCase { semantic: None }),
        crons: Arc::new(uwuwu_usecase::CronUseCase {
            wiki: Arc::new(StubWiki),
            sessions: Arc::new(uwuwu_usecase::SessionRouter::uniform(Arc::new(
                StubSessions,
            ))),
            clock: Arc::new(SystemClock),
            models: Arc::new(uwuwu_usecase::SpawnModels::canonical()),
            checkout: String::new(),
            cup: 7,
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

// ===== Автомерж #1232: MCP-поверхность (schema, legacy-ключ, edit-патч) =====

#[test]
fn descriptors_expose_automerge() {
    let list = tool_descriptors();
    let find = |name: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("{name} на поверхности"))["inputSchema"]["properties"]
            .clone()
    };
    let create = find("task_create");
    assert!(
        create.get("automerge").is_some(),
        "create рекламит automerge: {create:?}"
    );
    assert!(
        create.get("autodone").is_none(),
        "legacy-ключ не рекламится (но читается)"
    );
    let edit = find("task_edit");
    assert!(
        edit.get("automerge").is_some(),
        "edit рекламит automerge (до-пометка старых строк): {edit:?}"
    );
}

/// Legacy-ключ `autodone` читается как automerge (back-compat MCP-клиентов
/// 14.11); выключенный automerge создаёт строку без ошибок.
#[tokio::test]
async fn task_create_accepts_automerge_and_legacy_key() {
    let (tools, _memory) = hub();
    call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "auto on (новый ключ)", "body": "б", "automerge": true }),
    )
    .await
    .expect("новый ключ accepted");
    call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "auto on (legacy ключ)", "body": "б", "autodone": true }),
    )
    .await
    .expect("legacy autodone читается как automerge");
    call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "auto off", "body": "б", "automerge": false }),
    )
    .await
    .expect("выключенный automerge accepted");
}

/// task_edit с одним automerge — непустой патч: до фикса edit не нёс
/// automerge и патч падал «пустой патч» (инструмент до-пометки старых строк).
#[tokio::test]
async fn task_edit_automerge_patch_is_not_empty_at_surface() {
    let (tools, _memory) = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "под automerge-патч", "body": "б" }),
    )
    .await
    .expect("create ок");
    let n: u64 = created
        .trim()
        .split('#')
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .expect("номер в ответе");
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": n, "automerge": true }),
    )
    .await
    .expect("automerge-only патч не «пустой»");
}

/// #1166: фабрика записала Branch при спавне — `task edit --pr` без
/// сопутствующего `--branch` остаётся видимым на карточке (рид гейтится
/// веткой; инцидент #1152 — «pr: —» при живом URL).
#[tokio::test]
async fn task_get_renders_pr_after_factory_branch_without_manual_branch_edit() {
    let (tools, memory) = hub();
    let (number, _) = create_task(&tools, "t: branch at spawn", "## Постановка\n\nx.\n").await;

    // Фабрика: task-env создал env → Branch-ячейка от имени ветки ворктри.
    let branch = uwuwu_domain::BranchName::parse("work/task-8").unwrap();
    memory
        .set_branch("Задачи uwuwu", number, &branch)
        .await
        .expect("set_branch");

    // Агент финализирует PR одной командой без --branch.
    call(
        &tools,
        "task_edit",
        json!({ "number": number, "pr": "https://github.com/o/r/pull/9" }),
    )
    .await
    .expect("pr edit");

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    assert!(card.contains("branch: work/task-8"), "{card}");
    assert!(card.contains("pr: https://github.com/o/r/pull/9"), "{card}");
}

/// #1155: docs-only ячейка (вики-артефакт без ветки кода) рендерится в fm —
/// `pr: <URL>`, `branch: —` (инцидент #1153: два раунда ревью на «pr: —»
/// при живом в носителе значении).
#[tokio::test]
async fn task_get_renders_branchless_pr_cell() {
    let (tools, _memory) = hub();
    let (number, _) = create_task(&tools, "t: docs-only artifact", "## Постановка\n\nx.\n").await;

    // Агент финализирует docs-only задачу: PR-ячейка = URL вики-артефакта,
    // ветки кода нет и не будет (канон AGENTS.md).
    call(
        &tools,
        "task_edit",
        json!({
            "number": number,
            "pr": "https://github.com/yurvon-screamo/uwuwu-hub/blob/main/docs/Projects/uwuwu/Design%20Spec.md"
        }),
    )
    .await
    .expect("pr edit");

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    assert!(
        card.contains(
            "pr: https://github.com/yurvon-screamo/uwuwu-hub/blob/main/docs/Projects/uwuwu/Design%20Spec.md"
        ),
        "{card}"
    );
    assert!(card.contains("branch: —"), "{card}");
}

/// #1155/true-washing#379: чужовендорный pull-URL на безветочной строке
/// (env:false-проекты) — тот же рендер без вендор-гейта.
#[tokio::test]
async fn task_get_renders_foreign_vendor_pull_url() {
    let (tools, _memory) = hub();
    let (number, _) = create_task(&tools, "t: forgejo pull", "## Постановка\n\nx.\n").await;

    call(
        &tools,
        "task_edit",
        json!({
            "number": number,
            "pr": "https://git.true-wash.com/true-wash/deploy/pulls/9"
        }),
    )
    .await
    .expect("pr edit");

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    assert!(
        card.contains("pr: https://git.true-wash.com/true-wash/deploy/pulls/9"),
        "{card}"
    );
    assert!(card.contains("branch: —"), "{card}");
}

/// #1155 (поручение владельца): автомерж-ячейка живёт на mock-носителе —
/// create пишет дефолт on, edit переключает, fm рендерит фактическое
/// состояние (без хранения на носителе fm и тик стенда врали «false»).
#[tokio::test]
async fn automerge_cell_roundtrips_on_mock_carrier() {
    let (tools, _memory) = hub();
    let (number, _) = create_task(&tools, "t: automerge roundtrip", "## Постановка\n\nx.\n").await;

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    assert!(
        card.contains("automerge: true"),
        "create-дефолт on рендерится: {card}"
    );

    call(
        &tools,
        "task_edit",
        json!({ "number": number, "automerge": false }),
    )
    .await
    .expect("automerge edit");

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    assert!(
        card.contains("automerge: false"),
        "edit off рендерится: {card}"
    );
}
