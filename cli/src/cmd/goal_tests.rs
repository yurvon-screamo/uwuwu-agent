//! goal list/add/edit/rm на wiremock-ядре: доска целей заводится владельцем
//! в UI (CLI громко отказывается без неё и без Status-колонки), строки bound
//! к телам-саб-докам, статусы канонические todo/in progress/done, rm с
//! write-ahead undo-журналом.
//!
//! Сценарии:
//! 1. доски нет → отказ с инструкцией «заводит владелец», ноль мутаций
//! 2. доска без Status-колонки → отказ до любых записей
//! 3. add: bound-строка + Status mSelect + тело «## Суть»
//! 4. add: дубль-тайтл в живых строках → отказ без мутаций
//! 5. add: вставка не подтверждается render → ровно один ретрай, отказ
//! 6. edit --status канонизирует (IN_PROGRESS → in progress); вне канона —
//!    отказ без записей
//! 7. edit --title → renameDoc тела + verify Title-клетки
//! 8. rm: undo-журнал → item_remove → тело в трэш
//! 9. list: таблица СТАТУС/СТРОКА/ЦЕЛЬ, фильтр --status, маркер (!)
//! 10. парс: --project global у всей семьи, без него — отказ

use crate::cmd::cli::{GoalSub, TaskSub};
use crate::cmd::sy_dispatch::{format_goal_table, goal_run, task_run};
use crate::siyuan_task::SyTask;
use serde_json::json;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NB: &str = "nb-projects";
const GOAL_DOC: &str = "g-doc";
const GOAL_AV: &str = "av-g";

/// Мутабельное состояние мок-ядра: что создали и что записали.
#[derive(Default)]
struct Kernel {
    /// hpath созданных доков (createDocWithMd).
    created_docs: Mutex<Vec<String>>,
    /// Аргументы item_add как есть (blockID различим).
    item_adds: Mutex<Vec<serde_json::Value>>,
    /// (av, keyID, itemID, value) всех item_update.
    item_updates: Mutex<Vec<(String, String, String, serde_json::Value)>>,
    /// id строк, снятых item_remove (goal rm).
    item_removes: Mutex<Vec<String>>,
    /// Workspace-файлы (putFile → getFile): undo-журналы для undo-сценария.
    undo_files: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    /// Счётчик exportMdContent (journal_row → body_md).
    export_calls: Mutex<usize>,
    /// docId, отправленные в трэш (removeDocByID).
    removed_docs: Mutex<Vec<String>>,
    /// Имена write-ahead журналов (putFile multipart).
    journal_files: Mutex<Vec<String>>,
    /// false → дока «Цели uwuwu» нет (владелец ещё не завёл).
    goal_doc_exists: Mutex<bool>,
    /// false → keys доски без колонки Status (легаси-шаблон).
    goal_keys_full: Mutex<bool>,
    /// false → item_add не появляется в render (ядро «потеряло» вставку).
    render_confirms_insert: Mutex<bool>,
    /// true → renameDoc меняет Title-клетку сразу (синк без лага).
    rename_applies: Mutex<bool>,
    /// true → block get отвечает «док жив» (goal rm); false — привязка мертва.
    bodies_alive: Mutex<bool>,
    /// Строки render доски целей.
    goal_rows: Mutex<Vec<GoalRow>>,
}

/// Строка render доски целей в мок-ядре. `body_doc` — валидный node-id тела
/// (block.id живой клетки): невалидный id валит export_md_by_id (ensure_node_id)
/// и роняет undo-путь с detached-восстановлением.
struct GoalRow {
    item: String,
    title: String,
    status: String,
    body_doc: String,
}

fn goal_keys() -> serde_json::Value {
    json!([
        { "id": "key-title", "name": "Первичный ключ" },
        { "id": "key-status", "name": "Status" }
    ])
}

fn template_keys() -> serde_json::Value {
    // легаси-миграция: task-шаблон без Status (только первичный ключ)
    json!([{ "id": "key-title", "name": "Первичный ключ" }])
}

