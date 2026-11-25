//! Текстовые пины поверхности MCP (#1167, допокрытие тестировщика):
//! армы вне тест-покрытия tools_tests.rs (task_create/task_edit/task_list/
//! task_get/docs_*/search/session_export + ветка unknown диспетчера).
//! Рефакторинг #1167 переносил тела байт-в-байт; пины держат наблюдаемые
//! тексты отказов и ответов, чтобы дрейф вернуть нечем. Пробы через живой
//! диспетчер на MemoryHub — без GitHub, как в tools_tests.rs.

use super::test_support::{blob_usecase, tool_hub, StubCronState};
use super::tools::ToolHub;
use serde_json::{json, Value};
use std::sync::Arc;

/// ToolHub фабрики test_support: MemoryHub как вики (docs_* армы пинов
/// читают живой носитель) + memory-blob.
fn hub() -> Arc<ToolHub> {
    let (tools, _memory) = tool_hub(
        Arc::new(crate::mockhub::MemoryHub::from_env()),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    tools
}

/// Живой вызов диспетчера: Ok — текст контента, Err — текст отказа.
async fn call(tools: &ToolHub, name: &str, args: Value) -> Result<String, String> {
    let msg = json!({ "params": { "name": name, "arguments": args } });
    match tools.call(&msg).await {
        Ok(v) => Ok(v[0]["text"].as_str().unwrap_or_default().to_string()),
        Err(e) => Err(e),
    }
}

/// Баг-пин дрейфа ревью-раунда 1 (H-1): множественное «обязательны»
/// из require_args не должно подменять мастерский текст tuла search.
#[tokio::test]
async fn search_without_query_keeps_master_error_text() {
    let tools = hub();
    let err = call(&tools, "search", json!({})).await.expect_err("отказ");
    assert_eq!(err, "query is required");
}

#[tokio::test]
async fn docs_append_requires_slug_and_line() {
    let tools = hub();
    let err = call(&tools, "docs_append", json!({}))
        .await
        .expect_err("отказ");
    assert_eq!(err, "slug and line are required");
}

/// Гард append-семантики docs_append (#1353): дописывание сохраняет
/// тело существующего дока (fm + контент), строка — в конец; ответ
/// различает append и создание. Сид несёт fm явно (#1444: вход с fm
/// пишется дословно — append-пин фиксирует append-семантику на fm-доке).
#[tokio::test]
async fn docs_append_keeps_body_of_existing_doc() {
    let tools = hub();
    let slug = "Experience/devops/Опыт (srv, #1248)";
    let seeded = "---\ntitle: Опыт (srv, #1248)\ndate: 2026-11-19\n---\n\nстрока A\n";
    call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": seeded }),
    )
    .await
    .expect("write");
    let answer = call(
        &tools,
        "docs_append",
        json!({ "slug": slug, "line": "строка B" }),
    )
    .await
    .expect("append");
    assert!(
        answer.contains("appended to") && answer.contains("lines in the doc: 7"),
        "ответ append без факта операции: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    // append_to нормализует EOF: итог всегда с завершающим переносом
    assert_eq!(stored, format!("{seeded}строка B\n"));
}

#[tokio::test]
async fn docs_append_reports_creation_of_missing_doc() {
    let tools = hub();
    let answer = call(
        &tools,
        "docs_append",
        json!({ "slug": "Scratch/new#doc", "line": "первая строка" }),
    )
    .await
    .expect("append");
    assert!(
        answer.contains("created"),
        "создание не названо созданием: {answer}"
    );
}

#[tokio::test]
async fn docs_write_requires_slug_and_content() {
    let tools = hub();
    let err = call(&tools, "docs_write", json!({ "slug": "x/y" }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "slug and content are required");
}

// --- fm-гвард docs_write (#1444): шапка вики-дока переживает replace ---

/// Ветка Carried: шапка существующего дока переносится при правке без fm
/// во входе; date и прочие ключи целы, lastmod освежается, тело — из входа.
#[tokio::test]
async fn docs_write_carries_fm_header_of_existing_doc() {
    let tools = hub();
    let slug = "Experience/platform/ops-mechanics";
    let seeded = "---\ntitle: ops-mechanics\ndate: 2026-11-04T22:41:07+09:00\nlastmod: 2026-11-20T20:50:00+09:00\n---\n\n# ops-mechanics\n\nстарое тело\n";
    call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": seeded }),
    )
    .await
    .expect("seed: вход с fm пишется дословно");
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": "# ops-mechanics\n\nновое тело\n" }),
    )
    .await
    .expect("write");
    assert!(
        answer.contains("carried over"),
        "ветка переноса не названа в ответе: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    assert!(
        stored.starts_with("---\ntitle: ops-mechanics\n"),
        "шапка снесена: {stored}"
    );
    assert!(
        stored.contains("date: 2026-11-04T22:41:07+09:00"),
        "date потерян: {stored}"
    );
    assert!(
        !stored.contains("lastmod: 2026-11-20T20:50"),
        "lastmod не освежён моментом записи: {stored}"
    );
    assert!(
        stored.contains("lastmod: 2"),
        "lastmod отсутствует: {stored}"
    );
    assert!(
        stored.contains("новое тело") && !stored.contains("старое тело"),
        "тело не заменено: {stored}"
    );
}

