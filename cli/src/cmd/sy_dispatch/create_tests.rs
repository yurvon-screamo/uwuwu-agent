//! Тесты create: uploads до create, custom-agent/status, дедуп тайтла,
//! валидации до сети.

use super::attach::collect_asset_refs;

use super::task::task_run;

use crate::cmd::cli::TaskSub;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use super::test_support::*;
use serde_json::{json, Value};

/// Мок под полный create-путь: дедуп-пред-чек читает render, createDocWithMd
/// и asset/upload пишут в общий лог порядка, render stateful (строка доски
/// появляется после item_add — так живёт verify create).
async fn mock_siyuan_for_create(
    initial_row_title: Option<&str>,
) -> (
    MockServer,
    Arc<Mutex<Vec<&'static str>>>,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<Vec<(String, String)>>>,
    Arc<Mutex<Vec<String>>>,
) {
    let server = MockServer::start().await;
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let appends: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let attrs: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let status_writes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let item_added: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let initial_title = initial_row_title.map(String::from);

    mount_create_asset_upload(&server, &order).await;
    mount_create_read_apis(&server).await;
    mount_hpath_board_lookup(&server).await;
    mount_create_write_apis(&server, &order, &appends).await;
    mount_create_mcp(
        &server,
        &order,
        item_added.clone(),
        initial_title,
        &status_writes,
    )
    .await;
    mount_create_attr_apis(&server, &attrs).await;

    (server, order, appends, attrs, status_writes)
}

/// upload-хендлер create-стенда (порядок + kernel-путь ассетов).
async fn mount_create_asset_upload(server: &MockServer, order: &Arc<Mutex<Vec<&'static str>>>) {
    Mock::given(method("POST"))
        .and(path("/api/asset/upload"))
        .respond_with({
            let order = order.clone();
            move |req: &wiremock::Request| {
                order.lock().unwrap().push("upload");
                // kernel вставляет stem оригинала в сгенерированный путь:
                // assets/<stem>-<timestamp>-<random>.<ext>
                let body = String::from_utf8_lossy(&req.body);
                let name = body
                    .split("filename=\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .unwrap_or("f");
                let (stem, ext) = match name.rsplit_once('.') {
                    Some((stem, ext)) => (stem.to_string(), ext.to_string()),
                    None => (name.to_string(), "bin".to_string()),
                };
                ResponseTemplate::new(200).set_body_json(json!({
                    "code": 0,
                    "data": { "succFiles": [{ "index": 0, "name": name,
                              "path": format!("assets/{stem}-20261106090000-abc1234.{ext}") }] }
                }))
            }
        })
        .mount(server)
        .await;
}

/// read-only API create-стенда: ноутбук, av-носитель, hpath-поиск.
async fn mount_create_read_apis(server: &MockServer) {
    Mock::given(method("POST"))
            .and(path("/api/notebook/lsNotebooks"))
            .respond_with(|_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_string(
                    json!({ "code": 0, "data": { "notebooks": [{ "id": "nb-1", "name": "Projects" }] } })
                        .to_string(),
                )
            })
            .mount(server)
            .await;
    Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
                let data = if stmt.contains("type='av'") {
                    serde_json::json!([{ "id": CARRIER,
                        "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{AV_ID}\"></div>") }])
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({ "code": 0, "data": data }).to_string(),
                )
            })
            .mount(server)
            .await;
}

/// write-API create-стенда: createDocWithMd, appendBlock.
async fn mount_create_write_apis(
    server: &MockServer,
    order: &Arc<Mutex<Vec<&'static str>>>,
    appends: &Arc<Mutex<Vec<String>>>,
) {
    Mock::given(method("POST"))
        .and(path("/api/filetree/createDocWithMd"))
        .respond_with({
            let order = order.clone();
            move |_req: &wiremock::Request| {
                order.lock().unwrap().push("create");
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":"doc-new"}"#)
            }
        })
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/block/appendBlock"))
        .respond_with({
            let order = order.clone();
            let appends = appends.clone();
            move |req: &wiremock::Request| {
                order.lock().unwrap().push("append");
                appends
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req.body).to_string());
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
            }
        })
        .mount(server)
        .await;
}

