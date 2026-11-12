//! Общая тест-инфраструктура: мок-ядро (wiremock), сборка ответов rows/render,
//! док-пейзажи .sy, mount-хелперы.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub(super) const MARKER_ARCH: &str = "⁂ diagram:arch";
pub(super) const MARKER_BLOCK: &str = "20261105120100-marker1";
pub(super) const HTML_BLOCK: &str = "20261105120200-htmldoc1";
pub(super) const OTHER_ROW: &str = "item-other";
pub(super) const BOARD_DOC: &str = "board-doc";
pub(super) const AV_ID: &str = "av-1";
pub(super) const CARRIER: &str = "block-1";
pub(super) const TITLE_KEY: &str = "key-1";
pub(super) const ROW_ID: &str = "item-1";
pub(super) const BOUND_DOC: &str = "20261105120000-boundd1";
pub(super) const ORPHAN_DOC: &str = "20261105090000-orphan1";

pub(super) fn render_rows(with_binding: bool) -> serde_json::Value {
    render_rows_titled(with_binding, "таск")
}

pub(super) fn render_rows_titled(with_binding: bool, title: &str) -> serde_json::Value {
    let mut block = serde_json::json!({ "content": title, "refSubtype": "d" });
    if with_binding {
        block["id"] = serde_json::json!(BOUND_DOC);
    }
    serde_json::json!({
        "columns": [{ "id": TITLE_KEY, "name": "Primary Key" }],
        "rows": [{
            "id": ROW_ID,
            "cells": [{ "value": { "keyID": TITLE_KEY, "type": "block", "block": block } }]
        }]
    })
}

/// Ответ MCP tools/call(database render) как текст content[0].
pub(super) fn mcp_render_response(with_binding: bool) -> String {
    let mut view = render_rows(with_binding);
    view["rowCount"] = serde_json::json!(1);
    let payload = serde_json::json!({ "action": "render", "data": { "view": view } });
    serde_json::json!({
        "jsonrpc": "2.0", "id": 2,
        "result": { "content": [{ "type": "text", "text": payload.to_string() }] }
    })
    .to_string()
}

/// Ответ MCP render с тайтлом задачи, отличным от дефолтного «таск».
pub(super) fn mcp_render_response_titled(with_binding: bool, title: &str) -> String {
    let mut view = render_rows_titled(with_binding, title);
    view["rowCount"] = serde_json::json!(1);
    let payload = serde_json::json!({ "action": "render", "data": { "view": view } });
    serde_json::json!({
        "jsonrpc": "2.0", "id": 2,
        "result": { "content": [{ "type": "text", "text": payload.to_string() }] }
    })
    .to_string()
}

/// Минимальный мок kernel API SiYuan для body-пути: SQL-роутер по stmt,
/// MCP initialize/render. Счётчики: createDocWithMd (инвариант == 0)
/// и тела appendBlock (факт записи в привязанный док).
pub(super) async fn mock_siyuan(
    title_doc_ids: Vec<String>,
    with_binding: bool,
) -> (
    MockServer,
    Arc<AtomicUsize>,
    Arc<Mutex<Vec<String>>>,
    Arc<AtomicUsize>,
) {
    mock_siyuan_with_export(title_doc_ids, with_binding, String::new()).await
}

/// Расширенный мок: с экспортом дока (обёртка frontmatter + имя) для
/// rewrite-пути (replace/--replace-section). Четвёртый элемент — счётчик
/// deleteBlock: overwrite-путь удаляет блоки «штормом», no-write-тесты
/// ассертят его пустоту против именно опасного вызова.
pub(super) async fn mock_siyuan_with_export(
    title_doc_ids: Vec<String>,
    with_binding: bool,
    export_content: String,
) -> (
    MockServer,
    Arc<AtomicUsize>,
    Arc<Mutex<Vec<String>>>,
    Arc<AtomicUsize>,
) {
    mock_siyuan_with_export_titled(title_doc_ids, with_binding, export_content, "таск").await
}

