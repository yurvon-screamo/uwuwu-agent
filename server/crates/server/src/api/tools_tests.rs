//! Интеграционные пробы секционного write-path (#1152) на MemoryHub:
//! поверхность MCP (дескриптор + диспетчер) против живого юзкейса —
//! happy-path, freshness-отказ, append, твин-гард, пин-h3 (кейс #1046),
//! квитанция полного task_body. Мок-hub — в памяти, без GitHub.

use super::test_support::{
    blob_usecase, blob_usecase_missing, fm_hash, receipt_number, tool_hub, tool_hub_on_board,
    StubCronState, StubSessions, StubState, StubWiki,
};
use super::tools::{tool_descriptors, ToolHub};
use crate::clock::SystemClock;
use crate::mockhub::MemoryHub;
use serde_json::{json, Value};
use std::sync::Arc;
use uwuwu_usecase::WikiRepo;
use uwuwu_usecase::{
    DocsView, PersonView, SearchUseCase, TaskBoardRepo, TaskView, TaskWriteUseCase,
};

/// Фикстура поверхности: ToolHub фабрики test_support (StubWiki-вики,
/// memory-blob) — тело читается напрямую через вернувшийся MemoryHub.
fn hub() -> (Arc<ToolHub>, Arc<MemoryHub>) {
    tool_hub(
        Arc::new(StubWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    )
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

async fn create_task(tools: &ToolHub, title: &str, body: &str) -> (u64, String) {
    let text = call(
        tools,
        "task_create",
        json!({ "title": title, "body": body }),
    )
    .await
    .expect("create");
    let number = receipt_number(&text);
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
    assert!(text.contains("replaced"), "квитанция: {text}");
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
        text.contains(&format!("{} bytes", stored.len())),
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
    assert!(err.contains("stale base_hash"), "отказ по свежести: {err}");
    assert!(
        err.contains("current body_hash: "),
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
        text.contains("appended at the end"),
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
    assert!(err.contains("namesake exists at h3"), "гард: {err}");
    assert!(
        err.contains("pinned `### Суть изменений`"),
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
    assert!(text.contains("replaced"), "квитанция: {text}");
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
    assert!(err.contains("nothing to clear"), "{err}");
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
    assert!(text.contains("bytes"), "размер в квитанции: {text}");
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
    assert!(err.contains("base_hash is required"), "{err}");
    let err = call(
        &tools,
        "task_body_section",
        json!({ "number": number, "section": "", "content": "x", "base_hash": "ab" }),
    )
    .await
    .expect_err("section обязателен");
    assert!(err.contains("section is required"), "{err}");
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
    assert!(err.contains("unknown type"), "{err}");
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
    assert!(err.contains("outside the pipeline canon"), "{err}");
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
    assert!(err.contains("Priority"), "перечень без Приоритета: {err}");
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
        goals: Arc::new(uwuwu_usecase::GoalsView::new(
            memory.clone(),
            vec!["uwuwu".to_string()],
            "https://github.com/acme/hub/milestones".into(),
        )),
        docs: Arc::new(DocsView::new(wiki.clone())),
        person: Arc::new(PersonView::new(wiki.clone())),
        writer: Arc::new(TaskWriteUseCase {
            board: board.clone(),
            fields,
            boards,
            clock: Arc::new(SystemClock),
            state: Arc::new(StubState),
        }),
        clean: Arc::new(uwuwu_usecase::TaskCleanUseCase {
            clean: memory.clone() as Arc<dyn uwuwu_usecase::CleanBoardRepo>,
            journals: Arc::new(super::test_support::StubJournals::default()),
            boards: vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())],
            clock: Arc::new(SystemClock),
        }),
        status: Arc::new(uwuwu_usecase::BoardStatusRenameUseCase {
            repo: memory.clone() as Arc<dyn uwuwu_usecase::BoardStatusRepo>,
            journal: Arc::new(super::test_support::StubPreImages),
            boards: vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())],
            clock: Arc::new(SystemClock),
        }),
        wiki,
        sessions: Arc::new(uwuwu_usecase::SessionRouter::uniform(sessions)),
        search: Arc::new(SearchUseCase { semantic: None }),
        blobs: blob_usecase(),
        crons: Arc::new(uwuwu_usecase::CronUseCase::new(
            Arc::new(StubWiki),
            Arc::new(uwuwu_usecase::SessionRouter::uniform(Arc::new(
                StubSessions,
            ))),
            Arc::new(SystemClock),
            Arc::new(uwuwu_usecase::SpawnModels::canonical()),
            Default::default(),
            7,
            Arc::new(StubCronState::default()),
            None,
        )),
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

/// fact_track на MCP-поверхности (#1631): create рекламит ключ,
/// task_edit — нет (create-only, immutable; чужой ключ режет гард #1572).
#[test]
fn descriptors_expose_fact_track_create_only() {
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
        create.get("fact_track").is_some(),
        "create рекламит fact_track: {create:?}"
    );
    let edit = find("task_edit");
    assert!(
        edit.get("fact_track").is_none(),
        "edit не рекламит fact_track (create-only): {edit:?}"
    );
}

/// #1619: ключ ответа владельца объявлен на task_edit — гард #1572
/// принимает его как легальный, агент видит аргумент в tools/list.
#[test]
fn descriptor_exposes_owner_answer_on_task_edit() {
    let list = tool_descriptors();
    let edit = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"].as_str() == Some("task_edit"))
        .expect("task_edit на поверхности")["inputSchema"]["properties"]
        .clone();
    assert!(
        edit.get("owner_answer").is_some(),
        "edit рекламит owner_answer: {edit:?}"
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

/// Не-boolean automerge (мусор против boolean-схемы) — громкий отказ:
/// on-мандат не интерпретируется молча (ревью #1357 Low, круг 1→2).
/// Явный null — «не задано» (клиенты шлют), дефолт по классу строки:
/// обычная → on, direct → off.
#[tokio::test]
async fn task_create_non_boolean_automerge_is_loud_not_on_mandate() {
    let (tools, _memory) = hub();
    let garbage = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "мусор вместо automerge", "body": "б", "direct": true, "automerge": "on" }),
    )
    .await;
    assert!(
        garbage.is_err(),
        "строка-мусор не превращается в on-мандат молча"
    );
    let rejected = garbage.unwrap_err();
    assert!(
        rejected.contains("boolean"),
        "отказ называет ожидаемый тип: {rejected}"
    );
    // legacy-ключ с мусором — тот же отказ (читается как automerge)
    let legacy = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "мусор в legacy", "body": "б", "autodone": 1 }),
    )
    .await;
    assert!(legacy.is_err(), "legacy-ключ — та же честность");
    // явный null — «не задано»: обычная строка получает дефолт on
    let null_on = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "null как не задано", "body": "б", "automerge": null }),
    )
    .await
    .expect("null — не мусор, дефолт конвейера");
    assert!(null_on.contains("(Analys)"), "{null_on}");
    // direct + явный null — дефолт класса off, не мандат
    let created = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "direct с null", "body": "б", "direct": true, "automerge": null }),
    )
    .await
    .expect("null для direct — off без отказа");
    let n: u64 = created
        .trim()
        .split('#')
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .expect("номер в ответе");
    let card = call(
        &tools,
        "task_get",
        json!({ "project": "uwuwu", "number": n }),
    )
    .await
    .expect("task_get ок");
    assert!(card.contains("automerge: false"), "null не мандат: {card}");
    // direct + явный true — мандат владельца, on пишется
    let mandate = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "direct с явным мандатом", "body": "цитата поручения с мандатом", "direct": true, "automerge": true }),
    )
    .await
    .expect("явный boolean мандат accepted");
    let n: u64 = mandate
        .trim()
        .split('#')
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .expect("номер в ответе");
    let card = call(
        &tools,
        "task_get",
        json!({ "project": "uwuwu", "number": n }),
    )
    .await
    .expect("task_get ок");
    assert!(card.contains("automerge: true"), "мандат жив: {card}");
}

/// direct-круг (#1357) на MemoryHub-носителе: create(direct) рождает
/// строку в In Progress (fm несёт direct: true), агент сам финализирует
/// In Progress → Review; обычной строке то же ребро закрыто.
#[tokio::test]
async fn task_create_direct_is_in_progress_and_finishes_to_review() {
    let (tools, _memory) = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "direct: сделать в моменте", "body": "цитата поручения", "direct": true }),
    )
    .await
    .expect("direct-create accepted");
    assert!(
        created.contains("In Progress"),
        "ответ тула честен: {created}"
    );
    let n: u64 = created
        .trim()
        .split('#')
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .expect("номер в ответе");
    let card = call(
        &tools,
        "task_get",
        json!({ "project": "uwuwu", "number": n }),
    )
    .await
    .expect("task_get ок");
    assert!(card.contains("— In Progress"), "статус в fm: {card}");
    assert!(card.contains("direct: true"), "fm direct: {card}");
    assert!(
        card.contains("automerge: false"),
        "direct-дефолт automerge off: {card}"
    );
    // агент финализирует сам
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": n, "status": "Review" }),
    )
    .await
    .expect("direct-ребро In Progress → Review открыто");
}

/// Обычная строка не может прыгнуть In Progress → Review (#1357-регресс:
/// канон финализации через ревью-конвейер не ослаблен).
#[tokio::test]
async fn task_edit_in_progress_to_review_rejected_without_direct() {
    let (tools, _memory) = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "обычная строка", "body": "б" }),
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
    // Analys → In Progress (fast-track легален), затем прыжок в Review
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": n, "priority": "medium", "automerge": true }),
    )
    .await
    .expect("поля разбора заполнены (гейт Analys)");
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": n, "status": "In Progress" }),
    )
    .await
    .expect("fast-track легален");
    let res = call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": n, "status": "Review" }),
    )
    .await;
    assert!(res.is_err(), "без direct-флага ребро закрыто каноном");
}

/// fact_track-круг (#1631) на MemoryHub-носителе: create(fact_track)
/// рождает обычную строку в Analys (fm несёт fact_track: true), вход —
/// рукой через fast-track, дальше укороченный пайп: ReviewResult →
/// Review открыто, а оба входа в Testing закрыты.
#[tokio::test]
async fn task_create_fact_track_short_pipe() {
    let (tools, _memory) = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "fact_track: очевидное", "body": "б", "fact_track": true }),
    )
    .await
    .expect("fact_track-create accepted");
    assert!(created.contains("(Analys)"), "ответ тула честен: {created}");
    let n: u64 = created
        .trim()
        .split('#')
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .expect("номер в ответе");
    let card = call(
        &tools,
        "task_get",
        json!({ "project": "uwuwu", "number": n }),
    )
    .await
    .expect("task_get ок");
    assert!(card.contains("fact_track: true"), "fm fact_track: {card}");
    // вход рукой: Analys → In Progress (гейт полей + fast-track)
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": n, "priority": "medium", "automerge": true }),
    )
    .await
    .expect("поля разбора заполнены (гейт Analys)");
    for to in ["In Progress", "Result review"] {
        call(
            &tools,
            "task_edit",
            json!({ "project": "uwuwu", "number": n, "status": to }),
        )
        .await
        .expect("ход {to} легален");
    }
    // вердикт approve — сразу Review, мимо Testing
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": n, "status": "Review" }),
    )
    .await
    .expect("fact_track-ребро ReviewResult → Review открыто");
}

/// fact_track-строке оба входа в тест-контур закрыты (#1631, негативы).
#[tokio::test]
async fn task_edit_fact_track_cannot_enter_testing() {
    let (tools, _memory) = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "fact_track: без тестов", "body": "б", "fact_track": true }),
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
        json!({ "project": "uwuwu", "number": n, "priority": "medium", "automerge": true }),
    )
    .await
    .expect("поля разбора заполнены (гейт Analys)");
    call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": n, "status": "In Progress" }),
    )
    .await
    .expect("fast-track легален");
    let res = call(
        &tools,
        "task_edit",
        json!({ "project": "uwuwu", "number": n, "status": "Testing" }),
    )
    .await;
    assert!(res.is_err(), "прямая отправка на тест закрыта флагом");
}