/// Ветка Carried без lastmod в блоке (системный случай C-2: крон-мастера
/// и бот-доки): ключ дописывается последней строкой блока, операционные
/// ключи переносится байт-в-байт.
#[tokio::test]
async fn docs_write_appends_lastmod_to_fm_block_without_it() {
    let tools = hub();
    let slug = "cron/QA-ночной-аудит-кода/README";
    let seeded = "---\ntitle: QA: ночной аудит\nschedule: 30 1 * * *\nagent: tester\nmigrated-from: '#114'\nproject: uwuwu\n---\n\n## Инструкция\n\nтело\n";
    call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": seeded }),
    )
    .await
    .expect("seed");
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": "## Инструкция\n\nновое тело\n" }),
    )
    .await
    .expect("write");
    assert!(
        answer.contains("carried over"),
        "ветка переноса не названа в ответе: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    for key in [
        "title: QA: ночной аудит",
        "schedule: 30 1 * * *",
        "agent: tester",
        "migrated-from: '#114'",
        "project: uwuwu",
    ] {
        assert!(stored.contains(key), "ключ «{key}» потерян: {stored}");
    }
    assert!(
        stored.contains("project: uwuwu\nlastmod: 2"),
        "lastmod дописан не последней строкой блока: {stored}"
    );
}

/// Ветка Created: новый док без шапки получает каноническую; title с «: »
/// в значении сериализуется в двойных кавычках (C-3, канон ADR #1225).
#[tokio::test]
async fn docs_write_creates_canonical_header_with_quoted_title_on_colon() {
    let tools = hub();
    let slug = "Projects/uwuwu/ADR/ADR-test-guard: Заголовок решения";
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": "# ADR-test-guard\n\nтекст\n" }),
    )
    .await
    .expect("write");
    assert!(
        answer.contains("canonical"),
        "ветка создания не названа в ответе: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    assert!(
        stored.starts_with("---\ntitle: \"ADR-test-guard: Заголовок решения\"\n"),
        "каноническая шапка без кавычек в title с «: »: {stored}"
    );
    assert!(
        stored.contains("\ndate: 2") && stored.contains("\nlastmod: 2"),
        "date/lastmod отсутствуют: {stored}"
    );
}

/// Ветка Verbatim: вход с собственным fm пишется дословно, байт-в-байт.
#[tokio::test]
async fn docs_write_keeps_fm_from_payload_verbatim() {
    let tools = hub();
    let slug = "bot/Помощник/README";
    let payload = "---\ntitle: uwuwu-помощник\nenabled: true\nmodel: zai-coding-plan/glm-5.3-flash\ntoken_env: UWUWU_BOT_TOKEN_HELPER\nowner: yurvon_screamo\n---\n\nметодичка\n";
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": payload }),
    )
    .await
    .expect("write");
    assert!(
        answer.contains("verbatim"),
        "ветка дословной записи не названа в ответе: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    assert_eq!(stored, payload, "вход с fm искажён");
}