fn render_view(kernel: &Kernel) -> serde_json::Value {
    let columns = goal_keys();
    let rows: Vec<serde_json::Value> = kernel
        .goal_rows
        .lock()
        .unwrap()
        .iter()
        .map(|row| {
            json!({
                "id": row.item,
                "cells": [
                    { "value": { "keyID": "key-title", "type": "block", "block": { "content": row.title, "refSubtype": "d", "id": row.body_doc } } },
                    { "value": { "keyID": "key-status", "type": "mSelect", "mSelect": [{ "content": row.status }] } }
                ]
            })
        })
        .collect();
    let count = rows.len();
    json!({ "action": "render", "data": { "view": { "columns": columns, "rowCount": count, "rows": rows } } })
}

fn ok_kernel_json(data: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(json!({ "code": 0, "data": data }).to_string())
}

/// SQL-проба: av-блок доски целей по root_id (поиск database-носителя).
fn handle_sql(body: &[u8]) -> ResponseTemplate {
    let body: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
    let data = stmt
        .split("root_id='")
        .nth(1)
        .map(|s| s.split('\'').next().unwrap_or(""))
        .and_then(|root| {
            (root == GOAL_DOC).then(|| {
                json!([{
                    "id": format!("block-{root}"),
                    "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{GOAL_AV}\"></div>")
                }])
            })
        })
        .unwrap_or_else(|| json!([]));
    ok_kernel_json(data)
}

/// getIDsByHPath: доска «Цели uwuwu» (если владелец завёл) + родительский
/// док «uwuwu» — по одному id (board_doc_chain идёт по префиксам hpath).
fn handle_hpath(kernel: &Arc<Kernel>, body: &[u8]) -> ResponseTemplate {
    let body: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let p = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let ids: Vec<&str> = match p {
        "/uwuwu" => vec!["uwuwu-root"],
        "/uwuwu/Цели uwuwu" if *kernel.goal_doc_exists.lock().unwrap() => vec![GOAL_DOC],
        _ => vec![],
    };
    ok_kernel_json(ids.into())
}

static NEXT_DOC_ID: Mutex<usize> = Mutex::new(0);

/// createDocWithMd: тело цели; hpath-счётчик — детектор «сиротских» мутаций.
fn handle_create_doc(kernel: &Arc<Kernel>, body: &[u8]) -> ResponseTemplate {
    let body: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    kernel.created_docs.lock().unwrap().push(
        body.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    );
    let mut next = NEXT_DOC_ID.lock().unwrap();
    let id = format!("20261107120000-body{next:02}");
    *next += 1;
    ok_kernel_json(serde_json::Value::String(id))
}

/// putFile: write-ahead undo-журнал (goal rm); multipart: path + file-часть.
fn handle_put_file(kernel: &Arc<Kernel>, body: &[u8]) -> ResponseTemplate {
    let fields = multipart_fields(body);
    let path = fields
        .iter()
        .find(|(name, _)| name == "path")
        .map(|(_, v)| String::from_utf8_lossy(v).to_string())
        .unwrap_or_default();
    if let Some((_, bytes)) = fields.iter().find(|(name, _)| name == "file") {
        kernel
            .undo_files
            .lock()
            .unwrap()
            .insert(path.clone(), bytes.clone());
    }
    if !path.is_empty() {
        kernel.journal_files.lock().unwrap().push(path);
    }
    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"","data":null}"#)
}

/// getFile: undo-журналы и файлы досок (.sy) — байты; остальное — Missing
/// (контракт ядра 3.8.9: отсутствующий файл молча отвечает 200+0 байт).
fn handle_get_file(kernel: &Arc<Kernel>, body: &[u8]) -> ResponseTemplate {
    let body: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let path = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
    let stored = kernel.undo_files.lock().unwrap().get(path).cloned();
    match stored {
        Some(bytes) => ResponseTemplate::new(200)
            .set_body_bytes(bytes)
            .append_header("Content-Type", "application/octet-stream"),
        _ if path.ends_with(".sy") => ResponseTemplate::new(200).set_body_string("{}"),
        _ => ResponseTemplate::new(200).set_body_string(""),
    }
}

