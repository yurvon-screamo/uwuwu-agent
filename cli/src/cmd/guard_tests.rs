//! BDD-сценарии guard'а батч-сноса (задача 20261104215724): вердикты по
//! независимым источникам на wiremock-ядре с состоянием render (до/после
//! item_remove), FS-роутером getFile и write-ahead журналом.
//!
//! Сценарии плана ревью:
//! (а) Live+Dead без --live → отказ ЦЕЛИКОМ, ноль мутаций
//! (б) Dead+DraftDead с --yes → снесены, журнал записан, пост-чек зелёный
//! (в) Divergent → dry-run помечает, --yes отказывает весь батч
//! (г) undo восстанавливает; повторный undo — no-op (ноль мутаций)
//! (д) rm Live-не-Draft → отказ; rm Draft с контентным телом → журнал+снос
//! (е) контрольная проба доски miss → аборт без мутаций
//! (ж) пост-чек: лишняя пропажа тайтла → громкая ошибка + путь журнала

use crate::cmd::cli::TaskSub;
use crate::cmd::sy_dispatch::task_run;
use crate::siyuan_task::body_template;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NB: &str = "nb-1";
const BOARD_DOC: &str = "board-doc";
const AV: &str = "av-1";
const CARRIER: &str = "block-1";
const UNDO_JOURNAL: &str = "journal-test.json";

struct Row {
    item: &'static str,
    title: &'static str,
    status: &'static str,
    doc: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row { item: "item-live", title: "живая задача", status: "In Progress", doc: Some("20261104000001-livexx1") },
    Row { item: "item-dead", title: "мёртвая строка", status: "Todo", doc: Some("20261104000002-dedxxx2") },
    Row { item: "item-draft", title: "черновик-пустышка", status: "Draft", doc: Some("20261104000003-dra0003") },
    Row { item: "item-div", title: "див-строка", status: "Todo", doc: Some("20261104000004-divxx04") },
    Row { item: "item-skel", title: "скелет-черновик", status: "Draft", doc: Some("20261104000005-skelxx5") },
    Row { item: "item-draft-live", title: "живой черновик", status: "Draft", doc: Some("20261104000006-drlxx06") },
];

fn row_by_item(item: &str) -> &'static Row {
    ROWS.iter().find(|r| r.item == item).expect("fixture row")
}

/// Общее состояние мок-ядра: состав доски (до/после item_remove), счётчики
/// мутаций, содержимое undo-журналов, поведение FS (board-miss, post-check
/// «лишняя пропажа»).
struct Kernel {
    remaining: Mutex<Vec<&'static str>>,
    /// Восстановленные undo-строки (id, тайтл, статус) — вне фикстуры.
    added: Mutex<Vec<(String, String, String)>>,
    item_remove_calls: AtomicUsize,
    put_file_calls: AtomicUsize,
    remove_doc_calls: AtomicUsize,
    item_add_calls: AtomicUsize,
    item_update_calls: AtomicUsize,
    boards_file_missing: AtomicUsize,
    post_check_drop_all: AtomicUsize,
    undo_journal: Mutex<HashMap<String, Vec<u8>>>,
}

impl Kernel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            remaining: Mutex::new(ROWS.iter().map(|r| r.item).collect()),
            added: Mutex::new(Vec::new()),
            item_remove_calls: AtomicUsize::new(0),
            put_file_calls: AtomicUsize::new(0),
            remove_doc_calls: AtomicUsize::new(0),
            item_add_calls: AtomicUsize::new(0),
            item_update_calls: AtomicUsize::new(0),
            boards_file_missing: AtomicUsize::new(0),
            post_check_drop_all: AtomicUsize::new(0),
            undo_journal: Mutex::new(HashMap::new()),
        })
    }

    fn counter(&self, cell: &AtomicUsize) -> usize {
        cell.load(Ordering::SeqCst)
    }
}

fn render_row(item: &str) -> serde_json::Value {
    let row = row_by_item(item);
    let mut block = serde_json::json!({ "content": row.title, "refSubtype": "d" });
    if let Some(doc) = row.doc {
        block["id"] = serde_json::json!(doc);
    }
    render_row_cells(item, row.title, row.status, block)
}

fn render_added_row(id: &str, title: &str, status: &str) -> serde_json::Value {
    render_row_cells(
        id,
        title,
        status,
        serde_json::json!({ "content": title, "refSubtype": "d" }),
    )
}

fn render_row_cells(id: &str, _title: &str, status: &str, block: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "cells": [
            { "value": { "keyID": "key-title", "type": "block", "block": block } },
            { "value": { "keyID": "key-status", "type": "mSelect", "mSelect": [{ "content": status }] } }
        ]
    })
}

