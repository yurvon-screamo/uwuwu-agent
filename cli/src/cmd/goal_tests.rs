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

use crate::cmd::cli::GoalSub;
use crate::cmd::sy_dispatch::{format_goal_table, goal_run};
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

/// Строка render доски целей в мок-ядре.
struct GoalRow {
    item: String,
    title: String,
    status: String,
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
                    { "value": { "keyID": "key-title", "type": "block", "block": { "content": row.title, "refSubtype": "d", "id": format!("body-{}", row.item) } } },
                    { "value": { "keyID": "key-status", "type": "mSelect", "mSelect": [{ "content": row.status }] } }
                ]
            })
        })
        .collect();
    let count = rows.len();
    json!({ "action": "render", "data": { "view": { "columns": columns, "rowCount": count, "rows": rows } } })
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

async fn mount_kernel(server: &MockServer, kernel: &Arc<Kernel>) {
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
            let data = if let Some(root) =
                stmt.split("root_id='").nth(1).map(|s| s.split('\'').next().unwrap_or(""))
            {
                if root == GOAL_DOC {
                    json!([{
                        "id": format!("block-{root}"),
                        "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{GOAL_AV}\"></div>")
                    }])
                } else {
                    json!([])
                }
            } else {
                json!([])
            };
            ResponseTemplate::new(200)
                .set_body_string(json!({ "code": 0, "data": data }).to_string())
        })
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
            move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let p = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let ids: Vec<&str> = match p {
                    // тела целей по hpath не существуют (нет сирот); доска и её
                    // родительский док «uwuwu» — по одному id (board_doc_chain
                    // идёт по префиксам hpath)
                    "/uwuwu" => vec!["uwuwu-root"],
                    "/uwuwu/Цели uwuwu" if *kernel.goal_doc_exists.lock().unwrap() => {
                        vec![GOAL_DOC]
                    }
                    _ => vec![],
                };
                ResponseTemplate::new(200)
                    .set_body_string(json!({ "code": 0, "data": ids }).to_string())
            }
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/filetree/createDocWithMd"))
        .respond_with({
            let kernel = kernel.clone();
            let next_doc_id = Mutex::new(0usize);
            move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                kernel.created_docs.lock().unwrap().push(
                    body.get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                );
                let id = format!("20261107120000-body{:02}", next_doc_id.lock().unwrap());
                *next_doc_id.lock().unwrap() += 1;
                ResponseTemplate::new(200)
                    .set_body_string(json!({ "code": 0, "data": id }).to_string())
            }
        })
        .mount(server)
        .await;

    // putFile: write-ahead undo-журнал (goal rm)
    Mock::given(method("POST"))
        .and(path("/api/file/putFile"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                let body = String::from_utf8_lossy(&req.body).to_string();
                if let Some(name) = body
                    .split("filename=\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                {
                    kernel.journal_files.lock().unwrap().push(name.to_string());
                }
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"","data":null}"#)
            }
        })
        .mount(server)
        .await;

    // renameDoc: меняет Title-клетку строки сразу (синк без лага) или ничего
    Mock::given(method("POST"))
        .and(path("/api/filetree/renameDoc"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let new_title = body
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                if *kernel.rename_applies.lock().unwrap() {
                    for row in kernel.goal_rows.lock().unwrap().iter_mut() {
                        // renameDoc пути несёт docId тела: body-<itemID> в моке
                        // не отличим по id-цепочке — мок-ядро хранит одну строку
                        row.title = new_title.clone();
                    }
                }
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"","data":null}"#)
            }
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/filetree/removeDocByID"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                kernel.removed_docs.lock().unwrap().push(
                    body.get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string(),
                );
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"","data":null}"#)
            }
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
                let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("");
                let av = args.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();

                if action == "keys" {
                    let full = *kernel.goal_keys_full.lock().unwrap();
                    let keys = if full { goal_keys() } else { template_keys() };
                    return mcp_text(json!({ "action": "keys", "data": { "keys": keys } }));
                }
                if action == "item_add" {
                    kernel.item_adds.lock().unwrap().push(args.clone());
                    let item = "20261107120000-item01".to_string();
                    if *kernel.render_confirms_insert.lock().unwrap() {
                        kernel.goal_rows.lock().unwrap().push(GoalRow {
                            item: item.clone(),
                            title: "новая цель".into(),
                            status: "todo".into(),
                        });
                    }
                    return mcp_text(json!({ "action": "item_add", "data": { "itemID": item } }));
                }
                if action == "item_update" {
                    let key = args.get("keyID").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let item = args.get("itemID").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let value = args.get("value").cloned().unwrap_or(json!(null));
                    // mSelect-запись статуса отражаем в render-состоянии
                    let new_status = value
                        .pointer("/mSelect/0/content")
                        .and_then(|c| c.as_str())
                        .map(String::from);
                    if let Some(content) = &new_status {
                        for row in kernel.goal_rows.lock().unwrap().iter_mut() {
                            if row.item == item {
                                row.status = content.clone();
                            }
                        }
                    }
                    kernel.item_updates.lock().unwrap().push((av.clone(), key, item, value));
                    return mcp_text(json!({ "action": "item_update", "data": null }));
                }
                if action == "item_remove" {
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
                    return mcp_text(json!({ "action": "item_remove", "data": null }));
                }
                if action == "get" {
                    // block get (doc_block_exists): привязка тела по сценарию
                    let alive = *kernel.bodies_alive.lock().unwrap();
                    let text = if alive {
                        format!("ID: {av}\nType: NodeDocument\nHPath: /uwuwu/Цели uwuwu/тело")
                    } else {
                        "not found".to_string()
                    };
                    return ResponseTemplate::new(200).set_body_string(
                        json!({
                            "jsonrpc": "2.0", "id": 2,
                            "result": { "content": [{ "type": "text", "text": text }] }
                        })
                        .to_string(),
                    );
                }
                if action == "render" {
                    return mcp_text(render_view(&kernel));
                }
                mcp_text(json!({ "action": "unknown", "data": null }))
            }
        })
        .mount(server)
        .await;
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
        vec!["body-20261107110000-g1".to_string()],
        "привязанное тело в трэш (removeDocByID)"
    );
    let journals = kernel.journal_files.lock().unwrap();
    assert_eq!(journals.len(), 1, "write-ahead журнал один");
    assert!(
        journals[0].ends_with("-uwuwu-goal-rm.json"),
        "журнал kind=goal-rm: {:?}",
        journals
    );
}

