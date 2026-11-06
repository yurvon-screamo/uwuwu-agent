//! Фоллбек task-команд на крон-доски (задача 20261104231734): резолв по
//! доскам «Задачи <proj>» → «Кроны <proj>» → чужие «Кроны %» на wiremock-ядре.
//!
//! Сценарии плана (раунд 2):
//! 1. get точного id крона — фоллбек на «Кроны uwuwu» (+ render крон-доски)
//! 2. comment кладёт коммент в «## Комменты» тела крона
//! 3. полный промах — ошибка перечисляет все обысканные доски
//! 4. кросс-проект: строка «Кроны 1xgames» находится с --project uwuwu
//! 5. точный id задачи на «Задачи» — без захода на крон-доску (фаза A, ранний возврат)
//! 6. одинаковый тайтл на двух чужих крон-досках — громкая ошибка со списком
//! 7. Ambiguous на «Задачи» + Found на кронах — громкая раскладка, не тихий выбор
//! 8. полный id крона бьёт префикс-коллизию (same-minute задача); минутный
//!    префикс, матчащий обе доски — громкая ошибка
//! 9. сбой SQL ядра — громкая ошибка, не «не найдена»

use crate::cmd::cli::TaskSub;
use crate::cmd::sy_dispatch::task_run;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NB: &str = "nb-projects";

/// Доски мок-ядра: doc → (av, av-носитель), av → строки.
const TASKS_DOC: &str = "t-doc";
const TASKS_AV: &str = "av-t";
const CRON_DOC: &str = "k-doc";
const CRON_AV: &str = "av-k";
const K1_DOC: &str = "k1-doc";
const K1_AV: &str = "av-k1";
const K2_DOC: &str = "k2-doc";
const K2_AV: &str = "av-k2";

struct Row {
    item: &'static str,
    title: &'static str,
    status: &'static str,
    doc: Option<&'static str>,
    schedule: &'static str,
}

const TASKS_ROWS: &[Row] = &[
    Row {
        item: "20261105100000-ttask1",
        title: "обычная задача",
        status: "In Progress",
        doc: Some("20261105000001-tbodyx1"),
        schedule: "",
    },
    Row {
        item: "item-alpha1",
        title: "Alpha task",
        status: "Todo",
        doc: None,
        schedule: "",
    },
    Row {
        item: "item-alpha2",
        title: "Alpha second",
        status: "Todo",
        doc: None,
        schedule: "",
    },
];

const CRON_ROWS: &[Row] = &[
    Row {
        item: "20261105100000-kcron1",
        title: "крон задач",
        status: "fired",
        doc: Some("20261105000002-kbodyx2"),
        schedule: "*/5 * * * *",
    },
    Row {
        item: "item-kcron2",
        title: "другой крон",
        status: "active",
        doc: Some("20261105000003-kbodyx3"),
        schedule: "1d",
    },
    Row {
        item: "item-kalpha",
        title: "Alpha cron",
        status: "active",
        doc: None,
        schedule: "",
    },
];

const K1_ROWS: &[Row] = &[
    Row {
        item: "20261104120000-kxgame",
        title: "чужой крон",
        status: "active",
        doc: None,
        schedule: "",
    },
    Row {
        item: "20261104120000-kxdup1",
        title: "Дубль-тайтл",
        status: "active",
        doc: None,
        schedule: "",
    },
];

const K2_ROWS: &[Row] = &[Row {
    item: "20261104130000-kodup1",
    title: "Дубль-тайтл",
    status: "active",
    doc: None,
    schedule: "",
}];

fn rows_for_av(av: &str) -> &'static [Row] {
    match av {
        TASKS_AV => TASKS_ROWS,
        CRON_AV => CRON_ROWS,
        K1_AV => K1_ROWS,
        _ => K2_ROWS,
    }
}

/// Общее состояние мок-ядра: счётчики render по av, записи item_update
/// (av, keyID, itemID), тела appendBlock, флаг сбоя SQL.
struct Kernel {
    render_calls: Mutex<HashMap<String, usize>>,
    item_updates: Mutex<Vec<(String, String, String)>>,
    appends: Mutex<Vec<String>>,
    sql_fail: AtomicBool,
    /// true → item_update отвечает success-формой с ok:false (WRITE_QUEUE_TIMEOUT):
    /// логический фейл шлюза внутри HTTP 200 без isError.
    item_update_ok_false: AtomicBool,
}