/// renameDoc: меняет Title-клетку строки сразу (синк без лага) или ничего —
/// по rename_applies мок имитирует лаг синка ядра.
fn handle_rename(kernel: &Arc<Kernel>, body: &[u8]) -> ResponseTemplate {
    let body: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let new_title = body
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    if *kernel.rename_applies.lock().unwrap() {
        for row in kernel.goal_rows.lock().unwrap().iter_mut() {
            // renameDoc пути несёт docId тела: body-<itemID> в моке не отличим
            // по id-цепочке — мок-ядро хранит одну строку
            row.title = new_title.clone();
        }
    }
    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"","data":null}"#)
}

/// removeDocByID: тела целей в трэш.
fn handle_remove_doc(kernel: &Arc<Kernel>, body: &[u8]) -> ResponseTemplate {
    let body: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    kernel.removed_docs.lock().unwrap().push(
        body.get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string(),
    );
    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"","data":null}"#)
}

fn mcp_text(payload: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(
        json!({
            "jsonrpc": "2.0", "id": 2,
            "result": { "content": [{ "type": "text", "text": payload.to_string() }] }
        })
        .to_string(),
    )
}

/// Черновой разбор multipart-тела: (name поля, байты значения). Достаточно
/// для тестового мока putFile (path + file), boundary — первая строка тела.
fn multipart_fields(body: &[u8]) -> Vec<(String, Vec<u8>)> {
    let first_line_end = body.windows(2).position(|w| w == b"\r\n").unwrap_or(0);
    let boundary_line = &body[..first_line_end];
    let boundary = boundary_line.strip_prefix(b"--").unwrap_or(boundary_line);
    let mut marker = b"--".to_vec();
    marker.extend_from_slice(boundary);
    let mut fields = Vec::new();
    for part in split_on_marker(body, &marker) {
        let Some(headers_end) = part.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&part[..headers_end]).to_lowercase();
        let mut content = &part[headers_end + 4..];
        if content.ends_with(b"\r\n") {
            content = &content[..content.len() - 2];
        }
        let name = headers
            .split("name=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or("")
            .to_string();
        if !name.is_empty() {
            fields.push((name, content.to_vec()));
        }
    }
    fields
}

/// Разрез байтового среза по маркеру (замена slice::split для маркера >1 байта).
fn split_on_marker<'a>(data: &'a [u8], marker: &[u8]) -> Vec<&'a [u8]> {
    let mut parts = Vec::new();
    let mut rest = data;
    'outer: while !rest.is_empty() {
        if let Some(pos) = rest.windows(marker.len()).position(|w| w == marker) {
            parts.push(&rest[..pos]);
            rest = &rest[pos + marker.len()..];
        } else {
            parts.push(rest);
            break 'outer;
        }
    }
    parts
}

