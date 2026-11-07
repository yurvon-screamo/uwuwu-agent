//! Write-path тесты task create / body / comment на stateful wiremock-ядре
//! (срез покрытия 20261105220435). Create — через task_run (orphan-rebind,
//! verify-ретрай, дубль-тайтл в render, missing после ретрая); body — полный
//! --replace (overwrite in place, docId стабилен) и строка вовсе без тела;
//! comment — comment_with_ts (секция, маркер, потерянный ответ, сбой, reply).
//!
//! Мок имитирует ядро честно: render «лагает» (строка видна после N вставок),
//! appendBlock может отвечать 500, применяя или теряя блок — ассерты проверяют
//! исходы (что куда записалось, что создано/не создано), не внутренние вызовы.

use crate::cmd::cli::TaskSub;
use crate::cmd::sy_dispatch::task_run;
use crate::siyuan::SiYuan;
use crate::siyuan_task::{
    comment_with_ts, create_with_pacing, CommentDraft, RowInsertPacing, SyBoard, SyTask,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NB: &str = "nb-1";
const BOARD_HPATH: &str = "/uwuwu/Задачи uwuwu";
const BOARD_DOC: &str = "board-doc";
const AV: &str = "av-1";
const CARRIER: &str = "carrier-block";
const TITLE_KEY: &str = "key-title";
const STATUS_KEY: &str = "key-status";
const ROW: &str = "item-row";
const BOUND_DOC: &str = "20261106090000-boundd1";
const ORPHAN_DOC: &str = "20261105090000-orpha1";
const OTHER_ROW: &str = "item-other";
const TASK_TITLE: &str = "таск";
const NEW_TITLE: &str = "новая задача";

fn set_env(server: &MockServer) {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
}

fn kernel_rest_response(data: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(json!({ "code": 0, "data": data }).to_string())
}

/// MCP-ответ, где content[0].text — сериализованный payload (database tools).
fn mcp_payload(payload: Value) -> ResponseTemplate {
    mcp_raw_text(payload.to_string())
}

/// MCP-ответ, где content[0].text — сырой текст (block get / get_children).
fn mcp_raw_text(text: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(
        json!({
            "jsonrpc": "2.0", "id": 2,
            "result": { "content": [{ "type": "text", "text": text }] }
        })
        .to_string(),
    )
}

fn mcp_initialize() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_string(
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
        )
        .append_header("mcp-session-id", "sess-1")
}

fn mcp_initialized() -> ResponseTemplate {
    ResponseTemplate::new(200)
}

fn row_json(item: &str, title: &str, block_id: Option<&str>) -> Value {
    let mut block = json!({ "content": title, "refSubtype": "d" });
    if let Some(id) = block_id {
        block["id"] = json!(id);
    }
    json!({
        "id": item,
        "cells": [{ "value": { "keyID": TITLE_KEY, "type": "block", "block": block } }]
    })
}

/// Страница render: пустая страница (page ≥ 2) терминирует пагинацию.
fn render_payload(page: u64, rows: Vec<Value>) -> Value {
    let rows = if page <= 1 { rows } else { Vec::new() };
    json!({ "action": "render", "data": { "view": {
        "columns": [{ "id": TITLE_KEY, "name": "Primary Key" }],
        "rows": rows,
        "rowCount": rows.len() } } })
}

async fn mount_kernel_shell(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/notebook/lsNotebooks"))
        .respond_with(|_req: &Request| {
            kernel_rest_response(json!({ "notebooks": [{ "id": NB, "name": "Projects" }] }))
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
            let data = if stmt.contains("type='av'") {
                json!([{ "id": CARRIER,
                    "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{AV}\"></div>") }])
            } else {
                json!([])
            };
            kernel_rest_response(data)
        })
        .mount(server)
        .await;
}

/// docId по hpath: доска всегда, тело — по сценарию (сирота/нет тела).
async fn mount_hpath_lookup(server: &MockServer, body_title_ids: Arc<Mutex<Vec<String>>>) {
    Mock::given(method("POST"))
        .and(path("/api/filetree/getIDsByHPath"))
        .respond_with(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let data = if hpath == BOARD_HPATH {
                json!([BOARD_DOC])
            } else {
                json!(body_title_ids.lock().unwrap().clone())
            };
            kernel_rest_response(data)
        })
        .mount(server)
        .await;
}