/// Вариант с тайтлом задачи, отличным от дефолтного «таск» (регресс
/// маркер-подстроки в шапке get-вывода, задача 20261108003111).
/// .sy-док с парой «маркер arch + живой html-блок» (формат ядра 3.8.10).
pub(super) fn sy_doc(children: &[serde_json::Value]) -> String {
    serde_json::json!({ "ID": BOUND_DOC, "Children": children }).to_string()
}
pub(super) async fn mock_siyuan_with_export_titled(
    title_doc_ids: Vec<String>,
    with_binding: bool,
    export_content: String,
    title: &str,
) -> (
    MockServer,
    Arc<AtomicUsize>,
    Arc<Mutex<Vec<String>>>,
    Arc<AtomicUsize>,
) {
    let server = MockServer::start().await;
    let create_calls = Arc::new(AtomicUsize::new(0));
    let append_bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let delete_calls = Arc::new(AtomicUsize::new(0));

    let title_docs = title_doc_ids.clone();
    Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
                let data = if stmt.contains("type='av'") {
                    serde_json::json!([{ "id": CARRIER, "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{AV_ID}\"></div>") }])
                } else if stmt.contains(&format!("id='{BOUND_DOC}'")) {
                    serde_json::json!([{ "id": BOUND_DOC }])
                } else if stmt.contains("hpath like") {
                    serde_json::json!([{ "id": BOUND_DOC }, { "id": ORPHAN_DOC }])
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200).set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
            })
            .mount(&server)
            .await;

    let create_for_mount = create_calls.clone();
    Mock::given(method("POST"))
        .and(path("/api/filetree/createDocWithMd"))
        .respond_with(move |_req: &wiremock::Request| {
            create_for_mount.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200)
                .set_body_string(r#"{"code":-1,"msg":"createDocWithMd must not be called"}"#)
        })
        .mount(&server)
        .await;

    let appends = append_bodies.clone();
    Mock::given(method("POST"))
        .and(path("/api/block/appendBlock"))
        .respond_with(move |req: &wiremock::Request| {
            appends
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&req.body).to_string());
            ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
        })
        .mount(&server)
        .await;

    let deletes = delete_calls.clone();
    Mock::given(method("POST"))
        .and(path("/api/block/deleteBlock"))
        .respond_with(move |_req: &wiremock::Request| {
            deletes.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
        })
        .mount(&server)
        .await;

    let ids = title_docs.clone();
    let task_hpath = format!("/uwuwu/Задачи uwuwu/{title}");
    let export_hpath = task_hpath.clone();
    let block_hpath = task_hpath.clone();
    let render_title = title.to_string();
    Mock::given(method("POST"))
        .and(path("/api/filetree/getIDsByHPath"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let data = if hpath == "/uwuwu/Задачи uwuwu" {
                serde_json::json!([BOARD_DOC])
            } else if hpath == task_hpath {
                serde_json::json!(ids)
            } else {
                // прочие пути (крон-доски и пр.) в этом сценарии не существуют
                serde_json::json!([])
            };
            ResponseTemplate::new(200)
                .set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
        })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
            .and(path("/api/notebook/lsNotebooks"))
            .respond_with(|_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({ "code": 0, "data": { "notebooks": [{ "id": "nb-1", "name": "Projects" }] } })
                        .to_string(),
                )
            })
            .mount(&server)
            .await;

    Mock::given(method("POST"))
            .and(path("/api/export/exportMdContent"))
            .respond_with(move |_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({ "code": 0, "data": { "hPath": export_hpath, "content": export_content } })
                        .to_string(),
                )
            })
            .mount(&server)
            .await;

    Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-1")
                } else if body.contains("notifications/initialized") {
                    ResponseTemplate::new(200)
                } else if body.contains("tools/call") {
                    if body.contains("\"block\"") && body.contains("\"get\"") {
                        // doc_block_exists: живой bound-док → NodeDocument
                        ResponseTemplate::new(200).set_body_string(
                            format!(
                                r#"{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"ID: {BOUND_DOC}\nType: NodeDocument\nHPath: {block_hpath}"}}]}}}}"#
                            ),
                        )
                    } else if body.contains("\"block\"") && body.contains("get_children") {
                        // overwrite: док пуст → doc_content_is_empty → append
                        ResponseTemplate::new(200).set_body_string(
                            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":""}]}}"#,
                        )
                    } else {
                        ResponseTemplate::new(200)
                            .set_body_string(mcp_render_response_titled(with_binding, &render_title))
                    }
                } else {
                    ResponseTemplate::new(200).set_body_string(r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#)
                }
            })
            .mount(&server)
            .await;

    (server, create_calls, append_bodies, delete_calls)
}

