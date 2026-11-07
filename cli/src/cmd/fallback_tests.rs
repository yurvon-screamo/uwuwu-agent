//! Резолв task-команд только по доске задач (задача 20261106182051-o5ejlte):
//! крон-доски сняты (унификация кронов с задачами) — фоллбек «Задачи <proj>»
//! → «Кроны <proj>» → чужие «Кроны %» выпилен. Мок-ядро держит легаси
//! крон-доску намеренно: тесты доказывают, что она больше не обыскивается.
//!
//! Сценарии:
//! 1. get точного id задачи — резолв по «Задачи», крон-доска не читается
//! 2. строка только на легаси крон-доске — НЕ находится (ошибка называет «Задачи»)
//! 3. полный промах — ошибка перечисляет только доску задач
//! 4. неоднозначный префикс на «Задачи» — громкая раскладка без крон-досок
//! 5. доска «Задачи» отсутствует (крон-доска есть) — отказ «доска не найдена»
//! 6. сбой SQL ядра — громкая ошибка, не «не найдена»

use crate::cmd::cli::TaskSub;
use crate::cmd::sy_dispatch::task_run;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NB: &str = "nb-projects";

/// Доски мок-ядра: doc → (av, av-носитель). CRON_* — легаси-доска для
/// негативных тестов (в воркспейсе до сноса).
const TASKS_DOC: &str = "t-doc";
const TASKS_AV: &str = "av-t";
const CRON_DOC: &str = "k-doc";
const CRON_AV: &str = "av-k";

struct Row {
    item: &'static str,
    title: &'static str,
    status: &'static str,
    doc: Option<&'static str>,
}

const TASKS_ROWS: &[Row] = &[
    Row {
        item: "20261105100000-ttask1",
        title: "обычная задача",
        status: "In Progress",
        doc: Some("20261105000001-tbodyx1"),
    },
    Row {
        item: "item-alpha1",
        title: "Alpha task",
        status: "Todo",
        doc: None,
    },
    Row {
        item: "item-alpha2",
        title: "Alpha second",
        status: "Todo",
        doc: None,
    },
];

const CRON_ROWS: &[Row] = &[Row {
    item: "20261105100000-kcron1",
    title: "легаси крон",
    status: "fired",
    doc: Some("20261105000002-kbodyx2"),
}];

fn rows_for_av(av: &str) -> &'static [Row] {
    if av == TASKS_AV {
        TASKS_ROWS
    } else {
        CRON_ROWS
    }
}

/// Общее состояние мок-ядра: счётчики render по av, записи item_update
/// (av, keyID, itemID), тела appendBlock, флаг сбоя SQL.
struct Kernel {
    render_calls: Mutex<HashMap<String, usize>>,
    item_updates: Mutex<Vec<(String, String, String)>>,
    appends: Mutex<Vec<String>>,
    sql_fail: AtomicBool,
}

impl Kernel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            render_calls: Mutex::new(HashMap::new()),
            item_updates: Mutex::new(Vec::new()),
            appends: Mutex::new(Vec::new()),
            sql_fail: AtomicBool::new(false),
        })
    }

    fn render_count(&self, av: &str) -> usize {
        self.render_calls
            .lock()
            .unwrap()
            .get(av)
            .copied()
            .unwrap_or(0)
    }

    fn appends_snapshot(&self) -> Vec<String> {
        self.appends.lock().unwrap().clone()
    }

    fn updates_snapshot(&self) -> Vec<(String, String, String)> {
        self.item_updates.lock().unwrap().clone()
    }
}

fn render_row(row: &Row) -> serde_json::Value {
    let mut block = serde_json::json!({ "content": row.title, "refSubtype": "d" });
    if let Some(doc) = row.doc {
        block["id"] = serde_json::json!(doc);
    }
    let cells = vec![
        serde_json::json!({ "value": { "keyID": "key-title", "type": "block", "block": block } }),
        serde_json::json!({ "value": { "keyID": "key-status", "type": "mSelect", "mSelect": [{ "content": row.status }] } }),
    ];
    serde_json::json!({ "id": row.item, "cells": cells })
}