/// render-ответ create-стенда: строки доски = начальный дедуп-ряд (если задан)
/// + новая строка после item_add; page:2 — пусто (пагинация).
fn render_rows_payload(
    initial_title: &Option<String>,
    item_added: bool,
    page_done: bool,
) -> String {
    let existing = initial_title.clone().map(|t| {
        serde_json::json!({
            "id": OTHER_ROW,
            "cells": [{ "value": { "keyID": TITLE_KEY, "type": "block",
                       "block": { "content": t, "id": ORPHAN_DOC, "refSubtype": "d" } } }]
        })
    });
    let mut rows: Vec<Value> = Vec::new();
    if let Some(row) = existing {
        rows.push(row);
    }
    if item_added && !page_done {
        rows.push(serde_json::json!({
            "id": ROW_ID,
            "cells": [{ "value": { "keyID": TITLE_KEY, "type": "block",
                       "block": { "content": "новая задача", "id": "doc-new", "refSubtype": "d" } } }]
        }));
    }
    let rows = if page_done {
        json!([])
    } else {
        serde_json::Value::Array(rows)
    };
    json!({ "action": "render", "data": { "view": {
        "columns": [{ "id": TITLE_KEY, "name": "Primary Key" }],
        "rows": rows } } })
    .to_string()
}

/// MCP-роутер create-стенда: render с item_add-веткой (дедуп тайтла).
/// MCP-рукопожатие: initialize + notifications/initialized (общий каркас).
/// Матчеры точечные — tools/call уходит в основной роутер стенда.
pub(super) async fn mount_mcp_handshake(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(body_string_contains("\"initialize\""))
        .respond_with(|_req: &wiremock::Request| {
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
                )
                .append_header("mcp-session-id", "sess-1")
        })
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(body_string_contains("notifications/initialized"))
        .respond_with(|_req: &wiremock::Request| ResponseTemplate::new(200))
        .mount(server)
        .await;
}

async fn mount_create_mcp(
    server: &MockServer,
    order: &Arc<Mutex<Vec<&'static str>>>,
    item_added: Arc<Mutex<bool>>,
    initial_title: Option<String>,
    status_writes: &Arc<Mutex<Vec<String>>>,
) {
    mount_mcp_handshake(server).await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(body_string_contains("tools/call"))
        .respond_with({
            let order = order.clone();
            let status_writes = status_writes.clone();
            move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("tools/call") {
                    let action = if body.contains("\"item_add\"") {
                        *item_added.lock().unwrap() = true;
                        order.lock().unwrap().push("item_add");
                        json!({ "action": "item_add", "data": { "itemID": ROW_ID } }).to_string()
                    } else if body.contains("\"item_update\"") {
                        status_writes
                            .lock()
                            .unwrap()
                            .push(String::from_utf8_lossy(&req.body).to_string());
                        json!({ "action": "item_update", "data": null }).to_string()
                    } else if body.contains("\"keys\"") {
                        json!({ "action": "keys",
                                "data": { "keys": [
                                    { "id": TITLE_KEY, "name": "Status" },
                                    { "id": "key-type", "name": "Тип" }
                                ] } })
                        .to_string()
                    } else if body.contains("\"render\"") {
                        let page_done = body.contains("\"page\":2");
                        let payload = render_rows_payload(
                            &initial_title,
                            *item_added.lock().unwrap(),
                            page_done,
                        );
                        payload
                    } else {
                        json!({ "action": "unknown", "data": null }).to_string()
                    };
                    return ResponseTemplate::new(200).set_body_string(
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": 2,
                            "result": { "content": [{ "type": "text", "text": action }] }
                        })
                        .to_string(),
                    );
                }
                ResponseTemplate::new(200).set_body_string(
                    r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#,
                )
            }
        })
        .mount(server)
        .await;
}

/// attr-API create-стенда: setBlockAttrs (custom-schedule/agent), editRow (статус).
async fn mount_create_attr_apis(server: &MockServer, attrs: &Arc<Mutex<Vec<(String, String)>>>) {
    Mock::given(method("POST"))
        .and(path("/api/attr/setBlockAttrs"))
        .respond_with({
            let attrs = attrs.clone();
            move |req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                if let Some((name, value)) = body
                    .get("attrs")
                    .and_then(|a| a.as_object())
                    .and_then(|a| a.iter().next())
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                {
                    attrs.lock().unwrap().push((name, value));
                }
                ResponseTemplate::new(200).set_body_string(r#"{"code":0}"#)
            }
        })
        .mount(server)
        .await;
}

/// create-вызов стенда с дефолт-полями (аудитор проекта); переопределения —
/// аргументами (agent/status/schedule/stdin).
async fn create_via_task_run(
    server: &MockServer,
    agent: Option<String>,
    status: Option<String>,
) -> anyhow::Result<()> {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    task_run(
        "uwuwu",
        TaskSub::Create {
            title: "аудитор проекта".into(),
            r#type: None,
            priority: None,
            autodone: false,
            stdin: false,
            attach: Vec::new(),
            schedule: None,
            agent,
            status,
            dry_run: false,
        },
    )
    .await
}