/// Ответ MCP tools/call как текст content[0].
fn mcp_text(payload: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": { "content": [{ "type": "text", "text": payload.to_string() }] }
        })
        .to_string(),
    )
}

fn mcp_plain(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": { "content": [{ "type": "text", "text": text }] }
        })
        .to_string(),
    )
}

fn missing_file() -> ResponseTemplate {
    ResponseTemplate::new(202).set_body_string(r#"{"code":404,"msg":"file does not exist"}"#)
}

async fn mount_kernel(server: &MockServer, kernel: &Arc<Kernel>) {
    // — REST: SQL-роутер (av-носитель, SQL-пробы тел, hpath-выборки) —
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
            let data = if stmt.contains("type='av'") {
                serde_json::json!([{ "id": CARRIER, "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{AV}\"></div>") }])
            } else if let Some(doc) = stmt.split("id='").nth(1).map(|s| s.split('\'').next().unwrap_or("")) {
                // SQL-проба тела по id: живой только doc-live (индекс)
                match doc {
                    "20261104000001-livexx1" => serde_json::json!([{ "id": "20261104000001-livexx1" }]),
                    _ => serde_json::json!([]),
                }
            } else {
                serde_json::json!([])
            };
            ResponseTemplate::new(200).set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
        })
        .mount(server)
        .await;

    // — REST: файловый контракт 3.8.9 (FS-пробы, контрольная проба доски) —
    Mock::given(method("POST"))
        .and(path("/api/file/getFile"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let file = body.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
                if file == format!("data/{NB}/folder-doc/{BOARD_DOC}.sy") && kernel.counter(&kernel.boards_file_missing) > 0 {
                    return missing_file();
                }
                match file.as_str() {
                    "data/nb-1/folder-doc/board-doc/20261104000001-livexx1.sy"
                    | "data/nb-1/folder-doc/board-doc/20261104000005-skelxx5.sy"
                    | "data/nb-1/folder-doc/board-doc/20261104000006-drlxx06.sy" => {
                        ResponseTemplate::new(200).set_body_bytes(b"{\"Type\":\"NodeDocument\"}".to_vec())
                    }
                    "data/nb-1/folder-doc/board-doc/20261104000004-divxx04.sy" => {
                        ResponseTemplate::new(500).set_body_string(r#"{"code":-1,"msg":"boom"}"#)
                    }
                    "data/nb-1/folder-doc/board-doc.sy" => {
                        ResponseTemplate::new(200).set_body_bytes(b"{\"Type\":\"NodeDocument\"}".to_vec())
                    }
                    _ => {
                        let seeded = kernel.undo_journal.lock().unwrap().get(&file).cloned();
                        match seeded {
                            Some(bytes) => ResponseTemplate::new(200).set_body_bytes(bytes),
                            None => missing_file(),
                        }
                    }
                }
            }
        })
        .mount(server)
        .await;

    // — REST: getIDsByHPath (доска + доки по тайтлам) —
    Mock::given(method("POST"))
        .and(path("/api/filetree/getIDsByHPath"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let data = match hpath {
                "/uwuwu" => serde_json::json!(["folder-doc"]),
                "/uwuwu/Задачи uwuwu" => serde_json::json!([BOARD_DOC]),
                "/uwuwu/Задачи uwuwu/живая задача" => serde_json::json!(["20261104000001-livexx1"]),
                "/uwuwu/Задачи uwuwu/скелет-черновик" => serde_json::json!(["20261104000005-skelxx5"]),
                "/uwuwu/Задачи uwuwu/живой черновик" => serde_json::json!(["20261104000006-drlxx06"]),
                _ => serde_json::json!([]),
            };
            ResponseTemplate::new(200).set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
        })
        .mount(server)
        .await;

    // — REST: экспорт тел (скелет-сравнение) —
    Mock::given(method("POST"))
        .and(path("/api/export/exportMdContent"))
        .respond_with(move |req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let id = body.get("id").and_then(|i| i.as_str()).unwrap_or("");
            let content = match id {
                "20261104000005-skelxx5" => skeleton_export("скелет-черновик"),
                "20261104000006-drlxx06" => format!(
                    "---\ntitle: живой черновик\n---\n\n# живой черновик\n\n{}\nзаметки триажа владельца\n",
                    body_template("живой черновик", "dev", "")
                ),
                _ => "---\ntitle: живая задача\n---\n\n# живая задача\n\nживой контент владельца\n".to_string(),
            };
            ResponseTemplate::new(200).set_body_string(
                serde_json::json!({ "code": 0, "data": { "hPath": "/uwuwu/Задачи uwuwu/т", "content": content } }).to_string(),
            )
        })
        .mount(server)
        .await;

    // — REST: putFile (write-ahead журнал) + removeDocByID (тела в трэш) —
    Mock::given(method("POST"))
        .and(path("/api/file/putFile"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                kernel.put_file_calls.fetch_add(1, Ordering::SeqCst);
                let body = String::from_utf8_lossy(&req.body).to_string();
                if let Some(name) = journal_name_from_multipart(&body) {
                    let path = format!("data/assets/undo/{name}");
                    kernel.undo_journal.lock().unwrap().insert(path, req.body.clone());
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
            move |_: &Request| {
                kernel.remove_doc_calls.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"msg":"","data":null}"#)
            }
        })
        .mount(server)
        .await;

    // — REST: ноутбуки —
    Mock::given(method("POST"))
        .and(path("/api/notebook/lsNotebooks"))
        .respond_with(|_: &Request| {
            ResponseTemplate::new(200).set_body_string(
                serde_json::json!({ "code": 0, "data": { "notebooks": [{ "id": NB, "name": "Projects", "closed": false }] } }).to_string(),
            )
        })
        .mount(server)
        .await;

    // — MCP: render с состоянием, item_remove, keys, block get, item_add —
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-guard");
                }
                if body.contains("notifications/initialized") {
                    return ResponseTemplate::new(200);
                }
                if !body.contains("tools/call") {
                    return ResponseTemplate::new(200).set_body_string(r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#);
                }
                if body.contains("\"database\"") && body.contains("\"render\"") {
                    let remaining = kernel.remaining.lock().unwrap().clone();
                    let added = kernel.added.lock().unwrap().clone();
                    let mut rows: Vec<serde_json::Value> = remaining.iter().map(|item| render_row(item)).collect();
                    rows.extend(added.iter().map(|(id, title, status)| render_added_row(id, title, status)));
                    let count = rows.len();
                    return mcp_text(serde_json::json!({
                        "action": "render",
                        "data": { "view": { "columns": [
                            { "id": "key-title", "name": "Primary Key" },
                            { "id": "key-status", "name": "Status" }
                        ], "rowCount": count, "rows": rows } }
                    }));
                }
                if body.contains("\"database\"") && body.contains("\"item_remove\"") {
                    kernel.item_remove_calls.fetch_add(1, Ordering::SeqCst);
                    let parsed: serde_json::Value = body.parse().unwrap_or_default();
                    let ids: Vec<String> = parsed
                        .pointer("/params/arguments/itemIDs")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|i| i.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    let mut remaining = kernel.remaining.lock().unwrap();
                    if kernel.counter(&kernel.post_check_drop_all) > 0 {
                        remaining.clear();
                    } else {
                        remaining.retain(|item| !ids.iter().any(|id| id == item));
                    }
                    return mcp_text(serde_json::json!({ "action": "item_remove", "data": null }));
                }
                if body.contains("\"database\"") && body.contains("\"keys\"") {
                    return mcp_text(serde_json::json!({ "action": "keys", "data": { "keys": [
                        { "id": "key-title", "name": "Primary Key" },
                        { "id": "key-status", "name": "Status" },
                        { "id": "key-branch", "name": "Branch" },
                        { "id": "key-pr", "name": "PR" }
                    ] } }));
                }
                if body.contains("\"database\"") && body.contains("\"item_add\"") {
                    kernel.item_add_calls.fetch_add(1, Ordering::SeqCst);
                    // восстановленная строка живёт только в added (вне фикстуры):
                    // render отдаёт её через render_added_row
                    kernel.added.lock().unwrap().push((
                        "item-restored".to_string(),
                        "мёртвая строка".to_string(),
                        "Todo".to_string(),
                    ));
                    return mcp_text(serde_json::json!({ "action": "item_add", "data": { "itemID": "item-restored" } }));
                }
                if body.contains("\"database\"") && body.contains("\"item_update\"") {
                    kernel.item_update_calls.fetch_add(1, Ordering::SeqCst);
                    return mcp_text(serde_json::json!({ "action": "item_update", "data": null }));
                }
                if body.contains("\"block\"") && body.contains("\"get\"") {
                    return mcp_plain("ID: 20261104000001-livexx1\nType: NodeDocument\n");
                }
                mcp_text(serde_json::json!({ "action": "unknown", "data": null }))
            }
        })
        .mount(server)
        .await;
}