impl Kernel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            render_calls: Mutex::new(HashMap::new()),
            item_updates: Mutex::new(Vec::new()),
            appends: Mutex::new(Vec::new()),
            sql_fail: AtomicBool::new(false),
            item_update_ok_false: AtomicBool::new(false),
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
    let mut cells = vec![
        serde_json::json!({ "value": { "keyID": "key-title", "type": "block", "block": block } }),
        serde_json::json!({ "value": { "keyID": "key-status", "type": "mSelect", "mSelect": [{ "content": row.status }] } }),
    ];
    if !row.schedule.is_empty() {
        cells.push(serde_json::json!({
            "value": { "keyID": "key-schedule", "type": "text", "text": { "content": row.schedule } }
        }));
    }
    serde_json::json!({ "id": row.item, "cells": cells })
}

fn render_view(av: &str) -> serde_json::Value {
    let mut columns = vec![
        serde_json::json!({ "id": "key-title", "name": "Primary Key" }),
        serde_json::json!({ "id": "key-status", "name": "Status" }),
    ];
    if av != TASKS_AV {
        columns.push(serde_json::json!({ "id": "key-schedule", "name": "Schedule" }));
    }
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

/// id дока → doc-id-цепочка для getIDsByHPath: доски по точному пути,
/// тела кронов — как саб-доки крон-доски.
fn doc_ids_by_path(p: &str) -> Vec<&'static str> {
    match p {
        "/uwuwu/Задачи uwuwu" => vec![TASKS_DOC],
        "/uwuwu/Кроны uwuwu" => vec![CRON_DOC],
        "/1xgames/Кроны 1xgames" => vec![K1_DOC],
        "/origa/Кроны origa" => vec![K2_DOC],
        p if p.starts_with("/uwuwu/Кроны uwuwu/") => vec!["k-doc-body"],
        _ => vec![],
    }
}

async fn mount_kernel(server: &MockServer, kernel: &Arc<Kernel>) {
    // — REST: SQL-роутер (av-носители, дискавери крон-досок, саб-доки) —
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
                let data = if stmt.contains("content like 'Кроны %'") {
                    serde_json::json!([
                        { "id": K1_DOC, "hpath": "/1xgames/Кроны 1xgames" },
                        { "id": K2_DOC, "hpath": "/origa/Кроны origa" },
                        { "id": CRON_DOC, "hpath": "/uwuwu/Кроны uwuwu" },
                    ])
                } else if let Some(root) = stmt.split("root_id='").nth(1).map(|s| s.split('\'').next().unwrap_or("")) {
                    let (doc, av) = match root {
                        TASKS_DOC => (TASKS_DOC, TASKS_AV),
                        CRON_DOC => (CRON_DOC, CRON_AV),
                        K1_DOC => (K1_DOC, K1_AV),
                        K2_DOC => (K2_DOC, K2_AV),
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
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let data = body
                    .get("data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                kernel.appends.lock().unwrap().push(data);
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
            }
        })
        .mount(server)
        .await;

    // — MCP: initialize + database render/keys/item_update + block get —
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
                if !body.contains("tools/call") {
                    return ResponseTemplate::new(200)
                        .set_body_string(r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#);
                }
                let parsed: serde_json::Value = body.parse().unwrap_or_default();
                let args = parsed.pointer("/params/arguments").cloned().unwrap_or_default();
                let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("");
                if body.contains("\"database\"") && action == "render" {
                    let av = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    if !rows_for_av(av).is_empty() {
                        *kernel.render_calls.lock().unwrap().entry(av.to_string()).or_insert(0) += 1;
                        return mcp_text(render_view(av));
                    }
                    return mcp_text(serde_json::json!({ "action": "render", "data": { "view": { "columns": [], "rowCount": 0, "rows": [] } } }));
                }
                if body.contains("\"database\"") && action == "keys" {
                    let av = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let mut keys = vec![
                        serde_json::json!({ "id": "key-title", "name": "Primary Key" }),
                        serde_json::json!({ "id": "key-status", "name": "Status" }),
                    ];
                    if av == TASKS_AV {
                        keys.push(serde_json::json!({ "id": "key-branch", "name": "Branch" }));
                        keys.push(serde_json::json!({ "id": "key-pr", "name": "PR" }));
                    } else {
                        keys.push(serde_json::json!({ "id": "key-schedule", "name": "Schedule" }));
                        keys.push(serde_json::json!({ "id": "key-lastrun", "name": "Last Run" }));
                    }
                    return mcp_text(serde_json::json!({ "action": "keys", "data": { "keys": keys } }));
                }
                if body.contains("\"database\"") && action == "item_update" {
                    let av = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let item = args.get("itemID").and_then(|v| v.as_str()).unwrap_or("");
                    let key = args.get("keyID").and_then(|v| v.as_str()).unwrap_or("");
                    kernel.item_updates.lock().unwrap().push((av.to_string(), key.to_string(), item.to_string()));
                    if kernel.item_update_ok_false.load(Ordering::Relaxed) {
                        return mcp_text(serde_json::json!(
                            { "action": "item_update", "ok": false, "msg": "WRITE_QUEUE_TIMEOUT" }
                        ));
                    }
                    return mcp_text(serde_json::json!({ "action": "item_update", "data": null }));
                }
                if body.contains("\"block\"") && action == "get" {
                    let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    return ResponseTemplate::new(200).set_body_string(
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": 2,
                            "result": { "content": [{ "type": "text", "text": format!("ID: {id}\nType: NodeDocument\n") }] }
                        })
                        .to_string(),
                    );
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

/// (1) get точного id крона: фоллбек на «Кроны uwuwu» (render крон-доски случился).
#[tokio::test]
#[serial_test::serial]
async fn get_resolves_cron_row_via_fallback() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Get {
            id: "20261105100000-kcron1".into(),
            no_body: true,
        },
    )
    .await
    .expect("крон-строка резолвится через фоллбек");

    assert!(kernel.render_count(CRON_AV) >= 1, "крон-доска опрошена");
}

