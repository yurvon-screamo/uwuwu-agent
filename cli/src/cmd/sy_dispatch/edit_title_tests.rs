//! Тесты edit --title: rename дока/клетки, дедупы, orphan-конфликты, ретраи.

use crate::siyuan::SiYuan;

use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use super::test_support::*;

use crate::siyuan_task::{edit_title_with_pacing, SyTask, TitleSyncPacing};

const CHAIN_ROOT: &str = "uwuwu-root";
const OTHER_TITLE: &str = "чужой";
const ORPHAN_TITLE: &str = "сирота";

/// Мок под edit_title: id-цепочка доски (board_doc_chain), stateful render
/// (renameDoc-маунт кладёт новый тайтл в общее состояние — так живёт
/// verify-поллинг), renameDoc-маунт с записью запроса. `stateful_rename`
/// = false моделирует «зависшую» клетку: rename проходит, render-состояние
/// не меняется.
async fn mock_siyuan_for_title(
    stateful_rename: bool,
) -> (
    MockServer,
    Arc<Mutex<Option<String>>>,
    Arc<Mutex<Vec<String>>>,
) {
    let server = MockServer::start().await;
    let renamed_to: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let renames: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    Mock::given(method("POST"))
        .and(path("/api/filetree/getIDsByHPath"))
        .respond_with(|req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let data = match hpath {
                "/uwuwu" => serde_json::json!([CHAIN_ROOT]),
                "/uwuwu/Задачи uwuwu" => serde_json::json!([BOARD_DOC]),
                // «новый тайтл» отдаёт СОБСТВЕННЫЙ док: happy-путь проверяет
                // исключение своего docId из orphan-чека (retry после лага)
                "/uwuwu/Задачи uwuwu/новый тайтл" => {
                    serde_json::json!([BOUND_DOC])
                }
                "/uwuwu/Задачи uwuwu/сирота" => serde_json::json!([ORPHAN_DOC]),
                _ => serde_json::json!([]),
            };
            ResponseTemplate::new(200)
                .set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
        })
        .mount(&server)
        .await;

    let renamed_for_mount = renamed_to.clone();
    let renames_for_mount = renames.clone();
    Mock::given(method("POST"))
        .and(path("/api/filetree/renameDoc"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            renames_for_mount.lock().unwrap().push(
                body.get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
            if stateful_rename {
                *renamed_for_mount.lock().unwrap() = Some(
                    body.get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                );
            }
            ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
        })
        .mount(&server)
        .await;

    let renamed_for_render = renamed_to.clone();
    Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(move |req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-1");
                }
                if body.contains("notifications/initialized") {
                    return ResponseTemplate::new(200);
                }
                if body.contains("tools/call") {
                    let current = renamed_for_render.lock().unwrap().clone();
                    let own_title = current.unwrap_or_else(|| "таск".into());
                    let view = serde_json::json!({
                        "columns": [{ "id": TITLE_KEY, "name": "Primary Key" }],
                        "rows": [
                            { "id": ROW_ID, "cells": [{ "value": { "keyID": TITLE_KEY,
                                "type": "block",
                                "block": { "content": own_title, "id": BOUND_DOC, "refSubtype": "d" } } }] },
                            { "id": OTHER_ROW, "cells": [{ "value": { "keyID": TITLE_KEY,
                                "type": "block",
                                "block": { "content": OTHER_TITLE, "id": ORPHAN_DOC, "refSubtype": "d" } } }] },
                        ],
                    });
                    let page_done = body.contains("\"page\":2");
                    let rows = if page_done { serde_json::json!([]) } else { view["rows"].clone() };
                    let payload = serde_json::json!({ "action": "render", "data": { "view": {
                        "columns": view["columns"], "rows": rows } } });
                    return ResponseTemplate::new(200).set_body_string(
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": 2,
                            "result": { "content": [{ "type": "text", "text": payload.to_string() }] }
                        })
                        .to_string(),
                    );
                }
                ResponseTemplate::new(200).set_body_string(
                    r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#,
                )
            })
            .mount(&server)
            .await;

    (server, renamed_to, renames)
}

fn title_task() -> SyTask {
    SyTask {
        item_id: ROW_ID.into(),
        title: "таск".into(),
        status: String::new(),
        task_type: String::new(),
        priority: String::new(),
        autodone: false,
        branch: String::new(),
        pr: String::new(),
        dev_session: String::new(),
        planned: String::new(),
        planned_ms: 0,
        schedule_cell: String::new(),
        body_doc: Some(BOUND_DOC.into()),
    }
}

fn title_board() -> crate::siyuan_task::SyBoard {
    crate::siyuan_task::SyBoard {
        doc_id: BOARD_DOC.into(),
        av_id: AV_ID.into(),
        block_id: CARRIER.into(),
        hpath: "/uwuwu/Задачи uwuwu".into(),
        notebook: "nb-1".into(),
    }
}

fn set_kernel_url(server: &MockServer) {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
}