// ——— create: orphan-rebind, verify-ретрай, дубль в render, missing ———

struct CreateKernel {
    /// docId по hpath тела; непустой = тело-сирота существует.
    body_title_ids: Arc<Mutex<Vec<String>>>,
    /// itemID-ответы item_add по порядку; последний повторяется.
    item_add_replies: Mutex<Vec<String>>,
    /// render показывает вставленную строку, когда item_adds >= порога.
    row_visible_after_adds: AtomicUsize,
    /// после первой вставки render показывает чужую строку с тем же тайтлом.
    other_row_after_adds: bool,
    creates: AtomicUsize,
    item_adds: AtomicUsize,
    item_add_bodies: Mutex<Vec<String>>,
    status_updates: AtomicUsize,
    /// тела appendBlock (тег типа — единственная запись после подтверждения).
    appends: Mutex<Vec<String>>,
}

impl CreateKernel {
    /// `replies` — ответы item_add по порядку (последний повторяется);
    /// `row_visible_after_adds` — лаг render (usize::MAX = строка не видна);
    /// `orphan` — тело-сирота по заголовку уже существует.
    fn new(
        replies: &[&str],
        row_visible_after_adds: usize,
        other_row_after_adds: bool,
        orphan: bool,
    ) -> Self {
        let body_title_ids = if orphan {
            vec![ORPHAN_DOC.to_string()]
        } else {
            Vec::new()
        };
        Self {
            body_title_ids: Arc::new(Mutex::new(body_title_ids)),
            item_add_replies: Mutex::new(replies.iter().map(|s| s.to_string()).collect()),
            row_visible_after_adds: AtomicUsize::new(row_visible_after_adds),
            other_row_after_adds,
            creates: AtomicUsize::new(0),
            item_adds: AtomicUsize::new(0),
            item_add_bodies: Mutex::new(Vec::new()),
            status_updates: AtomicUsize::new(0),
            appends: Mutex::new(Vec::new()),
        }
    }

    fn next_item(&self) -> String {
        let mut replies = self.item_add_replies.lock().unwrap();
        if replies.len() > 1 {
            replies.remove(0)
        } else {
            replies.last().cloned().unwrap_or_default()
        }
    }

    fn current_item(&self) -> String {
        self.item_add_replies
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

async fn create_flow_mock(kernel: &Arc<CreateKernel>) -> MockServer {
    let server = MockServer::start().await;
    mount_kernel_shell(&server).await;
    mount_hpath_lookup(&server, Arc::clone(&kernel.body_title_ids)).await;

    Mock::given(method("POST"))
        .and(path("/api/filetree/createDocWithMd"))
        .respond_with({
            let kernel = Arc::clone(kernel);
            move |_req: &Request| {
                kernel.creates.fetch_add(1, Ordering::SeqCst);
                kernel_rest_response(json!("doc-new"))
            }
        })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/block/appendBlock"))
        .respond_with({
            let kernel = Arc::clone(kernel);
            move |req: &Request| {
                kernel
                    .appends
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req.body).to_string());
                kernel_rest_response(json!(null))
            }
        })
        .mount(&server)
        .await;