/// (2) comment крон-строке: секция «Комменты» создаётся, коммент кладётся в тело.
#[tokio::test]
#[serial_test::serial]
async fn comment_lands_in_cron_body_comments_section() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Comment {
            id: "item-kcron2".into(),
            reply: None,
            stdin: false,
            text: vec!["отчёт: ok".into()],
        },
    )
    .await
    .expect("коммент крон-строке уходит");

    let appends = kernel.appends_snapshot();
    assert!(
        appends.iter().any(|a| a.contains("## Комменты")),
        "секция Комменты создана: {appends:?}"
    );
    assert!(
        appends
            .iter()
            .any(|a| a.contains("**AGENTS**") && a.contains("отчёт: ok")),
        "коммент в теле крона: {appends:?}"
    );
}

/// (3) полный промах: ошибка перечисляет все обысканные доски.
#[tokio::test]
#[serial_test::serial]
async fn full_miss_lists_all_searched_boards() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Get {
            id: "совсем нет".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("полный промах — ошибка");

    let msg = err.to_string();
    for board in [
        "/uwuwu/Задачи uwuwu",
        "/uwuwu/Кроны uwuwu",
        "/1xgames/Кроны 1xgames",
        "/origa/Кроны origa",
    ] {
        assert!(
            msg.contains(board),
            "обысканная доска названа ({board}): {msg}"
        );
    }
}

/// (4) кросс-проект: строка «Кроны 1xgames» находится с --project uwuwu.
#[tokio::test]
#[serial_test::serial]
async fn cross_project_cron_row_found_from_other_project() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Get {
            id: "20261104120000-kxgame".into(),
            no_body: true,
        },
    )
    .await
    .expect("чужая крон-строка находится по контракту «всех проектов»");

    assert!(
        kernel.render_count(K1_AV) >= 1,
        "крон-доска 1xgames опрошена"
    );
}

/// (5) точный id задачи на «Задачи»: фаза A возвращает сразу, крон-доска не читается.
#[tokio::test]
#[serial_test::serial]
async fn exact_task_id_on_tasks_board_wins_without_cron_reads() {
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

    assert_eq!(
        kernel.render_count(TASKS_AV),
        1,
        "задачная доска прочитана один раз"
    );
    assert_eq!(
        kernel.render_count(CRON_AV),
        0,
        "крон-доска не читается (ранний возврат фазы A)"
    );
}