/// Сбой чтения существующего дока: вход без fm — громкий отказ (гварду
/// нечего переносить, молчаливая запись канонической шапки поверх
/// операционной недопустима); вход с fm — дословно, от чтения не зависит.
#[tokio::test]
async fn docs_write_read_failure_is_loud_without_fm_payload() {
    let (tools, _) = tool_hub(
        Arc::new(ReadFailingWiki),
        blob_usecase(),
        Arc::new(StubCronState::default()),
    );
    let err = call(
        &tools,
        "docs_write",
        json!({ "slug": "x/y", "content": "тело без шапки\n" }),
    )
    .await
    .expect_err("отказ");
    assert!(
        err.contains("fm-guard"),
        "отказ без вменяемого текста гварда: {err}"
    );
    let payload = "---\ntitle: x\n---\n\nтело с шапкой\n";
    let answer = call(
        &tools,
        "docs_write",
        json!({ "slug": "x/y", "content": payload }),
    )
    .await
    .expect("вход с fm не зависит от сбоя чтения");
    assert!(answer.contains("verbatim"), "ветка не названа: {answer}");
}

/// Вики-носитель с мёртвым чтением: write проходит, read рвётся транспортом.
struct ReadFailingWiki;
#[async_trait::async_trait]
impl uwuwu_usecase::WikiRepo for ReadFailingWiki {
    async fn doc_shas(&self) -> uwuwu_usecase::UsecaseResult<Vec<(String, String)>> {
        Ok(vec![])
    }
    async fn read_doc(&self, slug: &str) -> uwuwu_usecase::UsecaseResult<Option<String>> {
        Err(uwuwu_usecase::UsecaseError::Board(format!(
            "wiki transport down: {slug}"
        )))
    }
    async fn list_docs(&self, _prefix: &str) -> uwuwu_usecase::UsecaseResult<Vec<String>> {
        Ok(vec![])
    }
    async fn write_doc(
        &self,
        _slug: &str,
        _content: &str,
        _message: &str,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        Ok(())
    }
    async fn append_doc(
        &self,
        _slug: &str,
        _line: &str,
        _message: &str,
    ) -> uwuwu_usecase::UsecaseResult<uwuwu_usecase::AppendDocOutcome> {
        Ok(uwuwu_usecase::AppendDocOutcome {
            created: false,
            lines: 0,
        })
    }
}

/// Гвард пустого патча через диспетчер: TaskPatch::is_empty в домене.
#[tokio::test]
async fn task_edit_empty_patch_is_loudly_rejected() {
    let tools = hub();
    let err = call(&tools, "task_edit", json!({ "number": 1 }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "empty patch: set at least one field");
}

#[tokio::test]
async fn task_get_requires_number() {
    let tools = hub();
    let err = call(&tools, "task_get", json!({}))
        .await
        .expect_err("отказ");
    assert_eq!(err, "number is required (an integer)");
}

/// need_number принимает и число, и строку-число — наблюдаемое удобство.
#[tokio::test]
async fn task_get_accepts_number_as_string() {
    let tools = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "пин", "body": "## Постановка\n\nтело" }),
    )
    .await
    .expect("create");
    let number: u64 = created
        .trim_start_matches("task #")
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .expect("number");
    let text = call(&tools, "task_get", json!({ "number": number.to_string() }))
        .await
        .expect("get строкой");
    assert!(text.starts_with(&format!("#{number} пин — Analys")));
    assert!(text.contains("body_hash: "));
}

/// Метка времени коммента в выводе task_get (#1432): строка после «---»
/// — ISO 8601 UTC ровно формата YYYY-MM-DDTHH:MM:SSZ (контракт
/// тайминг-разборов дримера); тело коммента следует за меткой.
#[tokio::test]
async fn task_get_comment_carries_iso_utc_stamp() {
    let tools = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "пин метки", "body": "## Постановка\n\nтело" }),
    )
    .await
    .expect("create");
    let number: u64 = created
        .trim_start_matches("task #")
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .expect("number");
    call(
        &tools,
        "task_comment",
        json!({ "number": number, "text": "вербатим" }),
    )
    .await
    .expect("comment");
    let text = call(&tools, "task_get", json!({ "number": number }))
        .await
        .expect("get");
    let block = text
        .split("--- comments (latest 1 of 1) ---\n")
        .nth(1)
        .expect("хвост комментов");
    let stamp = block
        .trim_start_matches("---\n")
        .lines()
        .next()
        .expect("строка метки");
    assert_eq!(stamp.len(), 20, "метка YYYY-MM-DDTHH:MM:SSZ ровно: {stamp}");
    let separators = [
        (4, '-'),
        (7, '-'),
        (10, 'T'),
        (13, ':'),
        (16, ':'),
        (19, 'Z'),
    ];
    for (i, ch) in stamp.chars().enumerate() {
        match separators.iter().find(|(pos, _)| *pos == i) {
            Some((_, expected)) => assert_eq!(ch, *expected, "позиция {i} в «{stamp}»"),
            None => assert!(ch.is_ascii_digit(), "позиция {i} в «{stamp}» — не цифра"),
        }
    }
    let body_line = block
        .trim_start_matches("---\n")
        .lines()
        .nth(1)
        .expect("тело за меткой");
    assert_eq!(body_line, "вербатим");
}