/// (9) list: таблица несёт статус/заголовок, фильтр --status, маркер (!).
#[test]
fn list_table_carries_status_title_and_dead_binding_marker() {
    let mut rows = vec![
        SyTask {
            item_id: "20261107110000-g1".into(),
            title: "Отказ от SiYuan в пользу самописного движка".into(),
            status: "in progress".into(),
            branch: String::new(),
            pr: String::new(),
            schedule: String::new(),
            last_run: String::new(),
            dev_session: String::new(),
            body_doc: Some("body-20261107110000-g1".into()),
        },
        SyTask {
            item_id: "20261107110000-g2".into(),
            title: "Монетизация".into(),
            status: "done".into(),
            branch: String::new(),
            pr: String::new(),
            schedule: String::new(),
            last_run: String::new(),
            dev_session: String::new(),
            body_doc: None,
        },
    ];
    let mut dead = HashSet::new();
    dead.insert("body-20261107110000-g1".to_string());

    let table = format_goal_table(&rows, &dead);
    assert!(table.contains("СТАТУС"), "заголовок таблицы: {table}");
    assert!(table.contains("Монетизация"), "цель видна: {table}");
    assert!(table.contains("in progress"), "статус виден: {table}");
    assert!(table.contains("(!)"), "мёртвая привязка помечена: {table}");

    // done-цель сортируется после in progress (группировка по статусу)
    let in_progress_pos = table.find("Отказ от SiYuan").expect("g1 в таблице");
    let done_pos = table.find("Монетизация").expect("g2 в таблице");
    assert!(
        in_progress_pos < done_pos,
        "строки сгруппированы по статусу"
    );

    rows.remove(1);
    let filtered = format_goal_table(&rows, &dead);
    assert!(
        !filtered.contains("Монетизация"),
        "фильтр применён до таблицы"
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