async fn mount_kernel(server: &MockServer, kernel: &Arc<Kernel>) {
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(|req: &Request| handle_sql(&req.body))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/notebook/lsNotebooks"))
        .respond_with(move |_req: &Request| {
            ResponseTemplate::new(200).set_body_string(
                json!({ "code": 0, "data": { "notebooks": [ { "id": NB, "name": "Projects", "closed": false } ] } })
                    .to_string(),
            )
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/filetree/getIDsByHPath"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| handle_hpath(&kernel, &req.body)
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/filetree/createDocWithMd"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| handle_create_doc(&kernel, &req.body)
        })
        .mount(server)
        .await;

    // putFile: write-ahead undo-журнал (goal rm); multipart: path + file-часть
    Mock::given(method("POST"))
        .and(path("/api/file/putFile"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| handle_put_file(&kernel, &req.body)
        })
        .mount(server)
        .await;

    // exportMdContent: тело цели в журнал (journal_row → body_md)
    Mock::given(method("POST"))
        .and(path("/api/export/exportMdContent"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                *kernel.export_calls.lock().unwrap() += 1;
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let id = body.get("id").and_then(|i| i.as_str()).unwrap_or("");
                ResponseTemplate::new(200).set_body_string(
                    json!({ "code": 0, "data": { "content": format!("## Суть\n\nтело цели {id}\n") } })
                        .to_string(),
                )
            }
        })
        .mount(server)
        .await;

    // getFile: undo-журналы и файлы досок (.sy) — байты; остальное — Missing
    // (контракт ядра 3.8.9: отсутствующий файл молча отвечает 200+0 байт)
    Mock::given(method("POST"))
        .and(path("/api/file/getFile"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| handle_get_file(&kernel, &req.body)
        })
        .mount(server)
        .await;

    // renameDoc: меняет Title-клетку строки сразу (синк без лага) или ничего
    Mock::given(method("POST"))
        .and(path("/api/filetree/renameDoc"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| handle_rename(&kernel, &req.body)
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/filetree/removeDocByID"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| handle_remove_doc(&kernel, &req.body)
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":3}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-goal");
                }
                if body.contains("notifications/initialized") {
                    return ResponseTemplate::new(200);
                }
                if !body.contains("tools/call") {
                    return ResponseTemplate::new(200)
                        .set_body_string(r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#);
                }
                let parsed: serde_json::Value = body.parse().unwrap_or_default();
                let args = parsed.pointer("/params/arguments").cloned().unwrap_or_default();
                mock_mcp_database(&kernel, &args)
            }
        })
        .mount(server)
        .await;
}

/// Обработчик tools/call database-инструмента: действия keys/item_add/
/// item_update/item_remove/get/render — мутабельное состояние kernel.
fn mock_mcp_database(kernel: &Arc<Kernel>, args: &serde_json::Value) -> ResponseTemplate {
    let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("");
    let av = args
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match action {
        "keys" => {
            let full = *kernel.goal_keys_full.lock().unwrap();
            let keys = if full { goal_keys() } else { template_keys() };
            mcp_text(json!({ "action": "keys", "data": { "keys": keys } }))
        }
        "item_add" => {
            kernel.item_adds.lock().unwrap().push(args.clone());
            let item = "20261107120000-item01".to_string();
            if *kernel.render_confirms_insert.lock().unwrap() {
                kernel.goal_rows.lock().unwrap().push(GoalRow {
                    item: item.clone(),
                    title: "новая цель".into(),
                    status: "todo".into(),
                    body_doc: format!("body-{}", item),
                });
            }
            mcp_text(json!({ "action": "item_add", "data": { "itemID": item } }))
        }
        "item_update" => mock_item_update(kernel, args, av),
        "item_remove" => mock_item_remove(kernel, args),
        // block get — текстовый tool (mcp_tool_text читает content[0].text)
        "get" => ResponseTemplate::new(200).set_body_string(
            json!({
                "jsonrpc": "2.0", "id": 2,
                "result": { "content": [{ "type": "text", "text": block_get_text(kernel, &av) }] }
            })
            .to_string(),
        ),
        "render" => mcp_text(render_view(kernel)),
        _ => mcp_text(json!({ "action": "unknown", "data": null })),
    }
}

/// item_update: клетка Status отражается в render-состоянии (mSelect-контент).
fn mock_item_update(
    kernel: &Arc<Kernel>,
    args: &serde_json::Value,
    av: String,
) -> ResponseTemplate {
    let key = args
        .get("keyID")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let item = args
        .get("itemID")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let value = args.get("value").cloned().unwrap_or(json!(null));
    // mSelect-запись статуса отражаем в render-состоянии
    if let Some(content) = value.pointer("/mSelect/0/content").and_then(|c| c.as_str()) {
        for row in kernel.goal_rows.lock().unwrap().iter_mut() {
            if row.item == item {
                row.status = content.to_string();
            }
        }
    }
    kernel
        .item_updates
        .lock()
        .unwrap()
        .push((av, key, item, value));
    mcp_text(json!({ "action": "item_update", "data": null }))
}