/// direct + fact_track взаимоисключающи (#1631): create с обоими
/// true — громкий отказ, строка не рождается.
#[tokio::test]
async fn task_create_direct_and_fact_track_are_mutually_exclusive() {
    let (tools, _memory) = hub();
    let res = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "оба флага", "body": "б", "direct": true, "fact_track": true }),
    )
    .await;
    assert!(res.is_err(), "оба флага — отказ: {res:?}");
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

/// #1604 (срез 1): task_list отдаёт машиночитаемые JSON rows — массив
/// объектов number/status/priority/type/title; двухсловный статус
/// («Needs attention») выживает сериализацию целым полем, канонные имена
/// из словаря MCP-поверхности. Факт до фикса: ответ — текстовая таблица,
/// ручной колоночный парс рвал «Needs attention» на две колонки.
#[tokio::test]
async fn task_list_returns_json_rows_with_two_word_status_intact() {
    let (tools, _memory) = hub();
    // direct-create → In Progress: ребро In Progress → Needs attention
    // легально без аналитик-гейта (тот требует Priority на выходе из Analys)
    let created = call(
        &tools,
        "task_create",
        json!({
            "project": "uwuwu",
            "title": "t: json rows",
            "body": "## Постановка\n\nx.\n",
            "direct": true
        }),
    )
    .await
    .expect("direct create");
    let number: u64 = created
        .trim()
        .split('#')
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .and_then(|s| s.parse().ok())
        .expect("номер в ответе");
    call(
        &tools,
        "task_edit",
        json!({ "number": number, "status": "Needs attention" }),
    )
    .await
    .expect("needs attention edit");

    let list = call(&tools, "task_list", json!({ "project": "uwuwu" }))
        .await
        .expect("list");
    let rows: Value = serde_json::from_str(&list)
        .unwrap_or_else(|e| panic!("ответ не JSON-массив rows: {e}: {list}"));
    let arr = rows.as_array().expect("rows — JSON-массив");
    let row = arr
        .iter()
        .find(|r| r["title"] == "t: json rows")
        .expect("строка в списке");
    assert_eq!(row["number"], json!(number), "номер строки: {row}");
    assert_eq!(
        row["status"],
        json!("Needs attention"),
        "двухсловный статус целым полем: {row}"
    );
    assert_eq!(
        row["priority"],
        json!("medium"),
        "create-дефолт priority канонным именем: {row}"
    );
    assert_eq!(
        row["type"],
        json!("feature"),
        "create-дефолт типа канонным именем (#1271): {row}"
    );
}

/// #1604 (срез 1): «доска пуста» программно отличима от «ответ не читается» —
/// пустая доска отдаёт JSON `[]`; отказ чтения идёт isError-конвертом и
/// бросается в скрипте (вне этой поверхности).
#[tokio::test]
async fn task_list_empty_board_reads_as_empty_json_array() {
    let (tools, _memory) = hub();
    let list = call(&tools, "task_list", json!({ "project": "uwuwu" }))
        .await
        .expect("list");
    let rows: Value = serde_json::from_str(&list).expect("пустая доска — JSON, не таблица");
    assert_eq!(rows, json!([]), "пустая доска — пустой JSON-массив: {list}");
}

/// #1604 (срез 2): состав вывода task_get заявлен — body_bytes (точный
/// размер тела в UTF-8; потребитель Code Mode сверяет через TextEncoder)
/// и граница хвоста комментов «latest показано of всего». Инстанс 21.11:
/// ложный сплит тела по заголовку-двойнику «## Комменты» стал бы виден
/// сверкой (размер не сойдётся).
#[tokio::test]
async fn task_get_declares_body_bytes_and_comments_window_on_overflow() {
    let (tools, _memory) = hub();
    let body =
        "## Постановка\n\nтело состава вывода.\n\n## Комменты\n\nзаголовок-двойник внутри тела.\n";
    let (number, _) = create_task(&tools, "t: состав вывода", body).await;
    for i in 0..12 {
        call(
            &tools,
            "task_comment",
            json!({ "number": number, "text": format!("коммент {i}") }),
        )
        .await
        .expect("comment");
    }

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");

    // Потребительская сверка (аналог TextEncoder в Code Mode): заявленное
    // body_bytes == байтам UTF-8 среза тела между fm-блоком и заголовком
    // комментов. Носитель нормализует хвостовой перенос — входную строку
    // не считаем, сверяем то, что реально в выводе.
    let start = card.find("body_bytes: ").expect("fm несёт body_bytes") + "body_bytes: ".len();
    let line_end = start + card[start..].find('\n').expect("конец строки body_bytes");
    let declared: usize = card[start..line_end].parse().expect("body_bytes — целое");
    let body_end = card
        .find("\n\n--- comments (latest 10 of 12) ---")
        .expect("граница хвоста комментов");
    // после строки body_bytes в fm идёт пустая строка (\n\n) — тело с неё
    assert_eq!(
        card[line_end + 2..body_end].len(),
        declared,
        "срез тела == заявленному body_bytes: {card}"
    );
    assert!(
        card.contains("--- comments (latest 10 of 12) ---"),
        "граница хвоста комментов заявлена показано/всего: {card}"
    );
}

/// #1604 (срез 2): в пределах окна состав заявлен без ложной
/// усечённости — «latest N of N».
#[tokio::test]
async fn task_get_comments_window_full_when_within_limit() {
    let (tools, _memory) = hub();
    let (number, _) = create_task(&tools, "t: окно целиком", "## П\n\nx.\n").await;
    for i in 0..2 {
        call(
            &tools,
            "task_comment",
            json!({ "number": number, "text": format!("к {i}") }),
        )
        .await
        .expect("comment");
    }

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    assert!(
        card.contains("--- comments (latest 2 of 2) ---"),
        "полное окно — без «усечённости»: {card}"
    );
}

/// #1604 (тест-ревью Common-1): слово «latest» в заголовке хвоста —
/// обещание состава и порядка: при переполнении окна под ним лежат
/// именно последние N комментов в хронологическом порядке, старейшие
/// остаются за границей.
#[tokio::test]
async fn task_get_comments_tail_carries_latest_ten_in_chronological_order() {
    let (tools, _memory) = hub();
    let (number, _) = create_task(&tools, "t: хвост latest", "## П\n\nx.\n").await;
    for i in 0..12 {
        call(
            &tools,
            "task_comment",
            json!({ "number": number, "text": format!("коммент {i} из 12") }),
        )
        .await
        .expect("comment");
    }

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let tail_at = card
        .find("--- comments (latest 10 of 12) ---")
        .expect("граница хвоста заявлена");
    let tail = &card[tail_at..];
    assert!(
        !tail.contains("коммент 0 из 12"),
        "старейший 0 за границей окна: {tail}"
    );
    assert!(
        !tail.contains("коммент 1 из 12"),
        "старейший 1 за границей окна: {tail}"
    );
    let mut at = 0;
    for i in 2..12 {
        let marker = format!("коммент {i} из 12");
        let found = tail[at..]
            .find(&marker)
            .unwrap_or_else(|| panic!("{marker} в хвосте после предыдущего: {}", &tail[at..]));
        at += found;
    }
}

/// #1604 (тест-ревью Common-2): сериализация компактная — одна строка;
/// pretty парсится так же и тесты проходят, но на большой доске съедает
/// окно показа финального результата.
#[tokio::test]
async fn task_list_json_is_single_line_compact() {
    let (tools, _memory) = hub();
    create_task(&tools, "t: компакт", "## П\n\nx.\n").await;

    let list = call(&tools, "task_list", json!({ "project": "uwuwu" }))
        .await
        .expect("list");
    assert!(
        !list.contains('\n'),
        "компактный JSON — одна строка: {list}"
    );
    let rows: Value = serde_json::from_str(&list).expect("одна строка — валидный JSON");
    assert!(
        rows.as_array()
            .expect("массив rows")
            .iter()
            .any(|r| r["title"] == "t: компакт"),
        "строка в списке: {list}"
    );
}

/// #1604 (тест-ревью Common-3): опечатка в проекте — громкий отказ
/// с именем проекта для обоих тулов чтения, не тихий пустой список
/// (симптом #1548) и не карточка чужой доски с тем же номером.
#[tokio::test]
async fn unknown_project_is_a_loud_error_for_list_and_get() {
    let (tools, _memory) = hub();
    let (number, _) = create_task(&tools, "t: опечатка в проекте", "## П\n\nx.\n").await;

    let list_err = call(&tools, "task_list", json!({ "project": "опечатка" }))
        .await
        .expect_err("неизвестный проект — отказ");
    assert!(
        list_err.contains("опечатка"),
        "имя проекта в отказе: {list_err}"
    );
    let get_err = call(
        &tools,
        "task_get",
        json!({ "number": number, "project": "опечатка" }),
    )
    .await
    .expect_err("неизвестный проект — отказ");
    assert!(
        get_err.contains("опечатка"),
        "имя проекта в отказе: {get_err}"
    );
}

/// #1604 (срез 2): body_bytes — байты UTF-8, а не символы. Проба несёт
/// астральный эмодзи: только он разводит все три единицы — байты (4),
/// chars (1) и UTF-16-единицы JS `.length` (2); без астральности третья
/// единица совпадает с chars и проверка слепа.
#[tokio::test]
async fn task_get_body_bytes_counts_utf8_bytes_not_chars_on_astral_emoji() {
    let (tools, _memory) = hub();
    // Носитель нормализует хвостовой перенос — входную строку create не
    // считаем; сверяем срез тела как он реально лежит в выводе.
    let (number, _) = create_task(
        &tools,
        "t: байты, не символы",
        "## Постановка\n\nзапуск 🚀 ок.\n",
    )
    .await;

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    // Комментов у карточки нет — секции хвоста нет и срез тела идёт до
    // конца вывода; иначе в срез попал бы чужой контент.
    assert!(
        !card.contains("--- comments"),
        "секции хвоста нет, срез до конца вывода: {card}"
    );
    let start = card.find("body_bytes: ").expect("fm несёт body_bytes") + "body_bytes: ".len();
    let line_end = start + card[start..].find('\n').expect("конец строки body_bytes");
    let declared: usize = card[start..line_end].parse().expect("body_bytes — целое");
    let slice = &card[line_end + 2..];
    assert_ne!(
        slice.chars().count(),
        slice.len(),
        "проба обязана разводить символы и байты: {slice}"
    );
    assert_eq!(
        declared,
        slice.len(),
        "заявлены байты UTF-8, не chars: {card}"
    );
    assert_ne!(
        slice.encode_utf16().count(),
        slice.chars().count(),
        "эмодзи астральный: единицы UTF-16 и chars расходятся"
    );
}

/// #1604 (срез 2): карточка без комментов не несёт секцию хвоста —
/// заголовок «latest 0 of 0» вводил бы в заблуждение так же, как
/// молчаливый сброс сверх 10 до среза 2.
#[tokio::test]
async fn task_get_without_comments_carries_no_comments_section() {
    let (tools, _memory) = hub();
    let (number, _) = create_task(&tools, "t: без комментов", "## Постановка\n\nтихо.\n").await;

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    assert!(
        !card.contains("--- comments"),
        "секции хвоста нет, а не «latest 0 of 0»: {card}"
    );
    assert!(
        card.contains("body_bytes: "),
        "состав заявлен и без хвоста: {card}"
    );
}

/// #1632+#1604: зеркало шапки (strip_task_get_header в фазе записи)
/// собрано из РЕАЛЬНОГО рендера task_get, не из рукописной шапки, и от
/// card[0..] — тайтл-строка включительно, ровно то, что агент копирует
/// из карточки (старт от fm_start не ловит дрейф «новая первая
/// строка»): копия вывода карточки в task_body срезает шапку целиком
/// (все наборы ключей, включая body_bytes), тело на носителе чистое
/// (ADR-105); шапка без тела — отказ «нет эффективного контента»
/// (#1572) с хвостовым переносом и без него (терминатор #1604),
/// носитель цел.
#[tokio::test]
async fn task_body_strips_the_real_render_header_and_refuses_header_only() {
    let (tools, memory) = hub();
    let body = "## Постановка\n\nчистое тело носителя.\n";
    let (number, _) = create_task(&tools, "t: зеркало шапки", body).await;

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let body_at = card.find(body).expect("тело в выводе карточки");
    let copied = &card[..body_at + body.len()];

    let receipt = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": copied }),
    )
    .await
    .expect("task_body ок");
    assert!(receipt.contains("replaced"), "квитанция: {receipt}");

    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert_eq!(stored, body, "тело без шапки: {stored}");

    let header_only = &card[..body_at];
    let err = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": header_only }),
    )
    .await
    .expect_err("шапка без тела — отказ");
    assert!(
        err.contains("effective content"),
        "отказ называет отсутствие эффективного контента: {err}"
    );
    let err2 = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": header_only.trim_end() }),
    )
    .await
    .expect_err("шапка без переноса — тоже отказ");
    assert!(
        err2.contains("effective content"),
        "отказ без хвостового переноса: {err2}"
    );
    let intact = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert_eq!(intact, body, "оба отказа не трогают носитель");
}