/// (6) одинаковый тайтл на двух чужих крон-досках — громкая ошибка со списком.
#[tokio::test]
#[serial_test::serial]
async fn same_title_on_two_foreign_cron_boards_is_a_loud_error() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Get {
            id: "Дубль-тайтл".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("неоднозначность между чужими досками — ошибка");

    let msg = err.to_string();
    assert!(
        msg.contains("/1xgames/Кроны 1xgames"),
        "доска 1xgames названа: {msg}"
    );
    assert!(
        msg.contains("/origa/Кроны origa"),
        "доска origa названа: {msg}"
    );
}

/// (7) Ambiguous на «Задачи» + единственный Found на кронах — громкая раскладка.
#[tokio::test]
#[serial_test::serial]
async fn ambiguous_on_tasks_plus_found_on_cron_is_loud_not_silent() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Get {
            id: "Alpha".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("неоднозначный матч не уходит в тихий выбор доски");

    let msg = err.to_string();
    assert!(
        msg.contains("Задачи uwuwu"),
        "задачная доска с Ambiguous названа: {msg}"
    );
    assert!(
        msg.contains("Кроны uwuwu"),
        "крон-доска с Found названа: {msg}"
    );
}

/// (8а) полный id крона побеждает same-minute префикс-коллизию на «Задачи».
#[tokio::test]
#[serial_test::serial]
async fn full_cron_row_id_beats_prefix_collision_on_tasks_board() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Edit {
            id: "20261105100000-kcron1".into(),
            status: Some("Review".into()),
            title: None,
            pr: None,
            branch: None,
        },
    )
    .await
    .expect("edit по полному id крона уходит на крон-строку");

    let updates = kernel.updates_snapshot();
    assert!(
        updates
            .iter()
            .any(|(av, _, item)| av == CRON_AV && item == "20261105100000-kcron1"),
        "статус выставлен крон-строке, не same-minute задаче: {updates:?}"
    );
}

/// (8а-барьер) ok:false в success-форме item_update: edit падает громко,
/// не печатает «обновлено» (класс тихого ok:false, задача 20261104072045).
#[tokio::test]
#[serial_test::serial]
async fn edit_with_ok_false_item_update_fails_loud() {
    let kernel = Kernel::new();
    kernel.item_update_ok_false.store(true, Ordering::Relaxed);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Edit {
            id: "20261105100000-kcron1".into(),
            status: Some("In Progress".into()),
            title: None,
            pr: None,
            branch: None,
        },
    )
    .await
    .expect_err("ok:false на item_update обязан падать с явной ошибкой");

    let msg = err.to_string();
    assert!(
        msg.contains("ok=false"),
        "сообщение называет форму фейла: {msg}"
    );
    assert!(
        msg.contains("WRITE_QUEUE_TIMEOUT"),
        "сообщение несёт причину шлюза: {msg}"
    );
    // мутация ушла в ядро (барьер после RPC), но клиент обязан прокричать
    let updates = kernel.updates_snapshot();
    assert!(
        !updates.is_empty(),
        "item_update дошёл до ядра — барьер на ответе, не на отправке"
    );
}

/// (8б) минутный префикс, матчащий обе доски, — громкая ошибка, не тихий wrong-target.
#[tokio::test]
#[serial_test::serial]
async fn minute_prefix_matching_both_boards_is_a_loud_error() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Get {
            id: "20261105100000".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("префикс двух досок обязан ошибиться громко");

    let msg = err.to_string();
    assert!(
        msg.contains("Задачи uwuwu"),
        "задачная доска названа: {msg}"
    );
    assert!(msg.contains("Кроны uwuwu"), "крон-доска названа: {msg}");
}

/// (9) сбой SQL ядра — громкая ошибка, не «не найдена» (таксономия ошибок).
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
            id: "что угодно".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("инфра-сбой не маскируется под «не найдена»");

    let msg = err.to_string();
    assert!(
        msg.contains("POST /api/query/sql"),
        "инфра-сбой назван: {msg}"
    );
    assert!(
        !msg.contains("не найдена"),
        "нет маскировки под промах: {msg}"
    );
}