fn skeleton_export(title: &str) -> String {
    format!("---\ntitle: {title}\n---\n\n# {title}\n\n{}\n\n#dev\n", body_template(title, "dev", ""))
}

/// Имя журнала из multipart-тела putFile (поле path).
fn journal_name_from_multipart(body: &str) -> Option<String> {
    let marker = "name=\"path\"\r\n\r\n";
    let idx = body.find(marker)?;
    let rest = &body[idx + marker.len()..];
    let value = rest.split("\r\n").next()?;
    value.strip_prefix("data/assets/undo/").map(String::from)
}

fn set_env(server: &MockServer) {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
}

/// (а) Live+Dead без --live: отказ ЦЕЛИКОМ, ноль мутаций (и ноль журналов).
#[tokio::test]
#[serial_test::serial]
async fn clean_refuses_whole_batch_when_live_row_lacks_ack() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run("uwuwu", TaskSub::Clean {
        ids: "живая задача,мёртвая строка".into(),
        yes: true,
        live: None,
    })
    .await
    .expect_err("батч с Live без ack обязан отказать");

    assert!(err.to_string().contains("ЦЕЛИКОМ"), "суть отказа: {err}");
    assert!(err.to_string().contains("живая задача"), "отказная строка названа: {err}");
    assert_eq!(kernel.counter(&kernel.item_remove_calls), 0, "ноль item_remove");
    assert_eq!(kernel.counter(&kernel.put_file_calls), 0, "журнал не пишется до гейта");
}