#[tokio::test]
async fn task_create_rejects_unknown_type_with_canonical_list() {
    let tools = hub();
    let err = call(
        &tools,
        "task_create",
        json!({ "title": "пин", "type": "супер" }),
    )
    .await
    .expect_err("отказ");
    assert_eq!(
        err,
        "unknown type «супер»: bug|spike|docs|other|feature|refactoring|legacy|qa|infrastructure"
    );
}

#[tokio::test]
async fn session_export_requires_ses_prefix() {
    let tools = hub();
    let err = call(&tools, "session_export", json!({ "session": "мусор" }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "session must be ses_…");
}

#[tokio::test]
async fn task_body_section_requires_base_hash() {
    let tools = hub();
    let err = call(
        &tools,
        "task_body_section",
        json!({ "number": 1, "section": "Постановка", "content": "x" }),
    )
    .await
    .expect_err("отказ");
    assert_eq!(
        err,
        "base_hash is required (body_hash from the fm of task_get)"
    );
}

/// Ветка `_` match-таблицы диспетчера: неизвестное имя — громкий отказ.
#[tokio::test]
async fn unknown_tool_is_loud_at_dispatcher() {
    let tools = hub();
    let err = call(&tools, "нет_такого_тула", json!({}))
        .await
        .expect_err("отказ");
    assert_eq!(err, "unknown tool: нет_такого_тула");
}

/// Главный сценарий через живой диспетчер: create → квитанция Analys,
/// get → fm с body_hash и телом; task_list показывает строку.
#[tokio::test]
async fn create_get_list_roundtrip_reports_analys_receipt() {
    let tools = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "круговерть", "body": "## Постановка\n\nтело" }),
    )
    .await
    .expect("create");
    assert_eq!(created, "task #1 created (Analys) on the uwuwu board");
    let get = call(&tools, "task_get", json!({ "number": 1 }))
        .await
        .expect("get");
    assert!(get.starts_with("#1 круговерть — Analys"));
    assert!(get.contains("body_hash: "));
    assert!(get.contains("## Постановка"));
    let list = call(&tools, "task_list", json!({})).await.expect("list");
    assert!(list.contains("круговерть"));
    assert!(list.contains("Analys"));
}

// ---- Пины статуса Wait на живом диспетчере (#1441) ----
// (tester #1383: пины восстановлены из master — ребейз унёс файл целиком
// по --theirs, и четыре пина #1441 выпали из контура.)

/// task_edit ставит Wait, чтение возвращает канон Wait (create → edit →
/// get → list на живом диспетчере, не юнит).
#[tokio::test]
async fn status_wait_edit_reads_back_canon_wait() {
    let tools = hub();
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "wait-прогон", "direct": true }),
    )
    .await
    .expect("create direct");
    assert_eq!(
        created,
        "direct task #1 created (In Progress) on the uwuwu board"
    );
    let edited = call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Wait" }),
    )
    .await
    .expect("edit Wait");
    assert_eq!(edited, "task #1 updated, carrier confirms: \"Wait\"");
    let get = call(&tools, "task_get", json!({ "number": 1 }))
        .await
        .expect("get");
    assert!(get.starts_with("#1 wait-прогон — Wait"));
    let list = call(&tools, "task_list", json!({})).await.expect("list");
    assert!(list.contains("Wait"), "строка в Wait видна в списке");
}