/// Стенд edit_title-сценария: мок ядра + env + пара доска/строка.
async fn mount_title_scenario(
    orphan_body: bool,
) -> (
    MockServer,
    Arc<Mutex<Option<String>>>,
    Arc<Mutex<Vec<String>>>,
    crate::siyuan_task::SyBoard,
    SyTask,
) {
    let (server, renamed_to, renames) = mock_siyuan_for_title(orphan_body).await;
    set_kernel_url(&server);
    let (board, task) = (title_board(), title_task());
    (server, renamed_to, renames, board, task)
}

/// Общая обвязка edit_title-сценариев: мок + env + вызов с instant-пейсом.
async fn run_edit_title(
    server: &MockServer,
    board: &crate::siyuan_task::SyBoard,
    task: &SyTask,
    new_title: &str,
) -> anyhow::Result<String> {
    edit_title_with_pacing(
        &SiYuan::for_tests(server.uri()),
        board,
        task,
        new_title,
        TitleSyncPacing::instant(),
    )
    .await
}

#[tokio::test]
#[serial_test::serial]
async fn edit_title_renames_doc_and_title_cell_confirms() {
    let (server, _renamed_to, renames, board, task) = mount_title_scenario(true).await;
    let new_title = run_edit_title(&server, &board, &task, "  новый тайтл  ")
        .await
        .expect("rename + sync ok");

    assert_eq!(new_title, "новый тайтл", "trim применён, без слэшей");
    let paths = renames.lock().unwrap();
    assert_eq!(
        paths.last().map(String::as_str),
        Some(&format!("/{CHAIN_ROOT}/{BOARD_DOC}/{BOUND_DOC}.sy") as &str),
        "renameDoc по storage-пути из id-цепочки, относительно ноутбука"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn edit_title_bails_on_row_duplicate_without_rename() {
    let (server, _renamed_to, renames, board, task) = mount_title_scenario(true).await;
    let err = run_edit_title(&server, &board, &task, OTHER_TITLE)
        .await
        .expect_err("чужой тайтл — отказ до мутаций");

    assert!(
        err.to_string().contains("занят другой строкой"),
        "суть: {err}"
    );
    assert!(renames.lock().unwrap().is_empty(), "renameDoc не вызван");
}

#[tokio::test]
#[serial_test::serial]
async fn edit_title_bails_on_orphan_body_with_target_title() {
    let (server, _renamed_to, renames, board, task) = mount_title_scenario(true).await;
    let err = run_edit_title(&server, &board, &task, ORPHAN_TITLE)
        .await
        .expect_err("тело-сирота с целевым тайтлом — отказ");

    assert!(err.to_string().contains("уже существует"), "суть: {err}");
    assert!(renames.lock().unwrap().is_empty(), "renameDoc не вызван");
}

#[tokio::test]
#[serial_test::serial]
async fn edit_title_reports_loudly_when_title_cell_never_syncs() {
    let (server, _renamed_to, renames, board, task) = mount_title_scenario(false).await;
    let err = run_edit_title(&server, &board, &task, "новый тайтл")
        .await
        .expect_err("клетка не синкалась — громкая ошибка");

    let text = err.to_string();
    assert!(text.contains("Title-клетка"), "суть: {text}");
    assert!(text.contains("идемпотентен"), "хинт на повтор: {text}");
    assert_eq!(renames.lock().unwrap().len(), 1, "renameDoc был один");
}

#[tokio::test]
#[serial_test::serial]
async fn edit_title_refuses_detached_row_before_any_call() {
    let (server, _renamed_to, renames, _board, _task) = mount_title_scenario(true).await;
    let mut task = title_task();
    task.body_doc = None;

    let err = run_edit_title(&server, &title_board(), &task, "новый тайтл")
        .await
        .expect_err("detached-строка — отказ");

    assert!(err.to_string().contains("без тела"), "суть: {err}");
    assert!(renames.lock().unwrap().is_empty(), "renameDoc не вызван");
}

#[tokio::test]
#[serial_test::serial]
async fn edit_title_retry_after_lag_passes_when_own_doc_already_renamed() {
    // retry после неподтвердившегося синка: rename прошёл (док уже «новый
    // тайтл», hpath-лукап отдаёт собственный docId), клетка ещё держит
    // старый тайтл (task.title = «таск»). Повтор обязан пройти, а не
    // советовать «вычисти сироту» про живое тело
    let (server, renamed_to, renames, _board, _task) = mount_title_scenario(true).await;
    *renamed_to.lock().unwrap() = Some("новый тайтл".into());
    let mut task = title_task();
    task.title = "таск".into();

    let result = run_edit_title(&server, &title_board(), &task, "новый тайтл")
        .await
        .expect("retry идемпотентен: собственный док исключён из orphan-чека");

    assert_eq!(result, "новый тайтл");
    assert_eq!(
        renames.lock().unwrap().len(),
        1,
        "retry делает свой no-op renameDoc"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn edit_title_same_title_short_circuits_without_kernel_calls() {
    let (server, _renamed_to, renames, _board, _task) = mount_title_scenario(true).await;
    let task = title_task();

    let result = run_edit_title(&server, &title_board(), &task, "таск")
        .await
        .expect("no-op rename — успех");

    assert_eq!(result, "таск");
    assert!(renames.lock().unwrap().is_empty(), "ядро не трогается");
}

// ——— create --attach: порядок upload → create → append, пред-чек дедупа ———