/// (б) Dead+DraftDead с --yes: снесены одним item_remove, журнал до мутации,
/// пост-чек зелёный (живая осталась).
#[tokio::test]
#[serial_test::serial]
async fn clean_removes_dead_and_draft_rows_with_yes_and_writes_journal() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    task_run("uwuwu", TaskSub::Clean {
        ids: "мёртвая строка,черновик-пустышка".into(),
        yes: true,
        live: None,
    })
    .await
    .expect("dead+draftdead сносится с --yes");

    assert_eq!(kernel.counter(&kernel.item_remove_calls), 1, "один item_remove массивом");
    assert_eq!(kernel.counter(&kernel.put_file_calls), 1, "write-ahead журнал записан");
    assert_eq!(kernel.counter(&kernel.remove_doc_calls), 0, "у Dead/DraftDead тел нет — трэш не зовётся");
    {
        let remaining = kernel.remaining.lock().unwrap();
        assert!(remaining.contains(&"item-live"), "живая строка на доске");
        assert!(!remaining.contains(&"item-dead") && !remaining.contains(&"item-draft"), "кандидаты сняты");
    }
}

/// (в) Divergent: dry-run помечает и не мутирует; --yes отказывает весь батч.
#[tokio::test]
#[serial_test::serial]
async fn clean_marks_divergent_in_dry_run_and_refuses_execution() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    task_run("uwuwu", TaskSub::Clean { ids: "див-строка".into(), yes: false, live: None })
        .await
        .expect("dry-run всегда проходит");
    assert_eq!(kernel.counter(&kernel.item_remove_calls), 0, "dry-run не мутирует");

    let err = task_run("uwuwu", TaskSub::Clean { ids: "див-строка".into(), yes: true, live: None })
        .await
        .expect_err("Divergent не сносится");
    assert!(err.to_string().contains("ЦЕЛИКОМ"), "суть отказа: {err}");
    assert_eq!(kernel.counter(&kernel.item_remove_calls), 0);
}

/// DraftSkeleton: снос с --yes, пустой скелет тела уходит в трэш.
#[tokio::test]
#[serial_test::serial]
async fn clean_removes_draft_skeleton_and_trashes_body() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    task_run("uwuwu", TaskSub::Clean { ids: "скелет-черновик".into(), yes: true, live: None })
        .await
        .expect("DraftSkeleton сносится без ack");

    assert_eq!(kernel.counter(&kernel.item_remove_calls), 1);
    assert_eq!(kernel.counter(&kernel.remove_doc_calls), 1, "скелет тела в трэш");
    assert!(!kernel.remaining.lock().unwrap().contains(&"item-skel"));
}

/// (е) Контрольная проба доски: файла доски нет → аборт без мутаций.
#[tokio::test]
#[serial_test::serial]
async fn clean_aborts_without_mutations_when_board_file_missing() {
    let kernel = Kernel::new();
    kernel.boards_file_missing.store(1, Ordering::SeqCst);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run("uwuwu", TaskSub::Clean { ids: "мёртвая строка".into(), yes: true, live: None })
        .await
        .expect_err("контрольная проба обязана прервать батч");

    assert!(err.to_string().contains("контрольная проба"), "суть отказа: {err}");
    assert_eq!(kernel.counter(&kernel.item_remove_calls), 0, "ноль мутаций");
    assert_eq!(kernel.counter(&kernel.put_file_calls), 0, "журнал не пишется");
}