/// Алиас «Вейт» — мост совместимости (#1441): парсится в Wait и ставит
/// канон-опцию Wait идемпотентно (повторная установка не отказ).
#[tokio::test]
async fn status_weit_alias_sets_canon_wait() {
    let tools = hub();
    call(
        &tools,
        "task_create",
        json!({ "title": "wait-алиас", "direct": true }),
    )
    .await
    .expect("create direct");
    let edited = call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Вейт" }),
    )
    .await
    .expect("edit алиасом");
    assert_eq!(edited, "task #1 updated, carrier confirms: \"Wait\"");
    let get = call(&tools, "task_get", json!({ "number": 1 }))
        .await
        .expect("get");
    assert!(
        get.starts_with("#1 wait-алиас — Wait"),
        "алиас ставит канон Wait"
    );
}

/// Нераспознанный статус — громкий отказ гарда канона, состояние строки
/// не меняется (Empty не ставится молча).
#[tokio::test]
async fn status_garbage_is_loud_guard_reject() {
    let tools = hub();
    call(
        &tools,
        "task_create",
        json!({ "title": "wait-мусор", "direct": true }),
    )
    .await
    .expect("create direct");
    call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Wait" }),
    )
    .await
    .expect("edit Wait");
    let err = call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Ерунда" }),
    )
    .await
    .expect_err("отказ");
    assert_eq!(
        err,
        "board: transition Wait → Empty outside the pipeline canon"
    );
    let get = call(&tools, "task_get", json!({ "number": 1 }))
        .await
        .expect("get");
    assert!(
        get.starts_with("#1 wait-мусор — Wait"),
        "статус не изменился"
    );
}

/// Контракт тула task_edit (#1441): строка канона статусов несёт «Wait»,
/// старое имя «Вейт» из описания убрано — агент видит новый канон.
#[tokio::test]
async fn task_edit_descriptor_carries_wait_canon() {
    let desc = super::tools::tool_descriptors();
    let edit = desc
        .as_array()
        .expect("дескрипторы — массив")
        .iter()
        .find(|t| t["name"] == "task_edit")
        .expect("task_edit в дескрипторах");
    let status = edit["inputSchema"]["properties"]["status"]["description"]
        .as_str()
        .expect("status description");
    assert!(status.contains("/Wait"), "канон в описании тула: {status}");
    assert!(!status.contains("Вейт"), "старое имя не в контракте тула");
}

/// Пины rename статуса канбана (#1447): happy-path на MemoryHub-носителе
/// (строка Done не задета — распределение живо), отказы до мутации —
/// точные тексты (агент по ним различает ветки повтора/коллизии/no-op).
#[tokio::test]
async fn board_status_rename_happy_reports_journal_and_histogram() {
    let tools = hub();
    let answer = call(
        &tools,
        "board_status_rename",
        json!({ "from": "Deleted", "to": "Trash" }),
    )
    .await
    .expect("rename");
    assert!(
        answer.contains("Renamed \"Deleted\" → \"Trash\""),
        "{answer}"
    );
    assert!(answer.contains("option id kept: mopt-deleted"), "{answer}");
    assert!(
        answer.contains("Pre-image journal: /memory/rename-pre-image.json"),
        "{answer}"
    );
    assert!(answer.contains("Histogram before"), "{answer}");
    // не-каноническое имя — warn, не отказ (стоп-краны не заводим)
    assert!(
        answer.contains("WARNING: new name \"Trash\" is not a canonical"),
        "{answer}"
    );
}

#[tokio::test]
async fn board_status_rename_unknown_from_is_loud_and_hints_retry() {
    let tools = hub();
    let err = call(
        &tools,
        "board_status_rename",
        json!({ "from": "Несуществующий", "to": "Trash" }),
    )
    .await
    .expect_err("отказ");
    assert!(err.contains("not found"), "{err}");
    assert!(
        err.contains("already renamed"),
        "подсказка про повтор: {err}"
    );
    assert!(err.contains("pre-image journal"), "{err}");
}

#[tokio::test]
async fn board_status_rename_to_equals_from_refuses() {
    let tools = hub();
    let err = call(
        &tools,
        "board_status_rename",
        json!({ "from": "Draft", "to": "Draft" }),
    )
    .await
    .expect_err("отказ");
    assert!(err.contains("no-op"), "{err}");
}

#[tokio::test]
async fn board_status_rename_collision_refuses() {
    let tools = hub();
    let err = call(
        &tools,
        "board_status_rename",
        json!({ "from": "Draft", "to": "Done" }),
    )
    .await
    .expect_err("отказ");
    assert!(err.contains("collision"), "{err}");
}