/// item_remove: строки уходят из render-состояния (undo их возвращает).
fn mock_item_remove(kernel: &Arc<Kernel>, args: &serde_json::Value) -> ResponseTemplate {
    let removed: Vec<String> = args
        .get("itemIDs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|i| i.as_str().map(String::from))
        .collect();
    kernel
        .goal_rows
        .lock()
        .unwrap()
        .retain(|row| !removed.contains(&row.item));
    kernel.item_removes.lock().unwrap().extend(removed);
    mcp_text(json!({ "action": "item_remove", "data": null }))
}

/// block get (doc_block_exists / guard FS-проба): «док жив» по сценарию.
fn block_get_text(kernel: &Arc<Kernel>, av: &str) -> String {
    if *kernel.bodies_alive.lock().unwrap() {
        format!("ID: {av}\nType: NodeDocument\nHPath: /uwuwu/Цели uwuwu/тело")
    } else {
        "not found".to_string()
    }
}

fn set_env(server: &MockServer) {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
}

fn seeded_goal(kernel: &Arc<Kernel>) {
    kernel.goal_rows.lock().unwrap().push(GoalRow {
        item: "20261107110000-g1".into(),
        title: "Отказ от SiYuan в пользу самописного движка".into(),
        status: "in progress".into(),
        body_doc: "20261107110000-b0d0001".into(),
    });
}

/// (1) доски нет: громкий отказ с инструкцией, ноль мутаций.
#[tokio::test]
#[serial_test::serial]
async fn add_bails_loud_when_owner_has_not_created_goal_board() {
    let kernel = Arc::new(Kernel::default());
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = goal_run(
        "uwuwu",
        GoalSub::Add {
            title: "Монетизация".into(),
            status: None,
            stdin: false,
        },
    )
    .await
    .expect_err("доски нет — отказ");

    let msg = err.to_string();
    assert!(
        msg.contains("заводит владелец") && msg.contains("Цели uwuwu"),
        "отказ называет доску и кто её заводит: {msg}"
    );
    assert!(
        kernel.created_docs.lock().unwrap().is_empty(),
        "CLI не создаёт тело до существования доски"
    );
    assert!(
        kernel.item_adds.lock().unwrap().is_empty(),
        "CLI не создаёт строку до существования доски"
    );
}

/// (2) доска есть, Status-колонки нет: отказ до любых записей.
#[tokio::test]
#[serial_test::serial]
async fn add_bails_when_board_lacks_status_key() {
    let kernel = Arc::new(Kernel::default());
    *kernel.goal_doc_exists.lock().unwrap() = true;
    *kernel.goal_keys_full.lock().unwrap() = false;
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = goal_run(
        "uwuwu",
        GoalSub::Add {
            title: "Монетизация".into(),
            status: None,
            stdin: false,
        },
    )
    .await
    .expect_err("Status-колонки нет — отказ");

    assert!(
        err.to_string().contains("Status"),
        "отказ называет недостающую колонку: {err}"
    );
    assert!(
        kernel.item_adds.lock().unwrap().is_empty(),
        "строка не создаётся до валидации колонок"
    );
}

/// (3) add на полноценной доске: bound-строка, Status mSelect todo, тело «## Суть».
#[tokio::test]
#[serial_test::serial]
async fn add_creates_bound_row_with_canonical_status_and_body() {
    let kernel = Arc::new(Kernel::default());
    *kernel.goal_doc_exists.lock().unwrap() = true;
    *kernel.goal_keys_full.lock().unwrap() = true;
    *kernel.render_confirms_insert.lock().unwrap() = true;
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    goal_run(
        "uwuwu",
        GoalSub::Add {
            title: "Монетизация".into(),
            status: None,
            stdin: false,
        },
    )
    .await
    .expect("add на полной доске");

    let created = kernel.created_docs.lock().unwrap();
    assert_eq!(created.len(), 1, "ровно одно тело");
    assert!(
        created[0].contains("Монетизация"),
        "тело по hpath тайтла: {:?}",
        created[0]
    );

    let adds = kernel.item_adds.lock().unwrap();
    assert_eq!(adds.len(), 1, "одна вставка строки (verify с первого раза)");
    assert!(
        !adds[0]
            .get("blockID")
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .is_empty(),
        "строка привязана к телу (blockID): {:?}",
        adds[0]
    );

    let updates = kernel.item_updates.lock().unwrap();
    let status = updates
        .iter()
        .find(|(_, k, _, _)| k == "key-status")
        .expect("Status записан");
    assert_eq!(
        status.3,
        json!({ "mSelect": [{ "content": "todo" }] }),
        "дефолтный статус — todo через mSelect"
    );
}

/// (4) дубль-тайтла в живых строках: отказ без мутаций.
#[tokio::test]
#[serial_test::serial]
async fn add_rejects_duplicate_title_without_mutation() {
    let kernel = Arc::new(Kernel::default());
    *kernel.goal_doc_exists.lock().unwrap() = true;
    *kernel.goal_keys_full.lock().unwrap() = true;
    seeded_goal(&kernel);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = goal_run(
        "uwuwu",
        GoalSub::Add {
            title: "Отказ от SiYuan в пользу самописного движка".into(),
            status: None,
            stdin: false,
        },
    )
    .await
    .expect_err("дубль-тайтл");

    assert!(
        err.to_string().contains("уже есть"),
        "сообщение про дубль: {err}"
    );
    assert!(
        kernel.created_docs.lock().unwrap().is_empty(),
        "тело не создано"
    );
    assert!(
        kernel.item_adds.lock().unwrap().is_empty(),
        "строка не создана"
    );
}

/// (5) ядро «потеряло» вставку: ровно один ретрай, затем громкий отказ.
#[tokio::test]
#[serial_test::serial]
async fn add_retries_once_then_bails_when_row_never_appears() {
    let kernel = Arc::new(Kernel::default());
    *kernel.goal_doc_exists.lock().unwrap() = true;
    *kernel.goal_keys_full.lock().unwrap() = true;
    *kernel.render_confirms_insert.lock().unwrap() = false;
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = goal_run(
        "uwuwu",
        GoalSub::Add {
            title: "призрачная цель".into(),
            status: None,
            stdin: false,
        },
    )
    .await
    .expect_err("строка не появилась в render");

    assert!(
        err.to_string().contains("не появилась"),
        "отказ объясняет причину: {err}"
    );
    assert_eq!(
        kernel.item_adds.lock().unwrap().len(),
        2,
        "ровно один ретрай вставки"
    );
}

/// (6) edit --status: канонизация форматов (IN_PROGRESS → in progress); статус
/// вне канона — отказ без записей.
#[tokio::test]
#[serial_test::serial]
async fn edit_canonicalizes_status_and_rejects_unknown_one() {
    let kernel = Arc::new(Kernel::default());
    *kernel.goal_doc_exists.lock().unwrap() = true;
    *kernel.goal_keys_full.lock().unwrap() = true;
    seeded_goal(&kernel);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    goal_run(
        "uwuwu",
        GoalSub::Edit {
            id: "g1".into(),
            status: Some("IN_PROGRESS".into()),
            title: None,
        },
    )
    .await
    .expect("канонизация статуса");

    let updates_after_canon = {
        let updates = kernel.item_updates.lock().unwrap();
        let status = updates
            .iter()
            .find(|(_, k, _, _)| k == "key-status")
            .expect("Status записан");
        assert_eq!(
            status.3,
            json!({ "mSelect": [{ "content": "in progress" }] }),
            "IN_PROGRESS канонизирован в in progress"
        );
        updates.len()
    };

    let err = goal_run(
        "uwuwu",
        GoalSub::Edit {
            id: "g1".into(),
            status: Some("почти готово".into()),
            title: None,
        },
    )
    .await
    .expect_err("статус вне канона");
    assert!(
        err.to_string().contains("канон целей"),
        "отказ перечисляет канон: {err}"
    );
    let updates = kernel.item_updates.lock().unwrap();
    assert_eq!(
        updates.len(),
        updates_after_canon,
        "вне-канонный статус не доходит до записей"
    );
}

/// (7) edit --title: renameDoc тела + verify Title-клетки в render.
#[tokio::test]
#[serial_test::serial]
async fn edit_title_renames_body_doc_and_confirms_title_cell() {
    let kernel = Arc::new(Kernel::default());
    *kernel.goal_doc_exists.lock().unwrap() = true;
    *kernel.goal_keys_full.lock().unwrap() = true;
    *kernel.rename_applies.lock().unwrap() = true;
    seeded_goal(&kernel);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    goal_run(
        "uwuwu",
        GoalSub::Edit {
            id: "g1".into(),
            status: None,
            title: Some("Самописный движок вместо SiYuan".into()),
        },
    )
    .await
    .expect("переименование цели");

    let rows = kernel.goal_rows.lock().unwrap();
    assert_eq!(
        rows[0].title, "Самописный движок вместо SiYuan",
        "Title-клетка синкнулась renameDoc'ом"
    );
}

/// (8) rm: write-ahead undo-журнал → item_remove → тело в трэш.
#[tokio::test]
#[serial_test::serial]
async fn rm_removes_row_and_trashes_body_with_undo_journal() {
    let kernel = Arc::new(Kernel::default());
    *kernel.goal_doc_exists.lock().unwrap() = true;
    *kernel.goal_keys_full.lock().unwrap() = true;
    *kernel.bodies_alive.lock().unwrap() = true;
    seeded_goal(&kernel);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    goal_run(
        "uwuwu",
        GoalSub::Rm {
            id: "20261107110000-g1".into(),
        },
    )
    .await
    .expect("rm живой цели");

    assert_eq!(
        *kernel.item_removes.lock().unwrap(),
        vec!["20261107110000-g1".to_string()],
        "строка снята item_remove"
    );
    assert_eq!(
        *kernel.removed_docs.lock().unwrap(),
        vec!["20261107110000-b0d0001".to_string()],
        "привязанное тело в трэш (removeDocByID), id из block-клетки"
    );
    let journals = kernel.journal_files.lock().unwrap();
    assert_eq!(journals.len(), 1, "write-ahead журнал один");
    assert!(
        journals[0].ends_with("-uwuwu-goal-rm.json"),
        "журнал kind=goal-rm: {:?}",
        journals
    );
}

/// (9) list: таблица несёт статус/заголовок, маркер (!); сортировка по
/// статусу делается внутри таблицы — порядок входа не влияет на группы.
#[test]
fn list_table_sorts_by_status_and_marks_dead_binding() {
    // вход нарочно в обратном порядке: done раньше in progress
    let rows = vec![
        SyTask {
            item_id: "20261107110000-g2".into(),
            title: "Монетизация".into(),
            status: "done".into(),
            task_type: String::new(),
            priority: String::new(),
            autodone: false,
            branch: String::new(),
            pr: String::new(),
            dev_session: String::new(),
            planned: String::new(),
            planned_ms: 0,
            schedule_cell: String::new(),
            body_doc: None,
        },
        SyTask {
            item_id: "20261107110000-g1".into(),
            title: "Отказ от SiYuan в пользу самописного движка".into(),
            status: "in progress".into(),
            task_type: String::new(),
            priority: String::new(),
            autodone: false,
            branch: String::new(),
            pr: String::new(),
            dev_session: String::new(),
            planned: String::new(),
            planned_ms: 0,
            schedule_cell: String::new(),
            body_doc: Some("body-20261107110000-g1".into()),
        },
    ];
    let mut dead = HashSet::new();
    dead.insert("body-20261107110000-g1".to_string());

    let table = format_goal_table(&rows, &dead);
    assert!(table.contains("СТАТУС"), "заголовок таблицы: {table}");
    assert!(table.contains("Монетизация"), "цель видна: {table}");
    assert!(table.contains("in progress"), "статус виден: {table}");
    assert!(table.contains("(!)"), "мёртвая привязка помечена: {table}");

    // сортировка: in progress раньше done независимо от порядка входа
    let in_progress_pos = table.find("Отказ от SiYuan").expect("g1 в таблице");
    let done_pos = table.find("Монетизация").expect("g2 в таблице");
    assert!(
        in_progress_pos < done_pos,
        "строки сгруппированы по статусу"
    );
}

/// (8b) rm → task undo: журнал goal-rm резолвится на доску ЦЕЛЕЙ (kind-маршрут,
/// не find_board задач), строка возвращается привязанной к телу со Status.
#[tokio::test]
#[serial_test::serial]
async fn undo_restores_goal_rm_journal_on_goal_board() {
    let kernel = Arc::new(Kernel::default());
    *kernel.goal_doc_exists.lock().unwrap() = true;
    *kernel.goal_keys_full.lock().unwrap() = true;
    *kernel.bodies_alive.lock().unwrap() = true;
    seeded_goal(&kernel);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    // rm: write-ahead журнал + item_remove
    goal_run(
        "uwuwu",
        GoalSub::Rm {
            id: "20261107110000-g1".into(),
        },
    )
    .await
    .expect("rm живой цели");
    let journal_name = kernel.journal_files.lock().unwrap()[0].clone();

    // undo по журналу: Common-фикс ревью — резолв доски по kind=goal-rm
    task_run(
        "uwuwu",
        TaskSub::Undo {
            journal: Some(journal_name),
            list: false,
        },
    )
    .await
    .expect("undo goal-rm журнала восстанавливает цель");

    let adds = kernel.item_adds.lock().unwrap();
    assert_eq!(adds.len(), 1, "восстановлена одна строка");
    assert!(
        !adds[0]
            .get("blockID")
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .is_empty(),
        "строка привязана к телу (blockID): {:?}",
        adds[0]
    );
    let updates = kernel.item_updates.lock().unwrap();
    let status = updates
        .iter()
        .find(|(_, k, _, _)| k == "key-status")
        .expect("Status восстановлен");
    assert_eq!(
        status.3,
        json!({ "mSelect": [{ "content": "in progress" }] }),
        "статус цели восстановлен из журнала"
    );
}

/// (10) парс: --project global у всей семьи; без --project add отказывает.
#[test]
fn goal_family_accepts_global_project_flag() {
    use super::cli::{Cli, Commands};
    use clap::Parser as _;

    let argv: Vec<String> = [
        "uwuwu-cli",
        "goal",
        "list",
        "--status",
        "todo",
        "--project",
        "foilcap",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let Commands::Goal { command, project } = Cli::try_parse_from(argv).unwrap().command else {
        panic!("expected goal");
    };
    assert_eq!(project.as_deref(), Some("foilcap"));
    let GoalSub::List { status } = command else {
        panic!("expected list");
    };
    assert_eq!(status.as_deref(), Some("todo"));

    let argv: Vec<String> = ["uwuwu-cli", "goal", "rm", "g1"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let Commands::Goal { command, project } = Cli::try_parse_from(argv).unwrap().command else {
        panic!("expected goal");
    };
    assert!(project.is_none(), "дефолт — пустой проект");
    let GoalSub::Rm { id } = command else {
        panic!("expected rm");
    };
    assert_eq!(id, "g1");
}

/// (10b) без --project — отказ до обращения к ядру.
#[tokio::test]
#[serial_test::serial]
async fn goal_commands_require_project_before_kernel() {
    let kernel = Arc::new(Kernel::default());
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = goal_run(
        "",
        GoalSub::Add {
            title: "x".into(),
            status: None,
            stdin: false,
        },
    )
    .await
    .expect_err("без --project отказ");

    assert!(err.to_string().contains("--project"), "суть отказа: {err}");
    assert!(
        kernel.created_docs.lock().unwrap().is_empty(),
        "ядро не зовётся"
    );
}