/// (ж) Пост-чек: item_remove снял больше плана → громкая ошибка с журналом.
#[tokio::test]
#[serial_test::serial]
async fn clean_post_check_alarms_on_unplanned_loss() {
    let kernel = Kernel::new();
    kernel.post_check_drop_all.store(1, Ordering::SeqCst);
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run("uwuwu", TaskSub::Clean { ids: "мёртвая строка".into(), yes: true, live: None })
        .await
        .expect_err("лишняя пропажа обязана поднять аларм");

    let msg = err.to_string();
    assert!(msg.contains("ПОСТ-ЧЕК"), "суть ошибки: {msg}");
    assert!(msg.contains("живая задача"), "названа неплановая жертва: {msg}");
    assert!(msg.contains("task undo"), "путь восстановления указан: {msg}");
    assert!(msg.contains("data/assets/undo/"), "журнал упомянут: {msg}");
}

/// (д) rm: Live не-Draft → отказ; Draft с контентным телом (Live+Draft) →
/// журнал + снос строки и тела.
#[tokio::test]
#[serial_test::serial]
async fn rm_refuses_live_task_but_removes_contentful_draft() {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run("uwuwu", TaskSub::Rm { id: "живая задача".into() })
        .await
        .expect_err("rm живой задачи обязан отказать");
    let msg = err.to_string();
    assert!(msg.contains("rm только для Draft"), "суть отказа: {msg}");
    assert!(msg.contains("task close"), "hint на close: {msg}");
    assert_eq!(kernel.counter(&kernel.item_remove_calls), 0);

    task_run("uwuwu", TaskSub::Rm { id: "живой черновик".into() })
        .await
        .expect("контентный Draft (Live+Draft) сносится rm");
    assert_eq!(kernel.counter(&kernel.item_remove_calls), 1);
    assert_eq!(kernel.counter(&kernel.put_file_calls), 1, "журнал до мутации");
    assert_eq!(kernel.counter(&kernel.remove_doc_calls), 1, "тело черновика в трэш");
}

/// (г) undo: восстанавливает строку из журнала (detached — тела не было);
/// повторный undo — no-op: ни одного нового item_add.
#[tokio::test]
#[serial_test::serial]
async fn undo_restores_row_and_repeats_as_noop() {
    let kernel = Kernel::new();
    let journal = serde_json::json!({
        "created": "2026-11-04T21:00:00+00:00",
        "project": "uwuwu",
        "kind": "clean",
        "av_id": AV,
        "board_doc": BOARD_DOC,
        "notebook": NB,
        "rows": [{
            "item_id": "item-old",
            "title": "мёртвая строка",
            "status": "Todo",
            "branch": "cli/task-clean-guard",
            "pr": "https://github.com/yurvon-screamo/uwuwu-agent/pull/50",
            "extra_text_cells": [],
            "body_doc": "20261104000002-dedxxx2",
            "body_hpath": "/uwuwu/Задачи uwuwu/мёртвая строка",
            "body_md": null
        }]
    });
    kernel.undo_journal.lock().unwrap().insert(
        format!("data/assets/undo/{UNDO_JOURNAL}"),
        journal.to_string().into_bytes(),
    );
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    // строка снята с доски (журнал писался при её сносе) — иначе undo
    // честно no-op по тайтлу
    kernel.remaining.lock().unwrap().retain(|item| *item != "item-dead");

    task_run("uwuwu", TaskSub::Undo { journal: Some(UNDO_JOURNAL.into()), list: false })
        .await
        .expect("первый undo восстанавливает");
    let after_first = kernel.counter(&kernel.item_add_calls);
    assert_eq!(after_first, 1, "одна вставка строки");
    assert!(
        kernel.added.lock().unwrap().iter().any(|(id, title, _)| id == "item-restored" && title == "мёртвая строка"),
        "восстановленная строка отдаётся render'ом"
    );
    // Status (mSelect) + Branch (text) + PR (url) — три item_update по клеткам
    let updates = kernel.counter(&kernel.item_update_calls);
    assert!(updates >= 2, "Branch/PR клетки восстанавливаются item_update: {updates}");

    task_run("uwuwu", TaskSub::Undo { journal: Some(UNDO_JOURNAL.into()), list: false })
        .await
        .expect("повторный undo — идемпотентный no-op");
    assert_eq!(
        kernel.counter(&kernel.item_add_calls),
        after_first,
        "повтор не мутирует доску (тайтл уже на доске → skip)"
    );
}