    let mcp = Arc::clone(kernel);
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(move |req: &Request| {
            let body = String::from_utf8_lossy(&req.body);
            if body.contains("\"initialize\"") {
                return mcp_initialize();
            }
            if body.contains("notifications/") {
                return mcp_initialized();
            }
            if !body.contains("tools/call") {
                return mcp_payload(json!({ "action": "unexpected", "data": null }));
            }
            if body.contains("\"item_add\"") {
                mcp.item_adds.fetch_add(1, Ordering::SeqCst);
                mcp.item_add_bodies.lock().unwrap().push(body.to_string());
                let item = mcp.next_item();
                return mcp_payload(json!({ "action": "item_add", "data": { "itemID": item } }));
            }
            if body.contains("\"item_update\"") {
                mcp.status_updates.fetch_add(1, Ordering::SeqCst);
                return mcp_payload(json!({ "action": "item_update", "data": null }));
            }
            if body.contains("\"keys\"") {
                return mcp_payload(json!({ "action": "keys",
                    "data": { "keys": [{ "id": STATUS_KEY, "name": "Status" }] } }));
            }
            if body.contains("\"render\"") {
                let args: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let page = args
                    .pointer("/params/arguments/page")
                    .and_then(|p| p.as_u64())
                    .unwrap_or(1);
                let adds = mcp.item_adds.load(Ordering::SeqCst);
                let mut rows: Vec<Value> = Vec::new();
                if mcp.other_row_after_adds && adds >= 1 {
                    rows.push(row_json(OTHER_ROW, NEW_TITLE, Some(ORPHAN_DOC)));
                }
                if adds >= mcp.row_visible_after_adds.load(Ordering::SeqCst) {
                    let block = if mcp.body_title_ids.lock().unwrap().is_empty() {
                        "doc-new"
                    } else {
                        ORPHAN_DOC
                    };
                    rows.push(row_json(&mcp.current_item(), NEW_TITLE, Some(block)));
                }
                return mcp_payload(render_payload(page, rows));
            }
            mcp_payload(json!({ "action": "unknown", "data": null }))
        })
        .mount(&server)
        .await;
    server
}

async fn create_via_cli(server: &MockServer) -> anyhow::Result<()> {
    set_env(server);
    let sy = SiYuan::from_env()?;
    create_with_pacing(
        &sy,
        "uwuwu",
        NEW_TITLE,
        "dev",
        "",
        &RowInsertPacing::instant(),
    )
    .await
    .map(|_| ())
}

/// Идемпотентный ретрай (ADR-009 «достроить сироту»): строка потерялась,
/// тело-сирота цело → строка привязывается к нему, новый док НЕ создаётся.
#[tokio::test]
#[serial_test::serial]
async fn create_rebinds_lost_row_to_orphan_body_without_new_doc() {
    let kernel = Arc::new(CreateKernel::new(&[ROW], 1, false, true));
    let server = create_flow_mock(&kernel).await;

    create_via_cli(&server)
        .await
        .expect("ретрай перепривязывает сироту");

    assert_eq!(
        kernel.creates.load(Ordering::SeqCst),
        0,
        "новый док не минтится"
    );
    assert_eq!(
        kernel.item_adds.load(Ordering::SeqCst),
        1,
        "одна вставка строки"
    );
    let adds = kernel.item_add_bodies.lock().unwrap();
    assert!(
        adds[0].contains(ORPHAN_DOC),
        "item_add привязан к docId сироты: {adds:?}"
    );
    let appends = kernel.appends.lock().unwrap();
    assert_eq!(
        appends.len(),
        1,
        "после подтверждения пишется только тег типа"
    );
    assert!(appends[0].contains("#dev"), "тег типа: {appends:?}");
}

/// Verify-вставки: success item_add не виден в render (лаг ядра) → ретрай ×1,
/// второй item_add подтверждается, тег пишется один раз.
#[tokio::test]
#[serial_test::serial]
async fn create_retries_row_insert_once_when_render_lags() {
    let kernel = Arc::new(CreateKernel::new(&["item-a", ROW], 2, false, false));
    let server = create_flow_mock(&kernel).await;

    create_via_cli(&server)
        .await
        .expect("ретрай доводит строку до render");

    assert_eq!(
        kernel.creates.load(Ordering::SeqCst),
        1,
        "тело создаётся один раз"
    );
    assert_eq!(
        kernel.item_adds.load(Ordering::SeqCst),
        2,
        "item_add + ретрай"
    );
    assert_eq!(
        kernel.status_updates.load(Ordering::SeqCst),
        2,
        "Draft проставляется каждой вставке"
    );
    let appends = kernel.appends.lock().unwrap();
    assert_eq!(appends.len(), 1, "тег типа один, без дублей после ретрая");
}