#[tokio::test]
async fn board_status_rename_missing_args_keep_exact_text() {
    let tools = hub();
    let err = call(&tools, "board_status_rename", json!({}))
        .await
        .expect_err("отказ");
    assert_eq!(err, "from is required (current option name)");
    let err = call(&tools, "board_status_rename", json!({ "from": "Draft" }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "to is required (new option name)");
}

#[tokio::test]
async fn board_status_rename_project_outside_config_refuses() {
    let tools = hub();
    let err = call(
        &tools,
        "board_status_rename",
        json!({ "from": "Draft", "to": "Trash", "project": "origa" }),
    )
    .await
    .expect_err("отказ");
    assert!(err.contains("not in config"), "{err}");
}

// ---- Форма квитанции Unverified на поверхности MCP (тест-ревью C2 #1437) ----

/// Причина фейка — каноничный текст Unverified из hub.rs: rate limit на
/// verify-риде (единственная ветка, где задача принята, а применения
/// свежие чтения не показали).
const UNVERIFIED_REASON: &str = "GraphQL rate limit — confirmation not performed";

/// Фейк доски пина: всё делегирует MemoryHub, кроме set_status — запись
/// ПРИМЕНЯЕТСЯ (носитель сдвинулся, write-through жив — как PATCH на
/// бою), а исход отвечает Unverified. Точная имитация rate-limit ветки
/// verify (#1437): GitHub принял, квитанция успеха не заявляет.
struct UnverifiedStatusBoard {
    inner: Arc<crate::mockhub::MemoryHub>,
}

#[async_trait::async_trait]
impl uwuwu_usecase::TaskBoardRepo for UnverifiedStatusBoard {
    async fn read_board(
        &self,
        board: &str,
    ) -> uwuwu_usecase::UsecaseResult<Vec<uwuwu_domain::Task>> {
        self.inner.read_board(board).await
    }

    async fn set_status(
        &self,
        board: &str,
        number: u64,
        status: uwuwu_domain::Status,
    ) -> uwuwu_usecase::UsecaseResult<uwuwu_usecase::StatusDelivery> {
        self.inner.set_status(board, number, status).await?;
        Ok(uwuwu_usecase::StatusDelivery::Unverified {
            reason: UNVERIFIED_REASON.into(),
        })
    }

    async fn row_status(
        &self,
        board: &str,
        number: u64,
    ) -> uwuwu_usecase::UsecaseResult<Option<uwuwu_domain::Status>> {
        self.inner.row_status(board, number).await
    }

    async fn row_exists(&self, board: &str, number: u64) -> uwuwu_usecase::UsecaseResult<bool> {
        self.inner.row_exists(board, number).await
    }

    async fn set_session(
        &self,
        board: &str,
        number: u64,
        slot: uwuwu_domain::Slot,
        session: &str,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.set_session(board, number, slot, session).await
    }

    async fn set_branch(
        &self,
        board: &str,
        number: u64,
        branch: &uwuwu_domain::BranchName,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.set_branch(board, number, branch).await
    }
    async fn set_pr(
        &self,
        board: &str,
        number: u64,
        pr_url: &uwuwu_domain::PrUrl,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.set_pr(board, number, pr_url).await
    }

    async fn set_board_cells(
        &self,
        board: &str,
        number: u64,
        priority: Option<&str>,
        automerge: Option<bool>,
        direct: Option<bool>,
        fact_track: Option<bool>,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner
            .set_board_cells(board, number, priority, automerge, direct, fact_track)
            .await
    }

    async fn clear_session(
        &self,
        board: &str,
        number: u64,
        slot: uwuwu_domain::Slot,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.clear_session(board, number, slot).await
    }

    async fn post_comment(&self, number: u64, body: &str) -> uwuwu_usecase::UsecaseResult<String> {
        self.inner.post_comment(number, body).await
    }

    async fn close_issue(&self, number: u64) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.close_issue(number).await
    }

    async fn latest_comments(
        &self,
        number: u64,
    ) -> uwuwu_usecase::UsecaseResult<Vec<uwuwu_usecase::CommentRef>> {
        self.inner.latest_comments(number).await
    }

    async fn read_issue(
        &self,
        board: &str,
        number: u64,
    ) -> uwuwu_usecase::UsecaseResult<uwuwu_usecase::IssueView> {
        self.inner.read_issue(board, number).await
    }

    async fn issue_fields(
        &self,
        number: u64,
    ) -> uwuwu_usecase::UsecaseResult<uwuwu_domain::TaskFields> {
        self.inner.issue_fields(number).await
    }

    async fn create_issue(
        &self,
        board: &str,
        project: &str,
        title: &str,
        body: &str,
        kind_name: Option<&str>,
    ) -> uwuwu_usecase::UsecaseResult<u64> {
        self.inner
            .create_issue(board, project, title, body, kind_name)
            .await
    }

    async fn set_issue_type(
        &self,
        number: u64,
        kind_name: &str,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.set_issue_type(number, kind_name).await
    }

    async fn rename_issue(&self, number: u64, title: &str) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.rename_issue(number, title).await
    }

    async fn patch_cells(
        &self,
        number: u64,
        mutate: uwuwu_usecase::FmMutator,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.patch_cells(number, mutate).await
    }

    async fn close_issue_ext(
        &self,
        number: u64,
        not_planned: bool,
    ) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.close_issue_ext(number, not_planned).await
    }

    async fn archive_row(&self, board: &str, number: u64) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.archive_row(board, number).await
    }

    async fn issues_with_title(&self, title: &str) -> uwuwu_usecase::UsecaseResult<Vec<u64>> {
        self.inner.issues_with_title(title).await
    }

    async fn replace_body(&self, number: u64, body_md: &str) -> uwuwu_usecase::UsecaseResult<()> {
        self.inner.replace_body(number, body_md).await
    }
}

