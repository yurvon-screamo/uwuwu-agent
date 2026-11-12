//! Тесты html_embed-фенсов: append/replace/delete диаграмм по маркерам,
//! инлайн анонимных фенсов.

use super::task::task_run;

use crate::cmd::cli::TaskSub;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use super::test_support::*;

async fn write_md_content(content: &str, replace: bool, section: Option<String>) {
    let md = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(md.path(), content).unwrap();
    task_run(
        "uwuwu",
        TaskSub::Body {
            id: "таск".into(),
            stdin: false,
            md: Some(md.path().to_string_lossy().into_owned()),
            replace,
            replace_section: section,
            // тесты про конвертацию фенсов, не про freshness-гард (#119):
            // force снимает требование снапшота при полном replace
            base: None,
            force: replace,
            attach: Vec::new(),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
#[serial_test::serial]
async fn append_named_fence_updates_existing_diagram_in_place() {
    let (server, create_calls, append_bodies, _deletes) =
        mock_siyuan(vec![BOUND_DOC.to_string()], true).await;
    mount_doc_sy(
        &server,
        vec![sy_doc(&[marker_child(), html_child("<div>old</div>")])],
    )
    .await;
    let (update_bodies, _inserts) = mount_block_api(&server).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

    write_md_content(
        "текст рядом\n```html_embed arch\n<div>new</div>\n```\n",
        false,
        None,
    )
    .await;

    assert_eq!(create_calls.load(Ordering::SeqCst), 0);
    let updates = update_bodies.lock().unwrap();
    assert_eq!(
        updates.len(),
        1,
        "один updateBlock по html-блоку после маркера"
    );
    let payload = serde_json::from_str::<serde_json::Value>(&updates[0]).unwrap();
    assert_eq!(payload.get("id").unwrap(), HTML_BLOCK);
    assert_eq!(payload.get("data").unwrap(), "<div>new</div>");
    drop(updates);
    let appends = append_payloads(&append_bodies);
    assert_eq!(
        appends,
        vec!["текст рядом\n"],
        "residual без маркеров и html"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn append_named_fence_without_marker_appends_marker_pair() {
    let (server, create_calls, append_bodies, _deletes) =
        mock_siyuan(vec![BOUND_DOC.to_string()], true).await;
    mount_doc_sy(
        &server,
        vec![sy_doc(&[paragraph_child("p-1", "обычный текст")])],
    )
    .await;
    let (_update_bodies, _inserts) = mount_block_api(&server).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

    write_md_content("```html_embed arch\n<div>fresh</div>\n```\n", false, None).await;

    assert_eq!(create_calls.load(Ordering::SeqCst), 0);
    let appends = append_payloads(&append_bodies);
    assert_eq!(appends.len(), 1, "пара маркер+html одним appendBlock");
    assert_eq!(appends[0], "⁂ diagram:arch\n\n<div>fresh</div>");
}

#[tokio::test]
#[serial_test::serial]
async fn append_empty_named_fence_deletes_diagram() {
    let (server, create_calls, append_bodies, delete_calls) =
        mock_siyuan(vec![BOUND_DOC.to_string()], true).await;
    mount_doc_sy(
        &server,
        vec![sy_doc(&[marker_child(), html_child("<div>old</div>")])],
    )
    .await;
    let (_update_bodies, _inserts) = mount_block_api(&server).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

    write_md_content("```html_embed arch\n```\n", false, None).await;

    assert_eq!(create_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        delete_calls.load(Ordering::SeqCst),
        1,
        "deleteBlock html-блока диаграммы"
    );
    let appends = append_payloads(&append_bodies);
    assert!(appends.is_empty(), "удаление без residual не аппендит");
}

#[tokio::test]
#[serial_test::serial]
async fn append_anonymous_fence_inlines_html_without_doc_reads() {
    let (server, create_calls, append_bodies, _deletes) =
        mock_siyuan(vec![BOUND_DOC.to_string()], true).await;
    let info_hits = Arc::new(AtomicUsize::new(0));
    let hits = info_hits.clone();
    Mock::given(method("POST"))
        .and(path("/api/block/getBlockInfo"))
        .respond_with(move |_req: &wiremock::Request| {
            hits.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":{}}"#)
        })
        .mount(&server)
        .await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

    write_md_content("```html_embed\n<div>anon</div>\n```\n", false, None).await;

    assert_eq!(create_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        info_hits.load(Ordering::SeqCst),
        0,
        "анонимный фенс не адресует док — хирургия не нужна"
    );
    let appends = append_payloads(&append_bodies);
    assert_eq!(
        appends,
        vec!["<div>anon</div>\n\n"],
        "blank-guard после html"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn replace_inlines_named_fence_as_marker_pair() {
    let export = "---\ntitle: таск\n---\n\n# таск\n\nстарое тело\n".to_string();
    let (server, create_calls, append_bodies, _deletes) =
        mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
    let (_update_bodies, _inserts) = mount_block_api(&server).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

    write_md_content(
        "новое тело\n```html_embed arch\n<div>new</div>\n```\n",
        true,
        None,
    )
    .await;

    assert_eq!(create_calls.load(Ordering::SeqCst), 0);
    let appends = append_payloads(&append_bodies);
    assert_eq!(appends.len(), 1, "overwrite пустого мок-дока — один append");
    assert!(
        appends[0].contains("⁂ diagram:arch\n\n<div>new</div>"),
        "именованный фенс инлайнится парой «маркер + html»: {}",
        appends[0]
    );
}

#[tokio::test]
#[serial_test::serial]
async fn replace_section_keeps_live_html_outside_section_verbatim() {
    // живой html-блок вне секции: экспорт ядра отдаёт его сырым HTML —
    // перезапись обязана сохранить блок verbatim (High раунда 1)
    let export = "---\ntitle: таск\n---\n\n# таск\n\n<div class=\"live\">diag</div>\n\n## Решение\nстарая секция\n".to_string();
    let (server, create_calls, append_bodies, _deletes) =
        mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
    let (_update_bodies, _inserts) = mount_block_api(&server).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

    write_md_content(
        "новая секция\n```html_embed arch\n<div>new</div>\n```\n",
        false,
        Some("Решение".into()),
    )
    .await;

    assert_eq!(create_calls.load(Ordering::SeqCst), 0);
    let appends = append_payloads(&append_bodies);
    assert_eq!(appends.len(), 1);
    assert!(
        appends[0].contains("<div class=\"live\">diag</div>"),
        "живой html вне секции переживает перезапись verbatim: {}",
        appends[0]
    );
    assert!(
        appends[0].contains("⁂ diagram:arch"),
        "фенс секции инлайнится парой: {}",
        appends[0]
    );
    assert!(
        !appends[0].contains("```html_embed"),
        "литерального фенса нет"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn create_body_with_named_fence_writes_marker_pair() {
    // мини-стенд create-ветки (тела нет): createDocWithMd capture'ит markdown
    let server = MockServer::start().await;
    let created_md: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let created = created_md.clone();
    Mock::given(method("POST"))
        .and(path("/api/filetree/createDocWithMd"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            created.lock().unwrap().push(
                body.get("markdown")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
            ResponseTemplate::new(200)
                .set_body_string(r#"{"code":0,"data":"20261105120300-newdoc01"}"#)
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
                // поиск доски (doc_ids_by_hpath идёт через getIDsByHPath; здесь
                // только av-носитель доски) и прочие выборки — пусто
                let data = if stmt.contains("type='av'") {
                    serde_json::json!([{ "id": "block-1", "markdown": "<div data-type=\"NodeAttributeView\" data-av-id=\"av-1\"></div>" }])
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200).set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
            })
            .mount(&server)
            .await;
    Mock::given(method("POST"))
        .and(path("/api/notebook/lsNotebooks"))
        .respond_with(|_req: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_string(
                r#"{"code":0,"data":{"notebooks":[{"id":"nb-1","name":"Projects"}]}}"#,
            )
        })
        .mount(&server)
        .await;
    mount_hpath_board_lookup(&server).await;
    Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(|req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":3}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-1")
                } else if body.contains("notifications/initialized") {
                    ResponseTemplate::new(200)
                } else if body.contains("tools/call") {
                    if body.contains("\"block\"") && body.contains("\"get\"") {
                        // create-сценарий: привязки нет и дока нет → doc_block_exists=false
                        ResponseTemplate::new(200).set_body_string(
                            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"block not found"}}"#,
                        )
                    } else if body.contains("\"block\"") && body.contains("get_children") {
                        ResponseTemplate::new(200).set_body_string(
                            r#"{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":""}]}}"#,
                        )
                    } else {
                        ResponseTemplate::new(200).set_body_string(mcp_render_response(true))
                    }
                } else {
                    ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
                }
            })
            .mount(&server)
            .await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");

    write_md_content("```html_embed arch\n<div>first</div>\n```\n", false, None).await;

    let created = created_md.lock().unwrap();
    assert_eq!(created.len(), 1, "тело создано одним createDocWithMd");
    assert!(
        created[0].contains("⁂ diagram:arch\n\n<div>first</div>"),
        "create-ветка пишет пару «маркер + html»: {}",
        created[0]
    );
}