/// #1637: CRLF-копия реальной карточки task_get срезается на границе
/// task_body; байты тела остаются CRLF — сервер не нормализует контент.
#[tokio::test]
async fn task_body_strips_crlf_real_render_header() {
    let (tools, memory) = hub();
    let body = "## Постановка\n\nчистое тело носителя.\n";
    let (number, _) = create_task(&tools, "t: CRLF-зеркало", body).await;

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let body_at = card.find(body).expect("тело в выводе карточки");
    let copied = &card[..body_at + body.len()];
    let crlf = copied.replace('\n', "\r\n");

    let receipt = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": &crlf }),
    )
    .await
    .expect("task_body ок");
    assert!(receipt.contains("replaced"), "квитанция: {receipt}");

    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert_eq!(
        stored,
        body.replace('\n', "\r\n"),
        "тело без шапки: {stored}"
    );
}

/// #1637: CRLF-шапка без тела упирается в отказ #1572 и не трогает носитель.
#[tokio::test]
async fn task_body_refuses_crlf_header_only() {
    let (tools, memory) = hub();
    let body = "## Постановка\n\nчистое тело.\n";
    let (number, _) = create_task(&tools, "t: CRLF-шапка без тела", body).await;

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let body_at = card.find(body).expect("тело в выводе карточки");
    let crlf_header = card[..body_at].replace('\n', "\r\n");
    let before = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;

    let err = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": &crlf_header }),
    )
    .await
    .expect_err("CRLF-шапка без тела — отказ");
    assert!(
        err.contains("effective content"),
        "отказ называет пустой эффективный контент: {err}"
    );
    let intact = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert_eq!(intact, before, "носитель цел: {intact}");
}

/// #1637: task_create получает CRLF-копию карточки — новая строка несёт
/// чистое тело без тайтл-строки рендера и fm-блока.
#[tokio::test]
async fn task_create_strips_crlf_card_header() {
    let (tools, memory) = hub();
    let body = "## Постановка\n\nчистое тело носителя.\n";
    let (number, _) = create_task(&tools, "t: источник CRLF", body).await;
    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let body_at = card.find(body).expect("тело в выводе карточки");
    let crlf_card = format!(
        "{}{}",
        card[..body_at].replace('\n', "\r\n"),
        body.replace('\n', "\r\n")
    );

    let text = call(
        &tools,
        "task_create",
        json!({ "title": "t: CRLF create", "body": &crlf_card }),
    )
    .await
    .expect("create ок");
    let created = memory
        .read_issue("Задачи uwuwu", receipt_number(&text))
        .await
        .expect("read")
        .body;
    assert!(!created.contains("--- fm ---"), "fm вырезан: {created}");
    assert!(
        !created.contains("t: источник CRLF"),
        "тайтл-строка вырезана: {created}"
    );
    assert!(
        created.contains("чистое тело носителя."),
        "тело доставлено: {created}"
    );
}

/// Тестер #1637 (Low код-ревью): точный оракул на связке MCP — create из
/// CRLF-копии карточки даёт те же байты, что create из чистого CRLF-тела.
/// Прежний пин проверял только `contains` и пропустил бы обрезку тела.
#[tokio::test]
async fn task_create_strips_crlf_card_header_byte_exact() {
    let (tools, memory) = hub();
    let body = "## Постановка\r\n\r\nчистое тело.\r\n";
    let (number, _) = create_task(&tools, "t: CRLF-источник точного оракула", body).await;
    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let body_at = card.find(body).expect("тело в выводе карточки");
    let crlf_card = format!("{}{}", card[..body_at].replace('\n', "\r\n"), body);

    let (control, _) = create_task(&tools, "t: контроль CRLF create", body).await;
    let control_stored = memory
        .read_issue("Задачи uwuwu", control)
        .await
        .expect("read control")
        .body;

    let text = call(
        &tools,
        "task_create",
        json!({ "title": "t: CRLF create точный оракул", "body": &crlf_card }),
    )
    .await
    .expect("task_create");
    let created = memory
        .read_issue("Задачи uwuwu", receipt_number(&text))
        .await
        .expect("read created")
        .body;
    assert_eq!(
        created, control_stored,
        "create из CRLF-копии = create из чистого тела, байт-в-байт: {created}"
    );
    assert!(
        !created.contains("t: CRLF-источник точного оракула"),
        "тайтл-строка рендера не легла: {created}"
    );
    assert!(!created.contains("--- fm ---"), "fm-блок не лёг: {created}");
}

/// #1664: голый CRLF-легаси-fm (без карточки task_get) проходит второй
/// стриппер границы; носитель несёт чистое тело, квитанция честная.
#[tokio::test]
async fn task_body_strips_crlf_legacy_fm() {
    let (tools, memory) = hub();
    let body = "## Постановка\n\nчистое тело.\n";
    let (number, _) = create_task(&tools, "t: CRLF-fm", body).await;
    let crlf_body = body.replace('\n', "\r\n");
    let with_fm = format!("---\r\ntitle: legacy\r\nstatus: Draft\r\n---\r\n\r\n{crlf_body}");

    let receipt = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": &with_fm }),
    )
    .await
    .expect("task_body ок");
    assert!(receipt.contains("replaced"), "квитанция: {receipt}");

    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert_eq!(stored, crlf_body, "тело без fm: {stored}");
    assert!(
        receipt.contains(&uwuwu_usecase::body_hash(&stored)),
        "хэш квитанции = хэш записанного тела: {receipt}"
    );
}

/// #1664: один CRLF-fm без эффективного контента упирается в отказ #1572.
#[tokio::test]
async fn task_body_refuses_crlf_fm_only() {
    let (tools, memory) = hub();
    let (number, _) = create_task(&tools, "t: CRLF-fm-only", &big_body()).await;
    let before = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;

    let err = call(
        &tools,
        "task_body",
        json!({
            "number": number,
            "body": "---\r\ntitle: x\r\nstatus: Draft\r\n---\r\n\r\n"
        }),
    )
    .await
    .expect_err("CRLF-fm без тела — отказ");
    assert!(
        err.contains("effective content"),
        "отказ называет пустой эффективный контент: {err}"
    );
    let intact = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert_eq!(intact, before, "носитель цел");
}

/// #1664: task_create с CRLF-легаси-fm даёт те же байты, что create из
/// чистого CRLF-тела.
#[tokio::test]
async fn task_create_strips_crlf_legacy_fm_byte_exact() {
    let (tools, memory) = hub();
    let body = "## Постановка\r\n\r\nчистое тело.\r\n";
    let (control, _) = create_task(&tools, "t: контроль CRLF-create", body).await;
    let control_stored = memory
        .read_issue("Задачи uwuwu", control)
        .await
        .expect("read control")
        .body;
    let with_fm = format!("---\r\ntitle: legacy\r\nstatus: Draft\r\n---\r\n\r\n{body}");

    let text = call(
        &tools,
        "task_create",
        json!({ "title": "t: CRLF-fm create", "body": &with_fm }),
    )
    .await
    .expect("task_create ок");
    let created = memory
        .read_issue("Задачи uwuwu", receipt_number(&text))
        .await
        .expect("read created")
        .body;
    assert_eq!(created, control_stored, "байт-в-байт: {created}");
    assert!(!created.contains("status: Draft"), "fm не лёг: {created}");
}

/// #1604 (ревью раунда 5, Low-2): строгость зеркала держится и на
/// актуальной 8-ключевой форме (fact_track + body_bytes после слива
/// #1631) — чужая строка между type и body_hash ломает срез, и вся
/// копия карточки пишется телом целиком. Пин собран из реального
/// рендера.
#[tokio::test]
async fn task_body_keeps_real_render_header_with_foreign_line() {
    let (tools, memory) = hub();
    let body = "## Постановка\n\nчистое тело.\n";
    let (number, _) = create_task(&tools, "t: чужая строка в шапке", body).await;

    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let fm_start = card.find("--- fm ---").expect("fm-блок");
    let body_at = card.find(body).expect("тело в выводе карточки");
    let header = &card[fm_start..body_at];

    let at = header.find("body_hash: ").expect("body_hash в шапке");
    let poisoned = format!("{}чужой ключ: x\n{}", &header[..at], &header[at..]);
    let payload = format!("{poisoned}{body}");

    let receipt = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": &payload }),
    )
    .await
    .expect("task_body ок");
    assert!(receipt.contains("replaced"), "квитанция: {receipt}");

    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert_eq!(
        stored, payload,
        "чужой ключ ломает срез — копия карточки пишется целиком: {stored}"
    );
}

