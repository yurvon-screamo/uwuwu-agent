//! Текстовые пины поверхности MCP (#1167, допокрытие тестировщика):
//! армы вне тест-покрытия tools_tests.rs (task_create/task_edit/task_list/
//! task_get/docs_*/search/session_export + ветка unknown диспетчера).
//! Рефакторинг #1167 переносил тела байт-в-байт; пины держат наблюдаемые
//! тексты отказов и ответов, чтобы дрейф вернуть нечем. Пробы через живой
//! диспетчер на MemoryHub — без GitHub, как в tools_tests.rs.

use super::test_support::{blob_usecase, tool_hub};
use super::tools::ToolHub;
use serde_json::{json, Value};
use std::sync::Arc;

/// ToolHub фабрики test_support: MemoryHub как вики (docs_* армы пинов
/// читают живой носитель) + memory-blob.
fn hub() -> Arc<ToolHub> {
    let (tools, _memory) = tool_hub(
        Arc::new(crate::mockhub::MemoryHub::from_env()),
        blob_usecase(),
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
    assert_eq!(err, "query обязателен");
}

#[tokio::test]
async fn docs_append_requires_slug_and_line() {
    let tools = hub();
    let err = call(&tools, "docs_append", json!({}))
        .await
        .expect_err("отказ");
    assert_eq!(err, "slug и line обязательны");
}

/// Гард append-семантики docs_append (#1353): дописывание сохраняет
/// тело существующего дока (fm + контент), строка — в конец; ответ
/// различает append и создание.
#[tokio::test]
async fn docs_append_keeps_body_of_existing_doc() {
    let tools = hub();
    let slug = "Experience/devops/Опыт (srv, #1248)";
    call(
        &tools,
        "docs_write",
        json!({ "slug": slug, "content": "строка A" }),
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
        answer.contains("дописано") && answer.contains("строк в доке: 2"),
        "ответ append без факта операции: {answer}"
    );
    let stored = call(&tools, "docs_read", json!({ "slug": slug }))
        .await
        .expect("read");
    // append_to нормализует EOF: итог всегда с завершающим переносом
    assert_eq!(stored, "строка A\nстрока B\n");
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
        answer.contains("создан"),
        "создание не названо созданием: {answer}"
    );
}

#[tokio::test]
async fn docs_write_requires_slug_and_content() {
    let tools = hub();
    let err = call(&tools, "docs_write", json!({ "slug": "x/y" }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "slug и content обязательны");
}

/// Гвард пустого патча через диспетчер: TaskPatch::is_empty в домене.
#[tokio::test]
async fn task_edit_empty_patch_is_loudly_rejected() {
    let tools = hub();
    let err = call(&tools, "task_edit", json!({ "number": 1 }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "пустой патч: укажи хотя бы одно поле");
}

#[tokio::test]
async fn task_get_requires_number() {
    let tools = hub();
    let err = call(&tools, "task_get", json!({}))
        .await
        .expect_err("отказ");
    assert_eq!(err, "number обязателен (число)");
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
        .trim_start_matches("создана задача #")
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
        .trim_start_matches("создана задача #")
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
        .split("--- комменты (последние) ---\n")
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
        "неизвестный тип «супер»: bug|spike|docs|other|feature|refactoring|legacy|qa|infrastructure"
    );
}

#[tokio::test]
async fn session_export_requires_ses_prefix() {
    let tools = hub();
    let err = call(&tools, "session_export", json!({ "session": "мусор" }))
        .await
        .expect_err("отказ");
    assert_eq!(err, "session обязан быть ses_…");
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
    assert_eq!(err, "base_hash обязателен (body_hash из fm task_get)");
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
    assert_eq!(created, "создана задача #1 (Analys) на доске проекта uwuwu");
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

/// Переименование статуса «Вейт» → «Wait» (#1441), главный сценарий на
/// живом диспетчере: direct-строка (In Progress) переводится в Wait,
/// чтение тулами возвращает канон «Wait», строка видна в task_list.
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
        "создана direct-задача #1 (In Progress) на доске проекта uwuwu"
    );
    let edited = call(
        &tools,
        "task_edit",
        json!({ "number": 1, "status": "Wait" }),
    )
    .await
    .expect("edit Wait");
    assert_eq!(edited, "задача #1 обновлена, носитель подтверждает: «Wait»");
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
    assert_eq!(edited, "задача #1 обновлена, носитель подтверждает: «Wait»");
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
    assert_eq!(err, "доска: переход Wait → Empty вне канона конвейера");
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