/// Дедуп по факту render: в ленте появилась чужая строка с тем же тайтлом →
/// громкий отказ, вторую строку не минтим, тег не пишется.
#[tokio::test]
#[serial_test::serial]
async fn create_bails_when_render_shows_title_with_other_item() {
    let kernel = Arc::new(CreateKernel::new(&["item-a"], usize::MAX, true, false));
    let server = create_flow_mock(&kernel).await;

    let err = create_via_cli(&server)
        .await
        .expect_err("чужая строка с тем же тайтлом — отказ");

    assert!(
        err.to_string().contains("вторую строку не создаю"),
        "суть отказа: {err}"
    );
    assert_eq!(kernel.item_adds.load(Ordering::SeqCst), 1, "ретрая не было");
    assert!(
        kernel.appends.lock().unwrap().is_empty(),
        "тег не пишется в неустановленное тело"
    );
}

/// Строка не видна в render и после ретрая → громкая ошибка с item и телом.
#[tokio::test]
#[serial_test::serial]
async fn create_bails_loudly_when_row_missing_after_retry() {
    let kernel = Arc::new(CreateKernel::new(
        &["item-a", "item-b"],
        usize::MAX,
        false,
        false,
    ));
    let server = create_flow_mock(&kernel).await;

    let err = create_via_cli(&server)
        .await
        .expect_err("строка так и не видна — громкая ошибка");

    assert!(
        err.to_string().contains("после вставки и ретрая"),
        "суть отказа: {err}"
    );
    assert_eq!(
        kernel.item_adds.load(Ordering::SeqCst),
        2,
        "ровно ретрай ×1"
    );
    assert!(
        kernel.appends.lock().unwrap().is_empty(),
        "тег не пишется в неподтверждённую задачу"
    );
}

// ——— body: полный --replace и detached-строка ———

const OLD_BLOCK: &str = "20261106090000-blk0001";

struct BodyKernel {
    /// render-строка несёт привязку (block.id).
    bound: bool,
    /// последовательные ответы get_children; последний повторяется.
    get_children_seq: Mutex<Vec<String>>,
    export_md: String,
    creates: AtomicUsize,
    create_calls: Mutex<Vec<(String, String)>>,
    deletes: Mutex<Vec<String>>,
    appends: Mutex<Vec<String>>,
}

async fn body_flow_mock(kernel: &Arc<BodyKernel>) -> MockServer {
    let server = MockServer::start().await;
    mount_kernel_shell(&server).await;
    mount_hpath_lookup(&server, Arc::new(Mutex::new(Vec::new()))).await;

    Mock::given(method("POST"))
        .and(path("/api/export/exportMdContent"))
        .respond_with({
            let kernel = Arc::clone(kernel);
            move |_req: &Request| {
                kernel_rest_response(json!({ "content": kernel.export_md.clone() }))
            }
        })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/filetree/createDocWithMd"))
        .respond_with({
            let kernel = Arc::clone(kernel);
            move |req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                kernel.create_calls.lock().unwrap().push((
                    body.get("path")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .into(),
                    body.get("markdown")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .into(),
                ));
                kernel.creates.fetch_add(1, Ordering::SeqCst);
                kernel_rest_response(json!("doc-new"))
            }
        })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/block/deleteBlock"))
        .respond_with({
            let kernel = Arc::clone(kernel);
            move |req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                kernel
                    .deletes
                    .lock()
                    .unwrap()
                    .push(body.get("id").and_then(|i| i.as_str()).unwrap_or("").into());
                kernel_rest_response(json!(null))
            }
        })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/block/appendBlock"))
        .respond_with({
            let kernel = Arc::clone(kernel);
            move |req: &Request| {
                kernel
                    .appends
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req.body).to_string());
                kernel_rest_response(json!(null))
            }
        })
        .mount(&server)
        .await;

    let mcp = Arc::clone(kernel);
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(move |req: &Request| {
            let body = String::from_utf8_lossy(&req.body);
            if body.contains("\"initialize\"") {
                return mcp_initialize();
            }
            if body.contains("notifications/") {
                return mcp_initialized();
            }
            if !body.contains("tools/call") {
                return mcp_payload(json!({ "action": "unexpected", "data": null }));
            }
            if body.contains("\"get_children\"") {
                let mut seq = mcp.get_children_seq.lock().unwrap();
                let text = if seq.len() > 1 {
                    seq.remove(0)
                } else {
                    seq[0].clone()
                };
                return mcp_raw_text(text);
            }
            if body.contains("\"block\"") && body.contains("\"get\"") {
                return mcp_raw_text(format!(
                    "ID: {BOUND_DOC}\nType: NodeDocument\nHPath: {BOARD_HPATH}/{TASK_TITLE}"
                ));
            }
            if body.contains("\"render\"") {
                let args: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let page = args
                    .pointer("/params/arguments/page")
                    .and_then(|p| p.as_u64())
                    .unwrap_or(1);
                let bound = if mcp.bound { Some(BOUND_DOC) } else { None };
                return mcp_payload(render_payload(
                    page,
                    vec![row_json("item-task", TASK_TITLE, bound)],
                ));
            }
            mcp_payload(json!({ "action": "unknown", "data": null }))
        })
        .mount(&server)
        .await;
    server
}