/// #1604 (ревью Low): null-хвост закреплён через хендлер task_list, не
/// только юнит-тестом хелпера — сид-строка с Status::Empty,
/// Priority::Empty и без типа даёт все три null-ячейки разом.
#[tokio::test]
async fn task_list_renders_null_cells_for_empty_status_priority_and_type() {
    let memory = Arc::new(MemoryHub::with_seed(Some((7, uwuwu_domain::Status::Empty))));
    let board: Arc<dyn uwuwu_usecase::TaskBoardRepo> = memory.clone();
    let (tools, _aux) = tool_hub_on_board(
        Arc::new(StubWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
        board,
        memory,
    );

    let list = call(&tools, "task_list", json!({ "project": "uwuwu" }))
        .await
        .expect("list");
    let rows: Value = serde_json::from_str(&list).expect("rows — JSON");
    let row = rows
        .as_array()
        .expect("массив rows")
        .iter()
        .find(|r| r["number"] == json!(7))
        .expect("сид-строка в списке");
    assert_eq!(row["status"], Value::Null, "Status::Empty → null: {row}");
    assert_eq!(
        row["priority"],
        Value::Null,
        "Priority::Empty → null: {row}"
    );
    assert_eq!(row["type"], Value::Null, "тип отсутствует → null: {row}");
}

/// #1155 (поручение владельца): автомерж-ячейка живёт на mock-носителе —
/// create пишет дефолт on, edit переключает, fm рендерит фактическое
/// состояние (без хранения на носителе fm и тик стенда врали «false»).
/// Приёмочный путь MCP-клиента (#1298): tools/list поверх протокола
/// (POST /mcp) отдаёт схему task_create.type, совпадающую с валидацией —
/// выпиленные #1135 idea/analys в списке не обещаются, а вызов с ними
/// громко отвергается до мутаций доски.
#[tokio::test]
async fn mcp_tools_list_serves_canonical_task_types_only() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (tools, _memory) = hub();
    let state = Arc::new(super::McpState {
        bearer: Arc::new("t-bearer".into()),
        tools: tools.clone(),
    });
    let app = super::mcp_router(state);

    let post = |body: Value| {
        Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("authorization", "Bearer t-bearer")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    // tools/list: в схеме type только канон 9 типов.
    let res = app
        .clone()
        .oneshot(post(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let schema_type = json["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "task_create")
        .expect("task_create в tools/list")["inputSchema"]["properties"]["type"]["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !schema_type.contains("idea") && !schema_type.contains("analys"),
        "схема обещает выпиленное: {schema_type}"
    );
    for canonical in [
        "bug",
        "spike",
        "docs",
        "other",
        "feature",
        "refactoring",
        "legacy",
        "qa",
        "infrastructure",
    ] {
        assert!(
            schema_type.split('|').any(|t| t.trim() == canonical),
            "«{canonical}» нет в схеме: {schema_type}"
        );
    }

    // tools/call с выпиленным типом — громкий отказ, доска не мутируется.
    let res = app
        .oneshot(post(json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"task_create","arguments":{
                "project":"uwuwu","title":"т: idea не по схеме","type":"idea"
            }}
        })))
        .await
        .unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["result"]["isError"], Value::Bool(true));
    let text = json["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains("unknown type"), "{text}");
    let list = call(&tools, "task_list", json!({ "project": "uwuwu" }))
        .await
        .expect("list");
    assert!(
        !list.contains("т: idea не по схеме"),
        "отказ валидации создал строку: {list}"
    );
}

/// Статусная строка схемы task_edit несёт весь канон статусов доски
/// (#1413): агент конвейера ставит статусы по методичке — дрейф
/// «статус есть в каноне, нет в схеме тула» ловится здесь, а не в
/// рантайме (семейство пина типов #1298).
#[tokio::test]
async fn mcp_task_edit_schema_lists_canonical_statuses() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let (tools, _memory) = hub();
    let state = Arc::new(super::McpState {
        bearer: Arc::new("t-bearer".into()),
        tools: tools.clone(),
    });
    let app = super::mcp_router(state);

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("authorization", "Bearer t-bearer")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let schema_status = json["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "task_edit")
        .expect("task_edit в tools/list")["inputSchema"]["properties"]["status"]["description"]
        .as_str()
        .unwrap()
        .to_string();
    let list = schema_status
        .split_once("canon status:")
        .map(|(_, rest)| rest)
        .unwrap_or(&schema_status);
    for canonical in [
        "Draft",
        "Analys",
        "Review Analys",
        "Todo",
        "Design",
        "Plan review",
        "In Progress",
        "Result review",
        "Testing",
        "Testing review",
        "Review",
        "Needs attention",
        "Wait",
    ] {
        assert!(
            list.split('/').any(|s| s.trim() == canonical),
            "«{canonical}» нет в схеме task_edit: {schema_status}"
        );
    }
}

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

// ——— blob-канал (#1292) — полный туловой контур поверх memory-стора ———

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[tokio::test]
async fn blob_put_returns_key_and_url_get_resigns_by_key() {
    let (tools, _memory) = hub();
    let png = b"\x89PNG-fake-screenshot-bytes";

    let put = call(
        &tools,
        "blob_put",
        json!({
            "filename": "скрин от МПНЧ.png",
            "data_base64": base64_of(png),
            "number": 1292
        }),
    )
    .await
    .expect("put ok");
    let key = put
        .lines()
        .find_map(|l| l.strip_prefix("key: "))
        .expect("key in the receipt");
    assert!(key.starts_with("blobs/uwuwu/1292/"), "{put}");
    assert!(key.ends_with("скрин от МПНЧ.png"), "имя сохранено: {put}");
    assert!(put.contains("URL (TTL 7 days): memory://"), "{put}");
    assert!(put.contains("image/png"), "{put}");

    let get = call(&tools, "blob_get", json!({ "key": key }))
        .await
        .expect("get ok");
    assert!(
        get.contains(&format!("URL (TTL 7 days): memory://{key}")),
        "{get}"
    );
    assert!(
        get.contains("object existence is not checked"),
        "семантика L-1 в ответе: {get}"
    );
}

#[tokio::test]
async fn blob_put_presign_mode_returns_both_urls_without_bytes() {
    let (tools, _memory) = hub();
    let put = call(
        &tools,
        "blob_put",
        json!({ "filename": "скрин от МПНЧ.png", "number": 1537 }),
    )
    .await
    .expect("reserve ok");
    let key = put
        .lines()
        .find_map(|l| l.strip_prefix("key: "))
        .expect("key in the receipt");
    assert!(key.starts_with("blobs/uwuwu/1537/"), "{put}");
    assert!(put.contains("PUT URL (TTL 300 s"), "{put}");
    assert!(put.contains("memory-put://"), "{put}");
    assert!(put.contains("URL (TTL 7 days): memory://"), "{put}");
    assert!(put.contains("image/png"), "{put}");

    let get = call(&tools, "blob_get", json!({ "key": key }))
        .await
        .expect("get ok");
    assert!(get.contains(&format!("memory://{key}")), "{get}");
}

#[tokio::test]
async fn blob_put_empty_data_base64_is_loud_instead_of_silent_reserve() {
    let (tools, _memory) = hub();
    let err = call(
        &tools,
        "blob_put",
        json!({ "filename": "a.png", "data_base64": "   " }),
    )
    .await
    .expect_err("громкий отказ");
    assert!(err.contains("data_base64 is empty"), "{err}");
}

#[tokio::test]
async fn blob_put_null_data_base64_reserves_and_wrong_type_is_loud() {
    let (tools, _memory) = hub();
    let reserved = call(
        &tools,
        "blob_put",
        json!({ "filename": "a.png", "data_base64": null }),
    )
    .await
    .expect("null — отсутствие значения, как в optional_number");
    assert!(reserved.contains("PUT URL"), "{reserved}");

    let err = call(
        &tools,
        "blob_put",
        json!({ "filename": "a.png", "data_base64": 42 }),
    )
    .await
    .expect_err("громкий отказ на не-строку");
    assert!(err.contains("data_base64 must be a base64 string"), "{err}");
}

#[tokio::test]
async fn blob_put_without_number_goes_to_misc_segment() {
    let (tools, _memory) = hub();
    let put = call(
        &tools,
        "blob_put",
        json!({ "filename": "runbook.svg", "data_base64": base64_of(b"<svg/>") }),
    )
    .await
    .expect("put ok");
    assert!(put.contains("key: blobs/uwuwu/misc/"), "{put}");
    assert!(put.contains("image/svg+xml"), "{put}");
}

#[tokio::test]
async fn blob_put_rejects_bad_base64_loudly() {
    let (tools, _memory) = hub();
    let err = call(
        &tools,
        "blob_put",
        json!({ "filename": "a.png", "data_base64": "не-база64!!!" }),
    )
    .await
    .expect_err("громкий отказ");
    assert!(err.contains("data_base64"), "{err}");
}

#[tokio::test]
async fn blob_get_resigns_legacy_assets_key() {
    let (tools, _memory) = hub();
    let get = call(
        &tools,
        "blob_get",
        json!({ "key": "assets/task-1325/dash-cam-buttons.png" }),
    )
    .await
    .expect("legacy-ключ чтения легален");
    assert!(
        get.contains("memory://assets/task-1325/dash-cam-buttons.png"),
        "{get}"
    );
}

#[tokio::test]
async fn blob_get_rejects_malformed_key() {
    let (tools, _memory) = hub();
    for key in [
        "etc/passwd",
        "blobs//x",
        "blobs/a/../../x",
        "assets/../etc",
        "assets/",
    ] {
        let err = call(&tools, "blob_get", json!({ "key": key }))
            .await
            .expect_err("громкий отказ");
        assert!(err.contains("blobs/"), "{key}: {err}");
    }
}

#[tokio::test]
async fn blob_tools_fail_loud_without_env() {
    // Отдельный hub с несконфигурированным каналом: отказ — перечень env,
    // не тихая слепота (ADR-010). Фабрика с missing-бэкендом.
    let (tools, _memory) = tool_hub(
        Arc::new(StubWiki),
        blob_usecase_missing(&["UWUWU_S3_ENDPOINT", "UWUWU_S3_BUCKET"]),
        Arc::new(StubCronState::default()),
    );
    for args in [
        json!({ "filename": "a.png", "data_base64": base64_of(b"x") }),
        json!({ "filename": "a.png" }),
    ] {
        let err = call(&tools, "blob_put", args)
            .await
            .expect_err("громкий отказ");
        assert!(err.contains("blob is not configured"), "{err}");
        assert!(
            err.contains("UWUWU_S3_ENDPOINT") && err.contains("UWUWU_S3_BUCKET"),
            "{err}"
        );
    }
}

#[tokio::test]
async fn blob_put_over_limit_is_loud_through_dispatcher() {
    // Лимит 20 MiB через живой диспетчер: base64-конверт декодируется на
    // границе, отказ юзкейса доходит агенту как текст ошибки (тестировщик
    // #1292 — диспетчерский слой поверх юзкейс-теста place()).
    let (tools, _memory) = hub();
    let big = vec![b'a'; uwuwu_usecase::BLOB_MAX_BYTES + 1];
    let err = call(
        &tools,
        "blob_put",
        json!({
            "project": "uwuwu",
            "number": 1292,
            "filename": "big.bin",
            "data_base64": base64_of(&big),
        }),
    )
    .await
    .expect_err("громкий отказ по лимиту");
    assert!(err.contains("blob too big"), "{err}");
    assert!(err.contains("the UC-06 precedent"), "{err}");
}

// ===== Батч-снос #1158: поверхность MCP (dry-run/confirm/ack/undo) =====

/// Живая задача с содержательным телом (вердикт Live — ack обязателен).
fn live_body(title: &str) -> String {
    format!("## Постановка\n\nСодержательное тело задачи «{title}».\n")
}

#[tokio::test]
async fn task_clean_dry_run_verdicts_without_mutations() {
    let (tools, memory) = hub();
    // #1 — create без тела: скелет create → DraftSkeleton
    call(&tools, "task_create", json!({ "title": "пустышка" }))
        .await
        .expect("draft создана");
    // #2 — содержательное тело + close → Dead (не-Draft)
    call(
        &tools,
        "task_create",
        json!({ "title": "живая", "body": live_body("живая") }),
    )
    .await
    .expect("живая создана");
    call(
        &tools,
        "task_close",
        json!({ "number": 2, "not_planned": true }),
    )
    .await
    .expect("закрыта");
    let out = call(&tools, "task_clean", json!({ "ids": "1,2" }))
        .await
        .expect("dry-run");
    assert!(out.contains("DraftSkeleton"), "draft-вердикт: {out}");
    assert!(out.contains("Dead"), "dead-вердикт: {out}");
    assert!(out.contains("ack"), "подсказка про ack: {out}");
    assert!(out.contains("no mutations"), "заголовок dry-run: {out}");
    let rows = memory.read_board("Задачи uwuwu").await.unwrap();
    assert_eq!(rows.len(), 2, "dry-run — ноль мутаций");
}

#[tokio::test]
async fn task_clean_execute_refuses_live_without_ack() {
    let (tools, memory) = hub();
    call(
        &tools,
        "task_create",
        json!({ "title": "живая", "body": live_body("живая") }),
    )
    .await
    .expect("живая создана");
    let err = call(&tools, "task_clean", json!({ "ids": [1], "confirm": true }))
        .await
        .expect_err("Live без ack — отказ всего батча");
    assert!(err.contains("ack"), "{err}");
    let rows = memory.read_board("Задачи uwuwu").await.unwrap();
    assert_eq!(rows.len(), 1, "сноса нет");
}

#[tokio::test]
async fn task_clean_execute_deletes_draft_and_undo_restores() {
    let (tools, memory) = hub();
    call(&tools, "task_create", json!({ "title": "пустышка" }))
        .await
        .expect("draft создана");
    let out = call(&tools, "task_clean", json!({ "ids": "1", "confirm": true }))
        .await
        .expect("снос draft");
    assert!(out.contains("deleted 1 rows"), "{out}");
    assert!(out.contains("undo journal: uwuwu-"), "journal id: {out}");
    assert!(
        memory.read_board("Задачи uwuwu").await.unwrap().is_empty(),
        "строка покинула доску"
    );
    // undo без journal — свежий журнал проекта
    let out = call(&tools, "task_undo", json!({})).await.expect("undo");
    assert!(out.contains("restored"), "{out}");
    let rows = memory.read_board("Задачи uwuwu").await.unwrap();
    assert_eq!(rows.len(), 1, "строка вернулась");
    assert_eq!(rows[0].number(), 1, "номер тот же (item пересоздан)");
}

#[tokio::test]
async fn task_clean_execute_with_ack_takes_live_row() {
    let (tools, memory) = hub();
    call(
        &tools,
        "task_create",
        json!({ "title": "живая", "body": live_body("живая") }),
    )
    .await
    .expect("живая создана");
    // без ack — отказ; с ack — снос (журнал откатит)
    call(&tools, "task_clean", json!({ "ids": [1], "confirm": true }))
        .await
        .expect_err("без ack отказ");
    let out = call(
        &tools,
        "task_clean",
        json!({ "ids": [1], "confirm": true, "ack": [1] }),
    )
    .await
    .expect("ack владельца — снос");
    assert!(out.contains("deleted 1 rows"), "{out}");
    assert!(
        memory.read_board("Задачи uwuwu").await.unwrap().is_empty(),
        "live-строка снесена по ack"
    );
}

#[tokio::test]
async fn task_clean_garbage_ids_is_loud() {
    let (tools, _) = hub();
    let err = call(&tools, "task_clean", json!({ "ids": "1,два,3" }))
        .await
        .expect_err("мусорный токен — громкий отказ");
    assert!(err.contains("garbage token"), "{err}");
    // null в массиве тоже громкий (ревью #1158 Low-1: батч не сужается молча)
    let err = call(&tools, "task_clean", json!({ "ids": [1, null, 3] }))
        .await
        .expect_err("null-токен — громкий отказ");
    assert!(err.contains("garbage token"), "{err}");
    let err = call(&tools, "task_clean", json!({}))
        .await
        .expect_err("ids отсутствует");
    assert!(err.contains("ids is required"), "{err}");
}

// ——— goal_list (#1216): MCP-поверхность — склейка диспетчера с вьюхой ———
// Носитель в фикстуре — MemoryHub (целей нет): проверяется маршрутизация
// тула и подсказка носителя; живые данные — hub.rs::goals_live_tests
// (UWUWU_E2E=1).
#[tokio::test]
async fn goal_list_on_mcp_surface_carrier_hint_on_empty() {
    let (tools, _) = hub();
    let out = call(&tools, "goal_list", json!({}))
        .await
        .expect("дефолт-проект uwuwu читается без arguments");
    assert!(out.contains("no goals set for the project"), "{out}");
    assert!(out.contains("hub-repo milestones"), "{out}");
    assert!(
        out.contains("https://github.com/acme/hub/milestones"),
        "URL реестра из фикс.туры потерян: {out}"
    );
}

#[tokio::test]
async fn goal_list_on_mcp_surface_unknown_project_is_honest() {
    let (tools, _) = hub();
    let err = call(&tools, "goal_list", json!({ "project": "origa" }))
        .await
        .expect_err("проект вне конфига фикстуры — честный NotFound");
    assert!(err.contains("is not in the pipeline config"), "{err}");
}

// ——— Носитель типа — нативный issue Type (#1271): главный сценарий ———
// Сквозной круг на MCP-поверхности против MemoryHub: create(type) кладёт
// нативный тип в носитель, edit(type) меняет, чтения (task_get fm-шапка,
// task_list колонка) показывают канонное имя из носителя; мусорный type —
// громкий отказ до мутаций.
#[tokio::test]
async fn task_type_native_carrier_roundtrip_on_mcp_surface() {
    let (tools, memory) = hub();
    let n = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "носитель типа", "type": "bug", "body": "## П" }),
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

    // create(type=bug) → нативный Bug на агрегате носителя
    let rows = memory.read_board("Задачи uwuwu").await.unwrap();
    let row = rows.iter().find(|t| t.number() == number).unwrap();
    assert_eq!(row.kind(), Some(uwuwu_domain::TaskType::Bug));

    // edit(type=docs) → тип заменён в носителе
    call(
        &tools,
        "task_edit",
        json!({ "number": number, "project": "uwuwu", "type": "docs" }),
    )
    .await
    .expect("edit type ок");
    let rows = memory.read_board("Задачи uwuwu").await.unwrap();
    let row = rows.iter().find(|t| t.number() == number).unwrap();
    assert_eq!(row.kind(), Some(uwuwu_domain::TaskType::Docs));

    // task_get: fm-шапка несёт канонное имя из носителя
    let get = call(
        &tools,
        "task_get",
        json!({ "number": number, "project": "uwuwu" }),
    )
    .await
    .expect("task_get ок");
    assert!(get.contains("type: docs"), "fm-шапка с типом: {get}");

    // task_list: тип канонным именем из носителя (#1604 — JSON rows)
    let list = call(&tools, "task_list", json!({ "project": "uwuwu" }))
        .await
        .expect("task_list ок");
    let list_rows: Value = serde_json::from_str(&list).expect("rows — JSON");
    let list_row = list_rows
        .as_array()
        .expect("массив rows")
        .iter()
        .find(|r| r["title"] == "носитель типа")
        .expect("строка в списке");
    assert_eq!(list_row["type"], json!("docs"), "тип в rows: {list_row}");

    // мусорный type в edit — громкий отказ, носитель не тронут
    let err = call(
        &tools,
        "task_edit",
        json!({ "number": number, "project": "uwuwu", "type": "blabla" }),
    )
    .await
    .expect_err("мусорный тип отвергнут");
    assert!(err.contains("unknown type"), "{err}");
    let rows = memory.read_board("Задачи uwuwu").await.unwrap();
    let row = rows.iter().find(|t| t.number() == number).unwrap();
    assert_eq!(row.kind(), Some(uwuwu_domain::TaskType::Docs));
}