async fn create_via_cli(
    server: &MockServer,
    title: &str,
    attach: &[std::path::PathBuf],
) -> anyhow::Result<()> {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    task_run(
        "uwuwu",
        TaskSub::Create {
            title: title.into(),
            r#type: None,
            priority: None,
            autodone: false,
            stdin: false,
            attach: attach.to_vec(),
            schedule: None,
            agent: None,
            status: None,
            dry_run: false,
        },
    )
    .await
}

#[tokio::test]
#[serial_test::serial]
async fn create_with_attach_uploads_before_creating_and_appends_refs() {
    let (server, order, appends, _attrs, _status) = mock_siyuan_for_create(None).await;
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("скрин 1.png");
    std::fs::write(&img, b"png").unwrap();
    let doc = dir.path().join("notes.txt");
    std::fs::write(&doc, b"txt").unwrap();

    create_via_cli(&server, "новая задача", &[img, doc])
        .await
        .expect("create с аттачами ok");

    assert_eq!(
        *order.lock().unwrap(),
        vec!["upload", "upload", "create", "item_add", "append"],
        "порядок: upload все → createDoc → строка → референсы (тип — клетка доски, не append)"
    );
    let appends = appends.lock().unwrap();
    let refs_md: String = serde_json::from_str::<serde_json::Value>(&appends[0])
        .expect("appendBlock body — json")
        .get("data")
        .and_then(|d| d.as_str())
        .expect("data — markdown референсов")
        .to_string();
    assert!(
        refs_md.starts_with('\n'),
        "ведущая пустая строка: {refs_md:?}"
    );
    // round-trip через штатного читателя: закодированные пути декодируются
    assert_eq!(
        collect_asset_refs(&refs_md),
        vec![
            "скрин 1-20261106090000-abc1234.png".to_string(),
            "notes-20261106090000-abc1234.txt".to_string(),
        ],
        "обе ссылки читаются collect_asset_refs: {refs_md:?}"
    );
    assert!(
        refs_md.contains("![скрин 1.png]("),
        "картинка по расширению: {refs_md:?}"
    );
    assert!(
        !refs_md.contains("![notes.txt]"),
        "txt — ссылка, не картинка: {refs_md:?}"
    );
}

/// create --agent: attr custom-agent записан на теле свежей строки
/// (свойство документа, клетки на доске нет).
#[tokio::test]
#[serial_test::serial]
async fn create_with_agent_writes_custom_attr() {
    let (server, _order, _appends, attrs, _status) = mock_siyuan_for_create(None).await;

    create_via_task_run(&server, Some("code-quality-reviewer".into()), None)
        .await
        .expect("create --agent ok");

    let writes = attrs.lock().unwrap().clone();
    assert_eq!(
        writes,
        vec![(
            "custom-agent".to_string(),
            "code-quality-reviewer".to_string()
        )],
        "ровно одна запись атрибута на теле: {writes:?}"
    );
}

/// create --agent "мусор": slug-грамматика отвергает ДО мутаций
/// (дока нет, attr-записей нет).
#[tokio::test]
#[serial_test::serial]
async fn create_with_bad_agent_rejected_before_write() {
    let (server, _order, _appends, attrs, _status) = mock_siyuan_for_create(None).await;

    let err = create_via_task_run(&server, Some("Not A Slug".into()), None)
        .await
        .expect_err("кривой slug — отказ");

    assert!(err.to_string().contains("slug дефа"), "суть отказа: {err}");
    assert!(
        attrs.lock().unwrap().is_empty(),
        "записей нет: валидация до мутаций"
    );
}

/// create --status Todo (крон-ран-карточки): начальный статус строки
/// Todo записан в клетку доски, дефолт Draft не участвует.
#[tokio::test]
#[serial_test::serial]
async fn create_with_status_todo_writes_todo_to_cell() {
    let (server, _order, _appends, _attrs, status_writes) = mock_siyuan_for_create(None).await;

    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    task_run(
        "uwuwu",
        TaskSub::Create {
            title: "⏰ ран · 2026-11-12 03:00".into(),
            r#type: None,
            priority: None,
            autodone: false,
            stdin: false,
            attach: Vec::new(),
            schedule: None,
            agent: None,
            status: Some("Todo".into()),
            dry_run: false,
        },
    )
    .await
    .expect("create --status Todo ok");

    let bodies = status_writes.lock().unwrap();
    let todos: Vec<_> = bodies
        .iter()
        .filter(|body| {
            serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|v| {
                    v.pointer("/params/arguments/value/mSelect/0/content")
                        .or_else(|| v.pointer("/value/mSelect/0/content"))
                        .and_then(|c| c.as_str())
                        .map(String::from)
                })
                .as_deref()
                == Some("Todo")
        })
        .collect();
    assert_eq!(
        todos.len(),
        1,
        "ровно одна запись начального статуса Todo: {:#?}",
        bodies
    );
}