/// Диспетчер на подменённой доске: сборка фабрики та же, writer/tasks
/// смотрят на UnverifiedStatusBoard; aux — та же память, что под фейком
/// (носитель, куда фейк применяет запись).
fn hub_unverified() -> Arc<ToolHub> {
    let memory = Arc::new(crate::mockhub::MemoryHub::from_env());
    let board: Arc<dyn uwuwu_usecase::TaskBoardRepo> = Arc::new(UnverifiedStatusBoard {
        inner: memory.clone(),
    });
    let (tools, _aux) = super::test_support::tool_hub_on_board(
        Arc::new(crate::mockhub::MemoryHub::from_env()),
        blob_usecase(),
        Arc::new(StubCronState::default()),
        board,
        memory,
    );
    tools
}

/// Форма квитанции Unverified (тест-ревью C2 #1437): статусная запись
/// принята GitHub (носитель сдвинулся), свежее чтение применение не
/// подтвердило (rate limit на verify-риде). Ответ MCP НЕ заявляет
/// success: «did NOT confirm» + предписание контрольного чтения
/// НОСИТЕЛЯ, слепой ретрай осуждён. Сквозь настоящий TaskWriteUseCase
/// (исход проходит write.rs насквозь до текста тула). Второй смысл
/// пина: «accepted, not confirmed» ≠ «не применено» — носитель несёт
/// записанный статус.
#[tokio::test]
async fn task_edit_status_unverified_receipt_does_not_claim_success() {
    let tools = hub_unverified();
    let created = call(
        &tools,
        "task_create",
        json!({ "title": "unverified-прогон", "direct": true }),
    )
    .await
    .expect("create direct");
    assert_eq!(
        created,
        "direct task #1 created (In Progress) on the uwuwu board"
    );
    let edited = call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Wait" }),
    )
    .await
    .expect("edit Wait — исход Unverified не Err, задача принята");
    assert_eq!(
        edited,
        format!(
            "task #1: status accepted by GitHub, carrier read did NOT confirm \
             ({UNVERIFIED_REASON}) — verify the CARRIER (gh, board) yourself, do not retry blindly"
        )
    );
    // write-through жив: носитель (внутренность фейка) несёт Wait —
    // «accepted, not confirmed» ≠ «не применено»; task_get идёт через
    // TaskView → фейк → тот же носитель, куда писала запись
    let get = call(&tools, "task_get", json!({ "number": 1 }))
        .await
        .expect("get");
    assert!(
        get.starts_with("#1 unverified-прогон — Wait"),
        "запись применена несмотря на Unverified: {get}"
    );
}