/// create без type — дефолт Feature канона на носителе (#1271): fm-шапка
/// и колонка списка показывают feature; нативный дефолт Task и null-хвост
/// рендера пустого типа пинятся на уровне разбора (parse_page) и naming.
#[tokio::test]
async fn task_type_create_default_feature_renders_on_mcp_surface() {
    let (tools, memory) = hub();
    let n = call(
        &tools,
        "task_create",
        json!({ "project": "uwuwu", "title": "native-null строка", "body": "## П" }),
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
    let get = call(
        &tools,
        "task_get",
        json!({ "number": number, "project": "uwuwu" }),
    )
    .await
    .expect("task_get ок");
    assert!(get.contains("type: feature"), "дефолт feature: {get}");
    let list = call(&tools, "task_list", json!({ "project": "uwuwu" }))
        .await
        .expect("task_list ок");
    let list_rows: Value = serde_json::from_str(&list).expect("rows — JSON");
    let list_row = list_rows
        .as_array()
        .expect("массив rows")
        .iter()
        .find(|r| r["title"] == "native-null строка")
        .expect("строка в списке");
    assert_eq!(
        list_row["type"],
        json!("feature"),
        "дефолт типа в rows (#1604): {list_row}"
    );
    let rows = memory.read_board("Задачи uwuwu").await.unwrap();
    let row = rows.iter().find(|t| t.number() == number).unwrap();
    assert_eq!(row.kind(), Some(uwuwu_domain::TaskType::Feature));
}

#[tokio::test]
async fn cron_list_renders_machine_accounting() {
    // Рендер-путь cron_list (#1480): расписание + машинный учёт из state
    // (last fire JST-штампом, счётчик отказов); пустое состояние — «never».
    let wiki = Arc::new(crate::mockhub::MemoryHub::from_env());
    wiki.write_doc(
        "cron/тест/README",
        "---\ntitle: Тест\nschedule: 5m\nproject: uwuwu\n---\nТело",
        "test master",
    )
    .await
    .unwrap();
    let seeded = StubCronState::default().with_row(
        "тест",
        uwuwu_usecase::CronRow {
            last_fire_ms: Some(((20771u64 * 86400) + 17 * 3600 + 29 * 60) * 1000),
            anchor_ms: None,
            deferred_since_ms: None,
            run: None,
            fails: 2,
        },
    );
    let (tools, _memory) = tool_hub(wiki.clone(), blob_usecase(), Arc::new(seeded));
    let out = call(&tools, "cron_list", json!({})).await.unwrap();
    assert!(out.contains("Тест [uwuwu]"), "{out}");
    assert!(out.contains("`every 5 min`"), "{out}");
    assert!(out.contains("last fire 2026-11-15T02:29:00+09:00"), "{out}");
    assert!(out.contains("fails 2"), "{out}");
    // Пустое состояние: «never», отказов нет
    let (tools, _memory) = tool_hub(wiki, blob_usecase(), Arc::new(StubCronState::default()));
    let out = call(&tools, "cron_list", json!({})).await.unwrap();
    assert!(out.contains("last fire never"), "{out}");
    assert!(out.contains("fails 0"), "{out}");
}

// ==== #1572: гард пустой записи и чужих параметров на write-path тел ====

/// Тело с крупной секцией в ещё более крупном теле — дыра гарда сжатия
/// (#1572 срез 3): доля сжатия тела мала, ошибочный короткий контент
/// стирает большую секцию мимо телесного гарда (класс #1277).
fn body_with_fat_tail_section() -> String {
    let mut tail = String::new();
    for i in 0..25 {
        tail.push_str(&format!(
            "строка отчёта хвоста {i}: содержимое, которое теряется\n"
        ));
    }
    let mut filler = String::new();
    for i in 0..300 {
        filler.push_str(&format!(
            "строка наполнителя тела {i}: большая секция в ещё большем теле\n"
        ));
    }
    format!("## Постановка\n\nДержатель массы тела.\n\n## Хвост\n\n{tail}\n## Прочее\n\n{filler}")
}

/// Сценарий 1 (#1572, BDD): task_body без body — громкий отказ с длиной
/// текущего тела и рецептом следующего хода; носитель цел. Босый
/// «empty body» (гард #406) дважды прочитан знающим агентом как успех
/// (инстансы foilcap #31, 22–23.11) — отказ обязан сам объяснять ход.
#[tokio::test]
async fn task_body_without_body_refuses_with_carrier_size_and_recipe() {
    let (tools, memory) = hub();
    let (number, _) = create_task(&tools, "t: no-body", &big_body()).await;
    let err = call(&tools, "task_body", json!({ "number": number }))
        .await
        .expect_err("запись без body отказывает громко");
    assert!(
        err.contains("REPLACES"),
        "отказ называет природу вызова (запись, не чтение): {err}"
    );
    assert!(err.contains("task_get"), "рецепт чтения в отказе: {err}");
    assert!(
        err.contains("body: \"\""),
        "рецепт осознанной очистки в отказе: {err}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(
        err.contains(&format!("{} bytes", stored.len())),
        "длина текущего тела в отказе — агент сверяет, что ничего не стёрто: {err}"
    );
    assert!(
        stored.contains("строка наполнителя 119"),
        "носитель цел: {stored}"
    );
}

/// Сценарий 1-бис (ревью #1572 Common-1): вход из одного fm-блока непуст
/// как строка, но в носитель уходит нормализованное — strip срезает fm в
/// пустоту, и запись без эффективного контента стирала носитель под
/// квитанцию «replaced; 0 bytes» (тот же класс, прецедент копирования fm
/// в тело #1151/#1331). Гард обязан судить по эффективному контенту.
#[tokio::test]
async fn task_body_with_fm_only_body_refuses_without_write() {
    let (tools, memory) = hub();
    let (number, _) = create_task(&tools, "t: fm-only", &big_body()).await;
    for fm_only in [
        "---\nbranch: work/task-1\n---\n",
        "---\ntitle: x\nstatus: Done\n---\n",
        "--- fm ---\nbranch: work/task-1\npr: —\nautomerge: true\ndirect: false\nfact_track: false\ntype: —\nbody_hash: abc\n",
    ] {
        let err = call(
            &tools,
            "task_body",
            json!({ "number": number, "body": fm_only }),
        )
        .await
        .expect_err("запись без эффективного контента отказывает");
        assert!(
            err.contains("effective content"),
            "отказ называет отсутствие эффективного контента: {err}"
        );
        assert!(
            err.contains("body: \"\""),
            "рецепт осознанной очистки в отказе: {err}"
        );
    }
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(
        stored.contains("строка наполнителя 119"),
        "носитель цел — fm-only вход не стирает тело: {stored}"
    );
}

/// Сценарий 1, развилка А (#1572): явный `body: ""` — осознанная очистка
/// без confirm-параметра: пустая строка — уже явный акт, стоп-кран поверх
/// явного действия не заводится (гран ADR-037). Транспорт доносит пустую
/// строку как присутствующее значение (живая проба 24.11: task_clean
/// ack: "" → «garbage token „"“»).
#[tokio::test]
async fn task_body_with_explicit_empty_string_clears_carrier() {
    let (tools, memory) = hub();
    let (number, _) = create_task(&tools, "t: clear", "## Постановка\n\nЧерновик.\n").await;
    let receipt = call(&tools, "task_body", json!({ "number": number, "body": "" }))
        .await
        .expect("явная пустая строка — осознанная очистка");
    assert!(
        receipt.contains("cleared"),
        "квитанция очистки называет акт: {receipt}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(stored.trim().is_empty(), "носитель очищен явно: {stored:?}");
}

/// Сценарий 2 (#1572, инстансы 23–24.11 дословно): task_body_section с
/// контентом в чужих параметрах find/replace (в схеме их нет) — громкий
/// отказ с перечнем чужих ключей; секция цела. Факт-механика: чужие ключи
/// молча strip'ились, вызов деградировал до очистки секции (квитанции
/// «cleared», каждый обход — полная перезапись тела).
#[tokio::test]
async fn task_body_section_with_foreign_find_replace_arguments_refuses_loud() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: find-replace", &big_body()).await;
    let err = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "Постановка",
            "find": "Наблюдение",
            "replace": "Новое наблюдение",
            "base_hash": hash,
        }),
    )
    .await
    .expect_err("чужие параметры — громкий отказ до мутации");
    assert!(err.contains("find"), "чужой ключ назван: {err}");
    assert!(err.contains("replace"), "чужой ключ назван: {err}");
    assert!(
        err.contains("content"),
        "отказ раскрывает известные ключи (контент не передан): {err}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(
        stored.contains("Наблюдение: тело задачи"),
        "секция цела — деградации до очистки нет: {stored}"
    );
    assert!(!stored.contains("Новое наблюдение"), "замены нет: {stored}");
}