/// create --status вне Todo (включая Draft и пустое) — отказ ДО мутаций:
/// флаг строго для машинного Todo (Draft — дефолт без флага, прочие
/// статусы — task edit).
#[tokio::test]
#[serial_test::serial]
async fn create_with_status_other_than_todo_rejected_before_write() {
    let (server, order, _appends, _attrs, status_writes) = mock_siyuan_for_create(None).await;

    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    for raw in ["Draft", "In Progress", ""] {
        let err = task_run(
            "uwuwu",
            TaskSub::Create {
                title: "статус вне контракта".into(),
                r#type: None,
                priority: None,
                autodone: false,
                stdin: false,
                attach: Vec::new(),
                schedule: None,
                agent: None,
                status: Some(raw.into()),
                dry_run: false,
            },
        )
        .await
        .expect_err(&format!("--status «{raw}» — отказ"));

        assert!(
            err.to_string().contains("допустим только Todo"),
            "суть отказа для «{raw}»: {err}"
        );
    }
    assert!(
        status_writes.lock().unwrap().is_empty(),
        "записей нет: валидация до мутаций"
    );
    assert!(
        order.lock().unwrap().is_empty(),
        "ни upload, ни createDoc, ни item_add — отказ строго до записей: {:?}",
        order.lock().unwrap()
    );
}

#[tokio::test]
#[serial_test::serial]
async fn create_with_attach_bails_on_duplicate_title_before_any_upload() {
    let (server, order, _appends, _attrs, _status) =
        mock_siyuan_for_create(Some("новая задача")).await;
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("скрин.png");
    std::fs::write(&img, b"png").unwrap();

    let err = create_via_cli(&server, "новая задача", &[img])
        .await
        .expect_err("дубль-тайтл — отказ");

    assert!(
        err.to_string().contains("уже есть"),
        "пред-чек дедупа: {err}"
    );
    assert!(
        !order.lock().unwrap().contains(&"upload"),
        "ни одного upload до отказа"
    );
    assert!(
        !order.lock().unwrap().contains(&"create"),
        "createDoc не вызван"
    );
}

// ——— body --attach: порядок upload → контент → референсы; attach-only ———

#[tokio::test]
#[serial_test::serial]
async fn validation_errors_fire_before_network_and_uploads() {
    let (server, create_calls, append_bodies, delete_calls) =
        mock_siyuan(vec![BOUND_DOC.to_string()], true).await;
    let upload_hits = Arc::new(AtomicUsize::new(0));
    let hits = upload_hits.clone();
    Mock::given(method("POST"))
        .and(path("/api/asset/upload"))
        .respond_with(move |_req: &wiremock::Request| {
            hits.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_string(
                r#"{"code":0,"data":{"succMap":{"a.png":"assets/a-20261106090000-abc1234.png"}}}"#,
            )
        })
        .mount(&server)
        .await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

    let md = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        md.path(),
        "```html_embed arch\n<div>a</div>\n```\n```html_embed arch\n<div>b</div>\n```\n",
    )
    .unwrap();
    let attach = tempfile::NamedTempFile::with_suffix(".png").unwrap();
    std::fs::write(attach.path(), b"png-bytes").unwrap();
    let err = task_run(
        "uwuwu",
        TaskSub::Body {
            id: "таск".into(),
            stdin: false,
            md: Some(md.path().to_string_lossy().into_owned()),
            replace: false,
            replace_section: None,
            base: None,
            force: false,
            attach: vec![attach.path().to_path_buf()],
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("дубль имени"), "{err}");
    assert_eq!(create_calls.load(Ordering::SeqCst), 0, "отказ до сети");
    assert_eq!(
        upload_hits.load(Ordering::SeqCst),
        0,
        "отказ до upload: ассет не загружен"
    );
    assert!(append_payloads(&append_bodies).is_empty());
    assert_eq!(delete_calls.load(Ordering::SeqCst), 0);
}

// ——— контракт ответов мутаций: формат ответа — юнит-ассерты ———