async fn body_via_cli(server: &MockServer, replace: bool, content: &str) -> anyhow::Result<()> {
    set_env(server);
    let md = tempfile::NamedTempFile::new()?;
    std::fs::write(md.path(), content)?;
    task_run(
        "uwuwu",
        TaskSub::Body {
            id: TASK_TITLE.into(),
            stdin: false,
            md: Some(md.path().to_string_lossy().into_owned()),
            replace,
            replace_section: None,
            // тест про docId-стабильность overwrite, не про freshness-гард (#119):
            // force возвращает исходную семантику «replace проходит» без снапшота
            base: None,
            force: replace,
            attach: Vec::new(),
        },
    )
    .await
}

/// Полный --replace: контент перезаписывается на месте (волны удаления →
/// append), createDocWithMd не зовётся — docId тела стабилен.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_overwrites_doc_in_place_without_recreating() {
    let kernel = Arc::new(BodyKernel {
        bound: true,
        get_children_seq: Mutex::new(vec![
            format!("- [p] старый блок ({OLD_BLOCK})"),
            String::new(),
        ]),
        export_md: "старый контент\n".into(),
        creates: AtomicUsize::new(0),
        create_calls: Mutex::new(Vec::new()),
        deletes: Mutex::new(Vec::new()),
        appends: Mutex::new(Vec::new()),
    });
    let server = body_flow_mock(&kernel).await;

    body_via_cli(&server, true, "новое тело\n")
        .await
        .expect("replace проходит");

    assert_eq!(
        kernel.creates.load(Ordering::SeqCst),
        0,
        "док не пересоздаётся"
    );
    let deletes = kernel.deletes.lock().unwrap();
    assert_eq!(*deletes, vec![OLD_BLOCK.to_string()], "старый блок удалён");
    let appends = kernel.appends.lock().unwrap();
    assert_eq!(appends.len(), 1, "новый контент одним append");
    assert!(
        appends[0].contains("новое тело"),
        "контент записан: {appends:?}"
    );
    assert!(appends[0].contains(BOUND_DOC), "append в привязанный док");
}

/// Строка вовсе без тела: тело создаётся по заголовку с контентом,
/// привязка строки (itemID) не трогается.
#[tokio::test]
#[serial_test::serial]
async fn body_on_row_without_body_creates_doc_by_title_without_binding() {
    let kernel = Arc::new(BodyKernel {
        bound: false,
        get_children_seq: Mutex::new(vec![String::new()]),
        export_md: String::new(),
        creates: AtomicUsize::new(0),
        create_calls: Mutex::new(Vec::new()),
        deletes: Mutex::new(Vec::new()),
        appends: Mutex::new(Vec::new()),
    });
    let server = body_flow_mock(&kernel).await;

    body_via_cli(&server, false, "контент нового тела\n")
        .await
        .expect("body на detached-строке создаёт тело");

    assert_eq!(kernel.creates.load(Ordering::SeqCst), 1, "один createDoc");
    let calls = kernel.create_calls.lock().unwrap();
    assert_eq!(
        calls[0].0,
        format!("{BOARD_HPATH}/{TASK_TITLE}"),
        "тело создаётся по заголовку строки"
    );
    assert!(
        calls[0].1.contains("контент нового тела"),
        "контент в markdown"
    );
    assert!(
        kernel.appends.lock().unwrap().is_empty(),
        "запись идёт createDocWithMd, не appendBlock"
    );
}