pub(super) fn marker_child() -> serde_json::Value {
    serde_json::json!({
        "ID": MARKER_BLOCK, "Type": "NodeParagraph",
        "Children": [{ "Type": "NodeText", "Data": MARKER_ARCH }]
    })
}

pub(super) fn html_child(html: &str) -> serde_json::Value {
    serde_json::json!({ "ID": HTML_BLOCK, "Type": "NodeHTMLBlock", "Data": html })
}

pub(super) fn paragraph_child(id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "ID": id, "Type": "NodeParagraph",
        "Children": [{ "Type": "NodeText", "Data": text }]
    })
}

/// Моки адресации .sy (getBlockInfo + getFile) поверх базового стенда:
/// при необходимости путь (значение Children массива) переключается
/// ответы по порядку вызовов.
pub(super) async fn mount_doc_sy(server: &MockServer, sy_docs: Vec<String>) {
    let docs = std::sync::Arc::new(std::sync::Mutex::new(sy_docs));
    Mock::given(method("POST"))
        .and(path("/api/block/getBlockInfo"))
        .respond_with(move |_req: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_string(
                serde_json::json!({
                    "code": 0,
                    "data": { "box": "box-1", "path": "/20261105120000-boundd1.sy" }
                })
                .to_string(),
            )
        })
        .mount(server)
        .await;
    let queue = docs.clone();
    Mock::given(method("POST"))
        .and(path("/api/file/getFile"))
        .respond_with(move |_req: &wiremock::Request| {
            let mut queue = queue.lock().unwrap();
            let body = if queue.len() == 1 {
                queue[0].clone()
            } else {
                queue.remove(0)
            };
            ResponseTemplate::new(200).set_body_string(body)
        })
        .mount(server)
        .await;
}

pub(super) async fn mount_block_api(
    server: &MockServer,
) -> (Arc<Mutex<Vec<String>>>, Arc<AtomicUsize>) {
    let update_bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let updates = update_bodies.clone();
    Mock::given(method("POST"))
        .and(path("/api/block/updateBlock"))
        .respond_with(move |req: &wiremock::Request| {
            updates
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&req.body).to_string());
            ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
        })
        .mount(server)
        .await;
    let insert_bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let inserts = insert_bodies.clone();
    Mock::given(method("POST"))
        .and(path("/api/block/insertBlock"))
        .respond_with(move |req: &wiremock::Request| {
            inserts
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&req.body).to_string());
            ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
        })
        .mount(server)
        .await;
    (update_bodies, Arc::new(AtomicUsize::new(0)))
}

pub(super) fn append_payloads(append_bodies: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    append_bodies
        .lock()
        .unwrap()
        .iter()
        .map(|b| {
            serde_json::from_str::<serde_json::Value>(b)
                .expect("appendBlock body — json")
                .get("data")
                .and_then(|d| d.as_str())
                .expect("data — markdown")
                .to_string()
        })
        .collect()
}

/// Мок «поиск доски по hpath»: getIDsByHPath отдаёт BOARD_DOC для
/// «/uwuwu/Задачи uwuwu», прочие пути — пусто.
pub(super) async fn mount_hpath_board_lookup(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/filetree/getIDsByHPath"))
        .respond_with(|req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let data = if hpath == "/uwuwu/Задачи uwuwu" {
                serde_json::json!([BOARD_DOC])
            } else {
                serde_json::json!([])
            };
            ResponseTemplate::new(200)
                .set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
        })
        .mount(server)
        .await;
}