fn render_view(av: &str) -> serde_json::Value {
    let columns = vec![
        serde_json::json!({ "id": "key-title", "name": "Primary Key" }),
        serde_json::json!({ "id": "key-status", "name": "Status" }),
    ];
    let rows: Vec<serde_json::Value> = rows_for_av(av).iter().map(render_row).collect();
    let count = rows.len();
    serde_json::json!({ "action": "render", "data": { "view": { "columns": columns, "rowCount": count, "rows": rows } } })
}

fn mcp_text(payload: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": { "content": [{ "type": "text", "text": payload.to_string() }] }
        })
        .to_string(),
    )
}

/// id дока → doc-id-цепочка для getIDsByHPath: обе доски существуют
/// (легаси-воркспейс до сноса крон-досок).
fn doc_ids_by_path(p: &str) -> Vec<&'static str> {
    match p {
        "/uwuwu/Задачи uwuwu" => vec![TASKS_DOC],
        "/uwuwu/Кроны uwuwu" => vec![CRON_DOC],
        _ => vec![],
    }
}

async fn mount_kernel(server: &MockServer, kernel: &Arc<Kernel>) {
    // — REST: SQL-роутер (av-носители) —
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                if kernel.sql_fail.load(Ordering::SeqCst) {
                    return ResponseTemplate::new(200)
                        .set_body_string(r#"{"code":500,"msg":"boom"}"#);
                }
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
                let data = if let Some(root) = stmt
                    .split("root_id='")
                    .nth(1)
                    .map(|s| s.split('\'').next().unwrap_or(""))
                {
                    let (doc, av) = match root {
                        TASKS_DOC => (TASKS_DOC, TASKS_AV),
                        CRON_DOC => (CRON_DOC, CRON_AV),
                        _ => ("", ""),
                    };
                    if doc.is_empty() {
                        serde_json::json!([])
                    } else {
                        serde_json::json!([{
                            "id": format!("block-{doc}"),
                            "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{av}\"></div>")
                        }])
                    }
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200).set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
            }
        })
        .mount(server)
        .await;

    // — REST: ноутбуки и hpath-выборки —
    Mock::given(method("POST"))
        .and(path("/api/notebook/lsNotebooks"))
        .respond_with(move |_req: &Request| {
            ResponseTemplate::new(200).set_body_string(
                serde_json::json!({ "code": 0, "data": { "notebooks": [ { "id": NB, "name": "Projects", "closed": false } ] } })
                    .to_string(),
            )
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/filetree/getIDsByHPath"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let p = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let ids: Vec<&str> = doc_ids_by_path(p);
            ResponseTemplate::new(200)
                .set_body_string(serde_json::json!({ "code": 0, "data": ids }).to_string())
        })
        .mount(server)
        .await;

    // — REST: экспорт тел и append комментов —
    Mock::given(method("POST"))
        .and(path("/api/export/exportMdContent"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
            ResponseTemplate::new(200).set_body_string(
                serde_json::json!({ "code": 0, "data": { "content": format!("# {id}\n") } })
                    .to_string(),
            )
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/block/appendBlock"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                let body = String::from_utf8_lossy(&req.body).to_string();
                kernel.appends.lock().unwrap().push(body);
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({ "code": 0, "data": { "doNotify": true } }).to_string(),
                )
            }
        })
        .mount(server)
        .await;

    // — MCP: render досок —
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
                        .append_header("mcp-session-id", "sess-fallback");
                }
                if body.contains("notifications/initialized") {
                    return ResponseTemplate::new(200);
                }
                let parsed: serde_json::Value = body.parse().unwrap_or_default();
                let args = parsed.pointer("/params/arguments").cloned().unwrap_or_default();
                let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("");
                if action == "render" {
                    let av = args.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    *kernel.render_calls.lock().unwrap().entry(av.clone()).or_insert(0) += 1;
                    return mcp_text(render_view(&av));
                }
                mcp_text(serde_json::json!({ "action": "unknown", "data": null }))
            }
        })
        .mount(server)
        .await;
}