/// Сценарий 2, обобщение (#1572): чужой ключ на мутационном туле —
/// громкий отказ до мутации, не тихий strip (task_body с лишним «text»).
#[tokio::test]
async fn unknown_argument_on_task_body_refuses_loud() {
    let (tools, memory) = hub();
    let (number, _) = create_task(&tools, "t: stray", "## А\nтело\n").await;
    let err = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": "## А\nновое тело\n", "text": "лишний" }),
    )
    .await
    .expect_err("чужой ключ — громкий отказ");
    assert!(err.contains("text"), "чужой ключ назван: {err}");
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(
        stored.contains("тело") && !stored.contains("новое тело"),
        "записи нет: {stored:?}"
    );
}

/// Легально потребляемый лишний ключ жив: legacy autodone на task_create
/// («the legacy autodone field is read» — канон) не отвергается чужим-гардом.
#[tokio::test]
async fn legacy_autodone_argument_passes_unknown_guard() {
    let (tools, _) = hub();
    let receipt = call(
        &tools,
        "task_create",
        json!({ "title": "легаси autodone жив", "body": "б", "autodone": true }),
    )
    .await
    .expect("legacy-ключ проходит чужой-гард");
    assert!(receipt.contains("created"), "{receipt}");
}

/// Сценарий 3 (#1572): task_body_section без content — громкий отказ с
/// длиной текущей секции и рецептом; секция цела (тихая деградация до
/// очистки закрыта). «Параметр отсутствует» ≠ «явно пустая строка».
#[tokio::test]
async fn task_body_section_without_content_refuses_with_section_size_and_recipe() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: no-content", &big_body()).await;
    let err = call(
        &tools,
        "task_body_section",
        json!({ "number": number, "section": "План", "base_hash": hash }),
    )
    .await
    .expect_err("запись без content отказывает громко");
    assert!(
        err.contains("REPLACES"),
        "отказ называет природу вызова (запись, не чтение): {err}"
    );
    assert!(err.contains("task_get"), "рецепт чтения в отказе: {err}");
    assert!(
        err.contains("content: \"\""),
        "рецепт осознанной очистки в отказе: {err}"
    );
    assert!(
        err.contains("bytes"),
        "длина текущей секции в отказе: {err}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(
        stored.contains("Контракт секционного пути"),
        "секция «План» цела — деградации до очистки нет: {stored}"
    );
}

/// Развилка А на секциях (#1572, пин сохранённой семантики): явный
/// `content: ""` — осознанная очистка секции; гард сжатия явную очистку
/// не трогает — рецепт отказа полного task_body на неё опирается.
#[tokio::test]
async fn explicit_empty_content_still_clears_existing_section() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: clear-sec", "## А\nтело\n").await;
    let receipt = call(
        &tools,
        "task_body_section",
        json!({ "number": number, "section": "А", "content": "", "base_hash": hash }),
    )
    .await
    .expect("явная очистка проходит");
    assert!(receipt.contains("cleared"), "квитанция: {receipt}");
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(
        stored.contains("## А") && !stored.contains("тело"),
        "контент секции снят, заголовок жив: {stored:?}"
    );
}

/// Сценарий 4 (#1572, класс #1277): ошибочный короткий контент на большой
/// секции — громкий отказ с длинами было/стало по телу и секции; носитель
/// цел (срез 3: секция в крупном теле — мимо телесной доли сжатия).
#[tokio::test]
async fn radical_section_shrink_refuses_with_lengths_without_write() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: shrink", &body_with_fat_tail_section()).await;
    let err = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "Хвост",
            "content": "короткий хвост\n",
            "base_hash": hash,
        }),
    )
    .await
    .expect_err("радикальное сжатие секции — громкий отказ");
    assert!(err.contains("radical shrink"), "природа отказа: {err}");
    assert!(
        err.contains("a 1 KiB+ section was cut by 70%+"),
        "секционное плечо называет своё правило (тело не срезано за треть): {err}"
    );
    assert!(
        err.contains("→"),
        "длины было→стало по телу и секции в отказе: {err}"
    );
    assert!(err.contains("bytes"), "длины в байтах: {err}");
    assert!(
        err.contains("confirm: true"),
        "рецепт осознанного преодоления: {err}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(
        stored.contains("строка отчёта хвоста 24"),
        "секция цела — записи нет: {stored}"
    );
}

/// Осознанное сжатие: confirm: true проводит запись; квитанция несёт
/// было/стало по телу и по секции + отметку confirm (контракт #1409).
#[tokio::test]
async fn radical_shrink_with_confirm_writes_and_receipt_carries_lengths() {
    let (tools, memory) = hub();
    let (number, hash) = create_task(&tools, "t: confirm", &body_with_fat_tail_section()).await;
    // первый вызов без confirm — отказ (гард), записи нет
    call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "Хвост",
            "content": "короткий хвост\n",
            "base_hash": hash,
        }),
    )
    .await
    .expect_err("гард отказывает без confirm");
    // повтор с confirm той же свежести — запись проходит
    let receipt = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "Хвост",
            "content": "короткий хвост\n",
            "base_hash": hash,
            "confirm": true,
        }),
    )
    .await
    .expect("confirm-запись проходит");
    assert!(
        receipt.contains("confirm"),
        "квитанция помечает confirm: {receipt}"
    );
    assert!(
        receipt.contains("(was"),
        "было/стало по телу и секции: {receipt}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(
        stored.contains("короткий хвост"),
        "контент записан: {stored}"
    );
}

/// Приёмка пресайн-режима на проводе (#1537): тот же вызов, что делает
/// агент (JSON-RPC `tools/call` через MCP-роутер), затем заливка байтов
/// `reqwest::put` по URL из квитанции и чтение по GET-пресайну из неё же.
/// Носитель — локальный HTTP-фейк: тест воспроизводим изолированно, не
/// опирается на прод-бакет и не закрепляет протухшие пресайны.
#[tokio::test]
async fn blob_put_presign_mode_e2e_over_mcp_wire_with_local_http_store() {
    use super::test_support::{call_tool_wire, BlobHttpFake, BlobObjects};
    use crate::api::McpState;
    use axum::http::StatusCode;

    /// Пейлоад загрузки: та же строка служит иглой для оракула «байты не
    /// едут в контекст» и телом PUT — иначе ассерт не может упасть.
    const AGENT_BYTES: &[u8] = b"E2E-1537-agent-bytes";

    struct TestHttpStore {
        fake: BlobHttpFake,
    }

    #[async_trait::async_trait]
    impl uwuwu_usecase::BlobStore for TestHttpStore {
        async fn put(
            &self,
            key: &uwuwu_domain::BlobKey,
            content_type: &str,
            bytes: &[u8],
        ) -> uwuwu_usecase::UsecaseResult<()> {
            self.fake.objects.lock().expect("lock").insert(
                key.as_str().to_string(),
                (bytes.to_vec(), content_type.to_string()),
            );
            Ok(())
        }
        fn presign_get_url(&self, key: &uwuwu_domain::BlobKey) -> String {
            self.fake.agent_get_url(key.as_str())
        }
        fn presign_put_url(&self, key: &uwuwu_domain::BlobKey, _content_type: &str) -> String {
            self.fake.agent_put_url(key.as_str())
        }
    }

    /// Общий `tools/call` по проводу; эта приёмка ждёт успешный текст
    /// квитанции, а не голый JSON-RPC.
    async fn wire_call(app: &axum::Router, name: &str, args: Value) -> String {
        let (status, msg) = call_tool_wire(app, name, args, "wire-bearer").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(msg["result"]["isError"], json!(false), "{msg}");
        msg["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    let fake = BlobHttpFake::start().await;
    let objects: BlobObjects = fake.objects.clone();
    let store = Arc::new(TestHttpStore { fake });
    let (tools, _memory) = tool_hub(
        Arc::new(StubWiki),
        Arc::new(uwuwu_usecase::BlobUseCase::ready(store)),
        Arc::new(StubCronState::default()),
    );
    let mcp = crate::api::mcp_router(Arc::new(McpState {
        bearer: Arc::new("wire-bearer".to_string()),
        tools,
    }));

    // Шаг 1 агента: резервирование без байтов — квитанция несёт только
    // ключ и две ссылки (никакого payload в dehydrated-ответе).
    let receipt = wire_call(
        &mcp,
        "blob_put",
        json!({ "filename": "скрин 1537.png", "number": 1537 }),
    )
    .await;
    let put_line = receipt
        .lines()
        .find(|l| l.starts_with("PUT URL "))
        .expect("PUT URL в квитанции");
    let put_url = put_line.rsplit(": ").next().expect("put url").to_string();
    let content_type = put_line
        .split("send Content-Type: ")
        .nth(1)
        .expect("content-type в подписи")
        .split("):")
        .next()
        .expect("content-type value")
        .to_string();
    let get_url = receipt
        .lines()
        .find(|l| l.starts_with("URL (TTL 7 days): "))
        .expect("GET URL в квитанции")
        .rsplit(": ")
        .next()
        .expect("get url")
        .to_string();
    let key = receipt
        .lines()
        .find_map(|l| l.strip_prefix("key: "))
        .expect("key в квитанции")
        .to_string();
    assert!(key.starts_with("blobs/uwuwu/1537/"), "{receipt}");
    assert!(
        put_url.contains(&key),
        "PUT подписан на тот же ключ: {receipt}"
    );
    assert!(
        get_url.contains(&key),
        "GET подписан на тот же ключ: {receipt}"
    );
    assert_eq!(content_type, "image/png");
    // Оракул НФТ: в dehydrated-ответе — ключ и ссылки, не байты. Потолок
    // размера ловит даже скрытый base64-пакет (мутация +450 КБ → красный),
    // а игла — дословный пейлоад загрузки, а не похожая фраза.
    assert!(
        receipt.len() < 4096,
        "квитанция несёт только ключ и URL, а не байты: {} байт",
        receipt.len()
    );
    assert!(
        !receipt.contains(std::str::from_utf8(AGENT_BYTES).expect("utf8 payload")),
        "байты загрузки едут в квитанции"
    );

    // Шаг 2: байты идут из shell мимо контекста — здесь HTTP-клиент на URL
    // из квитанции с Content-Type из неё же. Саму подпись Content-Type
    // держат пин адаптера в s3.rs и живой прогон Tigris; этот тест проверяет,
    // что тип из квитанции доходит до носителя.
    let status = reqwest::Client::new()
        .put(&put_url)
        .header("content-type", &content_type)
        .body(AGENT_BYTES.to_vec())
        .send()
        .await
        .expect("PUT by presigned URL")
        .status();
    assert_eq!(status, StatusCode::OK, "заливка через пресайн");
    assert!(
        objects
            .lock()
            .expect("lock")
            .get(&key)
            .is_some_and(|(b, ct)| b.as_slice() == AGENT_BYTES && ct == &content_type),
        "байты и тип из квитанции дошли до носителя"
    );

    // Шаг 3: GET-пресайн из той же квитанции открывается и возвращает
    // залитые байты; blob_get по ключу переподписывает GET и отдаёт их же.
    let from_receipt = reqwest::get(&get_url)
        .await
        .expect("GET by presigned URL")
        .error_for_status()
        .expect("пресайн GET из квитанции")
        .bytes()
        .await
        .expect("body");
    assert_eq!(
        from_receipt.as_ref(),
        AGENT_BYTES,
        "пресайн GET из квитанции вернул залитые байты"
    );
    let resign = wire_call(&mcp, "blob_get", json!({ "key": key })).await;
    let resign_url = resign
        .lines()
        .next()
        .expect("url line")
        .rsplit(": ")
        .next()
        .expect("resigned url")
        .to_string();
    let got = reqwest::get(&resign_url)
        .await
        .expect("GET by resigned URL")
        .bytes()
        .await
        .expect("body");
    assert_eq!(got.as_ref(), AGENT_BYTES, "blob_get вернул залитые байты");
}

/// Строка в Needs attention для проб #1619: вопрос ушёл из Design.
async fn needs_attention_task(tools: &ToolHub) -> u64 {
    needs_attention_named(tools, "ответ через чат").await
}

/// Та же фикстура с явным тайтлом: в одном тесте строк несколько, а
/// дубли тайтлов на доске запрещены.
async fn needs_attention_named(tools: &ToolHub, title: &str) -> u64 {
    let created = call(tools, "task_create", json!({ "title": title, "body": "б" }))
        .await
        .expect("create ок");
    let n = receipt_number(&created);
    call(
        tools,
        "task_edit",
        json!({ "number": n, "priority": "medium", "automerge": true }),
    )
    .await
    .expect("поля разбора заполнены");
    for status in ["In Progress", "Needs attention"] {
        call(tools, "task_edit", json!({ "number": n, "status": status }))
            .await
            .expect("ход к Needs attention легален");
    }
    n
}

/// #1619, красный тест: агент держит дословный ответ владельца и не может
/// вернуть строку в стадию-источник — гард отказывает ребро
/// Needs attention → Design (инцидент #1607).
#[tokio::test]
async fn task_edit_owner_answer_reopens_needs_attention() {
    let (tools, _memory) = hub();
    let n = needs_attention_task(&tools).await;
    let receipt = call(
        &tools,
        "task_edit",
        json!({
            "number": n,
            "status": "Design",
            "owner_answer": "владелец: делай так, как в чате",
        }),
    )
    .await
    .expect("ответ владельца открывает возврат в стадию-источник");
    assert!(
        receipt.contains("Design"),
        "квитанция подтверждает возврат: {receipt}"
    );
}

/// Без факта ответа снятие Needs attention остаётся запрещено; отказ
/// называет ключ — самообучающий текст доходит до живого агента (#1619).
#[tokio::test]
async fn task_edit_needs_attention_without_answer_still_refused() {
    let (tools, _memory) = hub();
    let n = needs_attention_task(&tools).await;
    for answer in [None, Some("   ")] {
        let mut args = json!({ "number": n, "status": "Design" });
        if let Some(text) = answer {
            args["owner_answer"] = json!(text);
        }
        let err = call(&tools, "task_edit", args)
            .await
            .expect_err("нет ответа — отказ");
        assert!(err.contains("owner_answer"), "отказ учит ключу: {err}");
    }
}

/// Ключ без смены статуса — не мутация и не generic «empty patch» (#1619).
#[tokio::test]
async fn task_edit_owner_answer_requires_status() {
    let (tools, _memory) = hub();
    let n = needs_attention_task(&tools).await;
    let err = call(
        &tools,
        "task_edit",
        json!({ "number": n, "owner_answer": "владелец: ок" }),
    )
    .await
    .expect_err("ключ без статуса — громкий отказ");
    assert!(
        err.contains("requires a status transition"),
        "отказ объясняет причину: {err}"
    );
}

/// Повтор после успешной записи — идемпотентная квитанция, а не ложный
/// «переход запрещён»; ключ на обычном ходе вне NA — с припиской (#1619/#1437).
#[tokio::test]
async fn task_edit_owner_answer_retry_outside_needs_attention_is_noted() {
    let (tools, _memory) = hub();
    let n = needs_attention_task(&tools).await;
    call(
        &tools,
        "task_edit",
        json!({ "number": n, "status": "Design", "owner_answer": "владелец: ок" }),
    )
    .await
    .expect("первый вызов принят");
    // current == target: идемпотентная переустановка с тем же ключом.
    let retry = call(
        &tools,
        "task_edit",
        json!({ "number": n, "status": "Design", "owner_answer": "владелец: ок" }),
    )
    .await
    .expect("повтор после успеха не лжёт");
    assert!(
        retry.contains("carrier confirms"),
        "повтор не лжёт о статусе: {retry}"
    );
    // Обычный ход на строке, которая никогда не была в NA, — канон не
    // меняется, ключ помечен как проигнорированный.
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "обычный ход с ключом", "body": "б" }),
    )
    .await
    .expect("create ок");
    let other = receipt_number(&created);
    call(
        &tools,
        "task_edit",
        json!({ "number": other, "priority": "medium", "automerge": true }),
    )
    .await
    .expect("поля разбора заполнены");
    let next = call(
        &tools,
        "task_edit",
        json!({ "number": other, "status": "In Progress", "owner_answer": "владелец: ок" }),
    )
    .await
    .expect("fast-track легален");
    assert!(
        next.contains("owner_answer ignored"),
        "приписка вне NA: {next}"
    );
}