// ——— comment: секция, маркер, потерянный ответ, сбой, reply ———

const COMMENT_TS: &str = "2026-11-06 12:00";

struct CommentKernel {
    /// применённые блоки дока (state экспорта).
    blocks: Mutex<Vec<String>>,
    /// appendBlock применяется и отвечает 200.
    apply_on_append: AtomicBool,
    /// appendBlock применяется, но отвечает 500 (потерянный ответ).
    lost_response: AtomicBool,
    /// тела всех попыток append.
    appends: Mutex<Vec<String>>,
}

async fn comment_flow_mock(kernel: &Arc<CommentKernel>) -> MockServer {
    let server = MockServer::start().await;
    mount_kernel_shell(&server).await;
    mount_hpath_lookup(&server, Arc::new(Mutex::new(Vec::new()))).await;

    Mock::given(method("POST"))
        .and(path("/api/export/exportMdContent"))
        .respond_with({
            let kernel = Arc::clone(kernel);
            move |_req: &Request| {
                let md = kernel.blocks.lock().unwrap().join("\n");
                kernel_rest_response(json!({ "content": md }))
            }
        })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/block/appendBlock"))
        .respond_with({
            let kernel = Arc::clone(kernel);
            move |req: &Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                kernel
                    .appends
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req.body).to_string());
                let applies = kernel.apply_on_append.load(Ordering::SeqCst)
                    || kernel.lost_response.load(Ordering::SeqCst);
                if applies {
                    let md = body
                        .get("data")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .into();
                    kernel.blocks.lock().unwrap().push(md);
                }
                if kernel.apply_on_append.load(Ordering::SeqCst) {
                    kernel_rest_response(json!(null))
                } else {
                    ResponseTemplate::new(500)
                }
            }
        })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(|req: &Request| {
            let body = String::from_utf8_lossy(&req.body);
            if body.contains("\"initialize\"") {
                return mcp_initialize();
            }
            if body.contains("notifications/") {
                return mcp_initialized();
            }
            if body.contains("tools/call") && body.contains("\"block\"") && body.contains("\"get\"")
            {
                return mcp_raw_text(format!(
                    "ID: {BOUND_DOC}\nType: NodeDocument\nHPath: {BOARD_HPATH}/{TASK_TITLE}"
                ));
            }
            mcp_payload(json!({ "action": "unknown", "data": null }))
        })
        .mount(&server)
        .await;
    server
}

fn comment_board() -> SyBoard {
    SyBoard {
        doc_id: BOARD_DOC.into(),
        av_id: AV.into(),
        block_id: CARRIER.into(),
        hpath: BOARD_HPATH.into(),
        notebook: NB.into(),
    }
}

fn comment_task() -> SyTask {
    SyTask {
        item_id: "item-1".into(),
        title: TASK_TITLE.into(),
        status: "Draft".into(),
        branch: String::new(),
        pr: String::new(),
        schedule: String::new(),
        last_run: String::new(),
        dev_session: String::new(),
        body_doc: Some(BOUND_DOC.into()),
    }
}

#[derive(Debug)]
struct CommentOutcome {
    /// state дока после коммента (экспорт).
    md: String,
    /// сколько append-попыток сделал comment.
    append_attempts: usize,
}