fn set_env(server: &MockServer) {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
}

/// (1) get по точному id задачи — резолв по «Задачи»; крон-доска не читается.
#[tokio::test]
#[serial_test::serial]
async fn get_resolves_on_tasks_board_without_cron_reads() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Get {
            id: "20261105100000-ttask1".into(),
            no_body: true,
        },
    )
    .await
    .expect("задача резолвится штатно");

    assert!(
        kernel.render_count(TASKS_AV) > 0,
        "доска задач прочитана"
    );
    assert_eq!(
        kernel.render_count(CRON_AV),
        0,
        "легаси крон-доска не обыскивается"
    );
}

/// (2) Строка только на легаси крон-доске — НЕ находится: ошибка называет
/// доску задач (крон-строки мертвы вместе с контуром).
#[tokio::test]
#[serial_test::serial]
async fn cron_board_row_is_not_resolved_anymore() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Get {
            id: "20261105100000-kcron1".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("крон-строка больше не резолвится");

    let msg = err.to_string();
    assert!(msg.contains("не найдена"), "честный промах: {msg}");
    assert!(
        msg.contains("Задачи uwuwu"),
        "обыскана только доска задач: {msg}"
    );
    assert_eq!(
        kernel.appends_snapshot().len(),
        0,
        "никаких записей по промаху"
    );
}

/// (3) Полный промах — ошибка перечисляет только доску задач.
#[tokio::test]
#[serial_test::serial]
async fn full_miss_lists_only_tasks_board() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Get {
            id: "20261199999999-none".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("полный промах");

    let msg = err.to_string();
    assert!(msg.contains("Задачи uwuwu"), "обысканная доска в ошибке: {msg}");
    assert!(!msg.contains("Кроны"), "крон-доски в обыске не участвуют: {msg}");
}

/// (4) Неоднозначный префикс на «Задачи» — громкая раскладка; крон-доски
/// в раскладке не фигурируют.
#[tokio::test]
#[serial_test::serial]
async fn ambiguous_prefix_is_loud_without_cron_boards() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Get {
            id: "item-alpha".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("двусмысленный префикс");

    let msg = err.to_string();
    assert!(msg.contains("неоднозначно"), "раскладка: {msg}");
    assert!(!msg.contains("Кроны"), "крон-досок в обыске нет: {msg}");
}

/// (5) Доска «Задачи» отсутствует (легаси крон-доска есть) — отказ
/// «доска не найдена», фоллбека на кроны нет.
#[tokio::test]
#[serial_test::serial]
async fn missing_tasks_board_bails_even_with_cron_board_present() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run(
        "origa",
        TaskSub::Get {
            id: "20261105100000-ttask1".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("доски задач origa нет");

    let msg = err.to_string();
    assert!(
        msg.contains("Задачи origa") && msg.contains("не найдена"),
        "отказ называет отсутствующую доску: {msg}"
    );
    assert_eq!(
        kernel.render_count(CRON_AV),
        0,
        "крон-доска не подменяет доску задач"
    );
}

/// (6) Сбой SQL ядра — громкая ошибка, не «не найдена».
#[tokio::test]
#[serial_test::serial]
async fn kernel_sql_failure_is_loud_not_reported_as_not_found() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);
    kernel.sql_fail.store(true, Ordering::SeqCst);

    let err = task_run(
        "uwuwu",
        TaskSub::Get {
            id: "20261105100000-ttask1".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("сбой ядра — отказ");

    assert!(
        !err.to_string().contains("не найдена"),
        "инфра-сбой не маскируется промахом: {err}"
    );
}