/// #1619 (тестер): ответ владельца не открывает произвольные стадии —
/// белый список источников вопроса остаётся гейтом, носитель не тронут.
#[tokio::test]
async fn task_edit_owner_answer_keeps_whitelist_closed() {
    let (tools, memory) = hub();
    let n = needs_attention_task(&tools).await;
    for target in ["Done", "Todo"] {
        let err = call(
            &tools,
            "task_edit",
            json!({
                "number": n,
                "status": target,
                "owner_answer": "владелец: ответил, но не про это",
            }),
        )
        .await
        .expect_err("ответ не открывает стадию вне списка");
        assert!(
            err.contains("outside the pipeline canon"),
            "отказ по канону, а не по ключу: {err}"
        );
    }
    let card = call(&tools, "task_get", json!({ "number": n }))
        .await
        .expect("card после отказов");
    assert!(
        card.lines()
            .next()
            .is_some_and(|l| l.ends_with("Needs attention")),
        "отказ не сдвинул строку: {card}"
    );
    let board = memory.read_board("Задачи uwuwu").await.expect("борд");
    let row = board
        .iter()
        .find(|t| t.number() == n)
        .expect("строка в борде");
    assert_eq!(
        row.status(),
        uwuwu_domain::Status::NeedsAttention,
        "носитель не тронут отказом"
    );
}

/// #1619 (тестер): ключ не сужает прежние рёбра и возврат реально
/// приземляется в носителе — квитанция «carrier confirms» обязана
/// подтверждаться task_get, а не только событием гарда.
#[tokio::test]
async fn task_edit_owner_answer_lands_on_carrier() {
    let (tools, memory) = hub();
    let n = needs_attention_task(&tools).await;
    let receipt = call(
        &tools,
        "task_edit",
        json!({
            "number": n,
            "status": "Design",
            "owner_answer": "владелец: возвращайся в Design",
        }),
    )
    .await
    .expect("ответ открывает возврат");
    assert!(receipt.contains("carrier confirms"), "{receipt}");
    let card = call(&tools, "task_get", json!({ "number": n }))
        .await
        .expect("card после записи");
    assert!(
        card.lines().next().is_some_and(|l| l.ends_with("Design")),
        "task_get видит записанный возврат: {card}"
    );
    let board = memory.read_board("Задачи uwuwu").await.expect("борд");
    let row = board
        .iter()
        .find(|t| t.number() == n)
        .expect("строка в борде");
    assert_eq!(
        row.status(),
        uwuwu_domain::Status::Design,
        "носитель зафиксировал возврат"
    );
    // Прежнее ребро NA → InProgress работает и без ответа — новое ребро
    // не заменило старое.
    let other = needs_attention_named(&tools, "прежнее ребро без ключа").await;
    call(
        &tools,
        "task_edit",
        json!({ "number": other, "status": "In Progress" }),
    )
    .await
    .expect("прежнее ребро без ключа");
}

/// #1648: копия живого вывода task_get с хвостом комментов не несёт ленту
/// на носитель. Оракул — body_bytes из того же вывода: носитель байт-в-байт
/// равен телу, а не композитному рендеру.
#[tokio::test]
async fn task_body_strips_comments_tail_of_the_real_render() {
    let (tools, memory) = hub();
    let body = "## Постановка\n\nчистое тело карточки.\n";
    let (number, _) = create_task(&tools, "t: хвост в task_body", body).await;
    for i in 0..2 {
        call(
            &tools,
            "task_comment",
            json!({ "number": number, "text": format!("коммент {i}") }),
        )
        .await
        .expect("comment");
    }
    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    assert!(card.contains("--- comments (latest 2 of 2) ---"), "{card}");
    // create добавляет хвостовой перевод строки: оракул — срез тела из
    // рендера, а не строка, переданная в create.
    let body_at = card.find(body).expect("body in card");
    let body_end = card
        .find("\n\n--- comments (latest")
        .expect("tail boundary");
    let rendered_body = &card[body_at..body_end];
    let copied = &card[card.find("--- fm ---").expect("fm")..];

    let receipt = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": copied }),
    )
    .await
    .expect("task_body ok");
    assert!(receipt.contains("replaced"), "квитанция: {receipt}");

    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert_eq!(stored, rendered_body, "тело без ленты: {stored}");
    let declared_at = card.find("body_bytes: ").expect("fm") + "body_bytes: ".len();
    let declared_end = declared_at + card[declared_at..].find('\n').expect("end");
    let declared: usize = card[declared_at..declared_end].parse().expect("integer");
    assert_eq!(stored.len(), declared, "размер = body_bytes из копии");
}

/// #1648: та же копия в task_create рождает строку без ленты комментов.
#[tokio::test]
async fn task_create_strips_comments_tail_of_the_real_render() {
    let (tools, memory) = hub();
    let body = "## Постановка\n\nтело для копии.\n";
    let (number, _) = create_task(&tools, "t: источник для create", body).await;
    call(
        &tools,
        "task_comment",
        json!({ "number": number, "text": "коммент источника" }),
    )
    .await
    .expect("comment");
    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let copied = &card[card.find("--- fm ---").expect("fm")..];

    let receipt = call(
        &tools,
        "task_create",
        json!({ "title": "t: копия с лентой", "body": copied }),
    )
    .await
    .expect("create ok");
    let created = receipt_number(&receipt);
    let stored = memory
        .read_issue("Задачи uwuwu", created)
        .await
        .expect("read")
        .body;
    assert!(
        !stored.contains("--- comments (latest"),
        "ленты нет: {stored}"
    );
    assert!(
        !stored.contains("коммент источника"),
        "коммента нет: {stored}"
    );
    assert!(stored.contains("тело для копии"), "тело на месте: {stored}");
}