async fn run_comment(
    base_blocks: &[&str],
    apply: bool,
    lost: bool,
    text: &str,
    reply_to: Option<&str>,
) -> anyhow::Result<CommentOutcome> {
    let kernel = Arc::new(CommentKernel {
        blocks: Mutex::new(base_blocks.iter().map(|s| s.to_string()).collect()),
        apply_on_append: AtomicBool::new(apply),
        lost_response: AtomicBool::new(lost),
        appends: Mutex::new(Vec::new()),
    });
    let server = comment_flow_mock(&kernel).await;
    set_env(&server);
    let sy = SiYuan::from_env()?;
    let draft = CommentDraft {
        author: "AGENTS",
        text,
        reply_to,
    };
    comment_with_ts(&sy, &comment_board(), &comment_task(), &draft, COMMENT_TS).await?;
    let md = kernel.blocks.lock().unwrap().join("\n");
    let append_attempts = kernel.appends.lock().unwrap().len();
    Ok(CommentOutcome {
        md,
        append_attempts,
    })
}

/// Секция «Комменты» уже есть: заголовок не дублируется, дописана строка.
#[tokio::test]
#[serial_test::serial]
async fn comment_with_existing_section_appends_only_the_line() {
    let outcome = run_comment(
        &[
            "## Контекст\nзаметки",
            "## Комменты",
            "- **AGENTS** (11.11 10:00): старый",
        ],
        true,
        false,
        "готово",
        None,
    )
    .await
    .expect("коммент ложится в существующую секцию");

    assert_eq!(outcome.append_attempts, 1, "только строка, без заголовка");
    assert!(
        outcome
            .md
            .contains("- **AGENTS** (2026-11-06 12:00): готово"),
        "новый: {}",
        outcome.md
    );
    assert_eq!(
        outcome.md.matches("## Комменты").count(),
        1,
        "заголовок единственный"
    );
    assert!(outcome.md.contains("старый"), "прежние комменты на месте");
}

/// Маркер уже в доке (повтор команды / применённый, но потерянный ответ) —
/// append не делается, коммент остаётся в единственном экземпляре.
#[tokio::test]
#[serial_test::serial]
async fn comment_with_marker_already_in_doc_appends_nothing() {
    let line = "- **AGENTS** (2026-11-06 12:00): готово";
    let outcome = run_comment(&["## Комменты", line], true, false, "готово", None)
        .await
        .expect("дубль распознан как успех");

    assert_eq!(outcome.append_attempts, 0, "записи не было");
    assert_eq!(
        outcome.md.matches(line).count(),
        1,
        "без задвоения: {}",
        outcome.md
    );
}

/// Потерянный ответ: appendBlock ответил 500, но блок применился —
/// контрольное чтение узнаёт маркер, повтор НЕ делается.
#[tokio::test]
#[serial_test::serial]
async fn comment_survives_lost_append_response_when_marker_landed() {
    let outcome = run_comment(&["## Контекст"], false, true, "готово", None)
        .await
        .expect("потерянный ответ не валит коммент");

    assert_eq!(
        outcome.append_attempts, 2,
        "заголовок + строка, без ретраев"
    );
    assert!(
        outcome.md.contains("## Комменты"),
        "заголовок применился: {}",
        outcome.md
    );
    assert!(
        outcome
            .md
            .contains("- **AGENTS** (2026-11-06 12:00): готово"),
        "строка: {}",
        outcome.md
    );
}

/// Append не применился и ответил ошибкой — честный сбой, коммента в доке нет.
#[tokio::test]
#[serial_test::serial]
async fn comment_fails_loudly_when_append_never_lands() {
    let err = run_comment(&["## Контекст"], false, false, "готово", None)
        .await
        .expect_err("не применённый append — ошибка");

    assert!(
        err.to_string().contains("500"),
        "транспортная причина в сообщении: {err}"
    );
}

/// Реплай пишется с отступом под родителя (`  - **…**`).
#[tokio::test]
#[serial_test::serial]
async fn comment_reply_line_is_indented_under_parent() {
    let outcome = run_comment(
        &["## Комменты", "- **owner** (11.11 09:00): вопрос"],
        true,
        false,
        "ответ",
        Some("20261105120000-parent"),
    )
    .await
    .expect("реплай ложится");

    assert!(
        outcome
            .md
            .contains("\n  - **AGENTS** (2026-11-06 12:00): ответ"),
        "реплай с отступом: {:?}",
        outcome.md
    );
}