async fn hash_of(tools: &ToolHub, number: u64) -> String {
    let card = call(tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    fm_hash(&card)
}

fn dumped_body(clean: &str) -> String {
    format!(
        "{clean}\n\n--- comments (latest 1 of 1) ---\n---\n2026-11-24T04:59:18Z\nстарый коммент\n"
    )
}

/// #1648: дамп засевается прямо в носитель (create его уже срезал бы).
/// Секционная правка грязного носителя даёт байт-в-байт то же тело, что
/// правка чистого: оракул — контрольная строка чистого носителя.
#[tokio::test]
async fn task_body_section_on_carrier_with_dump_matches_clean_carrier() {
    let (tools, memory) = hub();
    let seed = "## Постановка\n\nтело до дампа.\n";
    let (clean_no, _) = create_task(&tools, "t: чистое плечо", seed).await;
    let (dirty_no, _) = create_task(&tools, "t: грязное плечо", seed).await;
    let clean_body = memory
        .read_issue("Задачи uwuwu", clean_no)
        .await
        .expect("read")
        .body;
    let polluted = dumped_body(&clean_body);
    memory
        .replace_body(dirty_no, &polluted)
        .await
        .expect("seed dump");
    let seeded = memory
        .read_issue("Задачи uwuwu", dirty_no)
        .await
        .expect("read")
        .body;
    assert_eq!(seeded, polluted, "дамп реально лежит до правки");

    for (number, hash) in [
        (clean_no, hash_of(&tools, clean_no).await),
        (dirty_no, hash_of(&tools, dirty_no).await),
    ] {
        let receipt = call(
            &tools,
            "task_body_section",
            json!({
                "number": number,
                "section": "План",
                "content": "новая секция",
                "base_hash": hash,
            }),
        )
        .await
        .expect("section edit");
        assert!(receipt.contains("appended"), "квитанция: {receipt}");
    }

    let clean_stored = memory
        .read_issue("Задачи uwuwu", clean_no)
        .await
        .expect("read")
        .body;
    let dirty_stored = memory
        .read_issue("Задачи uwuwu", dirty_no)
        .await
        .expect("read")
        .body;
    assert_eq!(
        dirty_stored, clean_stored,
        "грязное против чистого плеча: {dirty_stored}"
    );
    assert!(!dirty_stored.contains("старый коммент"), "дамп снят");
    assert!(!dirty_stored.contains("--- comments"), "маркер снят");
}

/// #1648: дамп вне редактируемой секции снимается молча — гард сжатия
/// судит по собранному телу до среза и не требует `confirm: true`.
#[tokio::test]
async fn task_body_section_silent_when_dump_is_outside_edited_section() {
    let (tools, memory) = hub();
    let (number, _) = create_task(
        &tools,
        "t: дамп в другой секции",
        "## Постановка\n\nтело\n\n## Смежное\n\nссылка\n",
    )
    .await;
    let clean_body = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    memory
        .replace_body(number, &dumped_body(&clean_body))
        .await
        .expect("seed dump");
    let hash = hash_of(&tools, number).await;

    let receipt = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "Постановка",
            "content": "новое тело",
            "base_hash": hash,
        }),
    )
    .await
    .expect("section edit");
    assert!(receipt.contains("replaced"), "квитанция: {receipt}");
    assert!(!receipt.contains("confirm"), "гард молчит: {receipt}");
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(stored.contains("новое тело"), "секция правлена: {stored}");
    assert!(!stored.contains("--- comments"), "дамп снят: {stored}");
}

/// #1648: шапка + хвост без тела — отказ «no effective content», носитель цел.
#[tokio::test]
async fn task_body_refuses_header_and_tail_without_body() {
    let (tools, memory) = hub();
    let body = "## Постановка\n\nтело остаётся.\n";
    let (number, _) = create_task(&tools, "t: отказ по хвосту", body).await;
    call(
        &tools,
        "task_comment",
        json!({ "number": number, "text": "коммент" }),
    )
    .await
    .expect("comment");
    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let fm_start = card.find("--- fm ---").expect("fm");
    let body_start = card.find(body).expect("body in card");
    let header_and_tail = format!(
        "{}{}",
        &card[fm_start..body_start],
        "\n\n--- comments (latest 1 of 1) ---\n---\n2026-11-24T04:59:18Z\nкоммент\n"
    );
    let err = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": header_and_tail }),
    )
    .await
    .expect_err("отказ");
    assert!(err.contains("effective content"), "причина отказа: {err}");
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(stored.contains("тело остаётся"), "носитель цел: {stored}");
}

/// #1648: копия карточки в `content` секции не оставляет ленту на носителе.
#[tokio::test]
async fn task_body_section_strips_copied_card_in_content() {
    let (tools, memory) = hub();
    let (number, _) = create_task(
        &tools,
        "t: копия в content",
        "## Постановка\n\nтело.\n\n## План\n\nстарое.\n",
    )
    .await;
    call(
        &tools,
        "task_comment",
        json!({ "number": number, "text": "лента источника" }),
    )
    .await
    .expect("comment");
    let card = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let copied = &card[card.find("--- fm ---").expect("fm")..];
    let receipt = call(
        &tools,
        "task_body_section",
        json!({
            "number": number,
            "section": "План",
            "content": copied,
            "base_hash": hash_of(&tools, number).await,
        }),
    )
    .await
    .expect("section edit");
    assert!(receipt.contains("replaced"), "квитанция: {receipt}");
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert!(!stored.contains("--- comments"), "маркер не лёг: {stored}");
    assert!(
        !stored.contains("лента источника"),
        "лента не легла: {stored}"
    );

    // Плечо плана: лента в `content`, после неё дописан H2-хвост — он жив.
    let (tail_no, _) =
        create_task(&tools, "t: хвост после копии", "## Постановка\n\nтело.\n").await;
    let tail_card = call(&tools, "task_get", json!({ "number": tail_no }))
        .await
        .expect("get tail");
    let copied_with_tail = format!(
        "{}\n\n--- comments (latest 1 of 1) ---\n---\n2026-11-24T04:59:18Z\nлента источника\n\n## Хвост\n\nсохранённый хвост\n",
        &tail_card[tail_card.find("--- fm ---").expect("fm")..]
    );
    let receipt = call(
        &tools,
        "task_body_section",
        json!({
            "number": tail_no,
            "section": "Смежное",
            "content": copied_with_tail,
            "base_hash": hash_of(&tools, tail_no).await,
        }),
    )
    .await
    .expect("section edit with tail");
    assert!(receipt.contains("appended"), "квитанция: {receipt}");
    let stored_tail = memory
        .read_issue("Задачи uwuwu", tail_no)
        .await
        .expect("read tail")
        .body;
    assert!(
        stored_tail.contains("## Хвост\n\nсохранённый хвост"),
        "H2-хвост после копии цел: {stored_tail}"
    );
    assert!(
        !stored_tail.contains("--- comments"),
        "маркер не лёг: {stored_tail}"
    );
    assert!(
        !stored_tail.contains("лента источника"),
        "лента не легла: {stored_tail}"
    );
}

// ---- tester #1648: приёмочный прогон главного сценария на MCP-поверхности ----

fn tester_fm_value(card: &str, key: &str) -> String {
    let prefix = format!("{key}: ");
    card.lines()
        .find_map(|l| l.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("fm-ключ {key:?} отсутствует: {card}"))
        .trim()
        .to_string()
}

/// Полный путь живого пользователя: прочитать карточку с комментами →
/// скопировать вывод от `--- fm ---` → записать копию через `task_body` и
/// через `task_create`. Оракул пути `task_body` — `body_hash`/`body_bytes`
/// из той же копии: носитель несёт тело источника байт-в-байт. Оракул пути
/// `task_create` — строка, рождённая из чистого тела источника (create
/// добавляет перевод строки): точное сравнение двух носителей.
#[tokio::test]
async fn tester_copy_of_card_with_comments_is_clean_on_both_write_paths() {
    let (tools, memory) = hub();
    let src_body = "## Постановка\n\nтело для копии.\n";
    let (src, _) = create_task(&tools, "t: источник с лентой", src_body).await;
    call(
        &tools,
        "task_comment",
        json!({ "number": src, "text": "первый коммент источника" }),
    )
    .await
    .expect("comment 1");
    call(
        &tools,
        "task_comment",
        json!({ "number": src, "text": "второй коммент источника" }),
    )
    .await
    .expect("comment 2");

    let card = call(&tools, "task_get", json!({ "number": src }))
        .await
        .expect("get source");
    assert!(
        card.contains("--- comments (latest 2 of 2) ---"),
        "лента есть: {card}"
    );
    let oracle_hash = tester_fm_value(&card, "body_hash");
    let oracle_bytes: usize = tester_fm_value(&card, "body_bytes")
        .parse()
        .expect("body_bytes");
    let copied = &card[card.find("--- fm ---").expect("fm")..];

    // Путь 1: task_body на существующей строке.
    let (target, _) = create_task(&tools, "t: цель task_body", "## Постановка\n\nстарое.\n").await;
    let receipt = call(
        &tools,
        "task_body",
        json!({ "number": target, "body": copied }),
    )
    .await
    .expect("task_body");
    assert!(
        receipt.contains(&format!("; {oracle_bytes} bytes;")),
        "квитанция несёт точный размер тела источника: {receipt}"
    );
    assert!(
        receipt.contains(&format!("body_hash: {oracle_hash}")),
        "квитанция несёт точный хэш: {receipt}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", target)
        .await
        .expect("read target")
        .body;
    // task_create добавляет хвостовой перевод строки к телу источника —
    // оракул — body_bytes из прочитанной копии, а не строка, поданная в create.
    assert_eq!(stored.len(), oracle_bytes, "размер = oracle body_bytes");
    assert_eq!(
        stored,
        format!("{src_body}\n"),
        "носитель = тело источника байт-в-байт"
    );
    assert!(!stored.contains("--- comments"), "маркер не лёг: {stored}");
    assert!(
        !stored.contains("коммент источника"),
        "лента не легла: {stored}"
    );

    // Путь 2: та же копия в task_create рождает строку без ленты.
    // Оракул — строка, рождённая из ЧИСТОГО тела источника (create добавляет
    // свой перевод строки): сравнение точных байт, а не вхождений.
    let clean_source_body = memory
        .read_issue("Задачи uwuwu", src)
        .await
        .expect("read source")
        .body;
    let (control, _) = create_task(&tools, "t: контроль create", &clean_source_body).await;
    let control_stored = memory
        .read_issue("Задачи uwuwu", control)
        .await
        .expect("read control")
        .body;
    let receipt = call(
        &tools,
        "task_create",
        json!({ "title": "t: цель task_create", "body": copied }),
    )
    .await
    .expect("task_create");
    let created = receipt_number(&receipt);
    let stored_new = memory
        .read_issue("Задачи uwuwu", created)
        .await
        .expect("read created")
        .body;
    assert_eq!(
        stored_new, control_stored,
        "create из копии = create из чистого тела, байт-в-байт"
    );
    assert!(
        !stored_new.contains("--- comments"),
        "маркер не лёг: {stored_new}"
    );
    assert!(
        !stored_new.contains("коммент источника"),
        "лента не легла: {stored_new}"
    );
}

/// Регресс-пин «не перерезать»: чистое тело и копия без ленты проходят
/// границу байт-в-байт; срез не должен трогать текст без сигнатуры записи.
#[tokio::test]
async fn tester_clean_body_copy_is_not_overstripped() {
    let (tools, memory) = hub();
    let body = "## Постановка\n\nчистое тело.\n\n## План\n\nбез ленты.\n";
    let (number, _) = create_task(&tools, "t: чистая копия", body).await;
    let receipt = call(
        &tools,
        "task_body",
        json!({ "number": number, "body": body }),
    )
    .await
    .expect("task_body");
    assert!(
        receipt.contains(&format!("; {} bytes;", body.len())),
        "квитанция несёт точный размер неизменённого тела: {receipt}"
    );
    let stored = memory
        .read_issue("Задачи uwuwu", number)
        .await
        .expect("read")
        .body;
    assert_eq!(stored, body, "чистое тело не тронуто");
}

/// Дельтовый пин Low-фикса (#1537): `data_base64: null` на проводе —
/// резервирование (как отсутствие аргумента), не-строка — громкий отказ.
/// Дев запинает диспетчер; здесь тот же контракт проверяется через
/// JSON-RPC-роутер — тем вызовом, который делает агент.
#[tokio::test]
async fn blob_put_null_data_base64_reserves_over_wire_and_wrong_type_refuses() {
    use super::test_support::call_tool_wire;
    use crate::api::McpState;

    let (tools, _memory) = hub();
    let mcp = crate::api::mcp_router(Arc::new(McpState {
        bearer: Arc::new("wire-bearer".to_string()),
        tools,
    }));

    let (status, msg) = call_tool_wire(
        &mcp,
        "blob_put",
        json!({ "filename": "null.png", "number": 1537, "data_base64": null }),
        "wire-bearer",
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(msg["result"]["isError"], json!(false), "{msg}");
    let receipt = msg["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(receipt.starts_with("key: blobs/uwuwu/1537/"), "{receipt}");
    assert!(receipt.contains("PUT URL (TTL 300 s"), "{receipt}");
    assert!(receipt.contains("URL (TTL 7 days): memory://"), "{receipt}");

    let (_, wrong) = call_tool_wire(
        &mcp,
        "blob_put",
        json!({ "filename": "a.png", "number": 1537, "data_base64": 42 }),
        "wire-bearer",
    )
    .await;
    assert_eq!(wrong["result"]["isError"], json!(true), "{wrong}");
    assert!(
        wrong["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("data_base64 must be a base64 string"),
        "{wrong}"
    );
}
