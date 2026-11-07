//! cron set/list/pause/resume на wiremock-ядре (задача 20261104220156):
//! колонки крон-доски валидируются до мутаций (без сирот), строка
//! привязана к телу (blockID), Status — mSelect, список печатает
//! Schedule/Last Run, семья pause/resume/list умеет --project.
//!
//! Сценарии:
//! 1. доска без колонки Schedule → громкий отказ ДО создания тела/строки
//! 2. полноценная доска → bound-строка + Schedule/Status(mSelect)/Last Run
//! 3. дубль-тайтла в живых строках → отказ без мутаций
//! 4. item_add без строки в render → отказ после ретрая ×1 (verify вставки)
//! 5. pause с --project пишет Status на доске указанного проекта
//! 6. таблица list несёт значения Schedule и Last Run
//! 7. парс: --project у list/pause/resume/rm, дефолт None

use crate::cmd::cli::CronSub;
use crate::cmd::sy_dispatch::{cron_run, cron_set_with_pacing, format_cron_table};
use crate::siyuan::SiYuan;
use crate::siyuan_task::{RowInsertPacing, SyTask};
use serde_json::json;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NB: &str = "nb-projects";
const CRON_DOC: &str = "k-doc";
const CRON_AV: &str = "av-k";
const FOIL_DOC: &str = "f-doc";
const FOIL_AV: &str = "av-f";

/// Мутабельное состояние мок-ядра: что создали и что записали.
#[derive(Default)]
struct Kernel {
    /// hpath созданных доков (createDocWithMd) — «сироты» видны сразу.
    created_docs: Mutex<Vec<String>>,
    /// Аргументы item_add как есть (blockID/detached различимы).
    item_adds: Mutex<Vec<serde_json::Value>>,
    /// (av, keyID, itemID, value) всех item_update.
    item_updates: Mutex<Vec<(String, String, String, serde_json::Value)>>,
    /// false → на доске uwuwu нет колонок Schedule/Last Run (легаси-шаблон).
    cron_keys_full: Mutex<bool>,
    /// false → item_add не появляется в render (ядро «потеряло» вставку).
    render_confirms_insert: Mutex<bool>,
    /// Строки render крон-доски uwuwu (title, last_run статичны, item — id).
    cron_rows: Mutex<Vec<CronRow>>,
    /// id строк, снятых item_remove (cron rm).
    item_removes: Mutex<Vec<String>>,
    /// docId, отправленные в трэш (removeDocByID).
    removed_docs: Mutex<Vec<String>>,
    /// имена write-ahead журналов (putFile multipart).
    journal_files: Mutex<Vec<String>>,
    /// block get отвечает «док жив» (cron rm); false — привязка мертва.
    bodies_alive: Mutex<bool>,
}

/// Строка render крон-доски в мок-ядре.
struct CronRow {
    item: String,
    title: &'static str,
    last_run: &'static str,
}

fn full_keys(av: &str) -> serde_json::Value {
    let mut keys = vec![
        json!({ "id": "key-title", "name": "Первичный ключ" }),
        json!({ "id": "key-status", "name": "Status" }),
    ];
    if av == CRON_AV {
        keys.push(json!({ "id": "key-branch", "name": "Branch" }));
        keys.push(json!({ "id": "key-pr", "name": "PR" }));
    }
    keys.push(json!({ "id": "key-schedule", "name": "Schedule" }));
    keys.push(json!({ "id": "key-lastrun", "name": "Last Run" }));
    json!(keys)
}

fn template_keys(av: &str) -> serde_json::Value {
    // легаси-миграция: task-шаблон без Schedule/Last Run
    let keys = vec![
        json!({ "id": "key-title", "name": "Первичный ключ" }),
        json!({ "id": "key-status", "name": "Status" }),
        json!({ "id": "key-branch", "name": "Branch" }),
        json!({ "id": "key-pr", "name": "PR" }),
    ];
    let _ = av;
    json!(keys)
}

fn render_view(av: &str, kernel: &Kernel) -> serde_json::Value {
    let columns = full_keys(av).as_array().cloned().unwrap_or_default();
    let rows: Vec<serde_json::Value> = kernel
        .cron_rows
        .lock()
        .unwrap()
        .iter()
        .map(|row| {
            json!({
                "id": row.item,
                "cells": [
                    { "value": { "keyID": "key-title", "type": "block", "block": { "content": row.title, "refSubtype": "d", "id": format!("body-{}", row.item) } } },
                    { "value": { "keyID": "key-status", "type": "mSelect", "mSelect": [{ "content": "active" }] } },
                    { "value": { "keyID": "key-schedule", "type": "text", "text": { "content": "*/5 * * * *" } } },
                    { "value": { "keyID": "key-lastrun", "type": "text", "text": { "content": row.last_run } } }
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
                let av = match root {
                    CRON_DOC => CRON_AV,
                    FOIL_DOC => FOIL_AV,
                    _ => "",
                };
                if av.is_empty() {
                    json!([])
                } else {
                    json!([{
                        "id": format!("block-{root}"),
                        "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{av}\"></div>")
                    }])
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
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let p = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
            // тела кронов не существуют (нет сирот); доски — по одной
            let ids: Vec<&str> = match p {
                "/uwuwu/Кроны uwuwu" => vec![CRON_DOC],
                "/foilcap/Кроны foilcap" => vec![FOIL_DOC],
                _ => vec![],
            };
            ResponseTemplate::new(200)
                .set_body_string(json!({ "code": 0, "data": ids }).to_string())
        })
        .mount(server)
        .await;

    // createDocWithMd: тело крона; счётчик — детектор «сиротских» мутаций
    Mock::given(method("POST"))
        .and(path("/api/filetree/createDocWithMd"))
        .respond_with({
            let kernel = kernel.clone();
            let next_doc_id = Mutex::new(0usize);
            move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let hpath = body
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                kernel.created_docs.lock().unwrap().push(hpath);
                let id = format!("20261105120000-body{:02}", next_doc_id.lock().unwrap());
                ResponseTemplate::new(200)
                    .set_body_string(json!({ "code": 0, "data": id }).to_string())
            }
        })
        .mount(server)
        .await;

    // putFile: write-ahead undo-журнал (cron rm)
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

    // removeDocByID: тела в трэш (cron rm)
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
                        .append_header("mcp-session-id", "sess-cron");
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
                    let full = *kernel.cron_keys_full.lock().unwrap();
                    let keys = if av == CRON_AV && !full { template_keys(&av) } else { full_keys(&av) };
                    return mcp_text(json!({ "action": "keys", "data": { "keys": keys } }));
                }
                if action == "item_add" {
                    kernel.item_adds.lock().unwrap().push(args.clone());
                    let item = "20261105120000-item01";
                    if *kernel.render_confirms_insert.lock().unwrap() {
                        kernel.cron_rows.lock().unwrap().push(CronRow {
                            item: item.into(),
                            title: "новый крон",
                            last_run: "",
                        });
                    }
                    return mcp_text(json!({ "action": "item_add", "data": { "itemID": item } }));
                }
                if action == "item_update" {
                    let key = args.get("keyID").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let item = args.get("itemID").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let value = args.get("value").cloned().unwrap_or(json!(null));
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
                        .cron_rows
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
                        format!("ID: {av}\nType: NodeDocument\nHPath: /uwuwu/Кроны uwuwu/тело")
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
                    return mcp_text(render_view(&av, &kernel));
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

/// set через cron_set_with_pacing с мгновенным пейсингом: тесты не наследуют
/// прод-паузу verify-ретрая (пауза 1.5 s вынесена в RowInsertPacing).
async fn set_via_flow(server: &MockServer, title: &str) -> anyhow::Result<()> {
    set_env(server);
    let sy = SiYuan::from_env()?;
    cron_set_with_pacing(
        &sy,
        "uwuwu",
        title,
        "10m",
        "<промпт крона>",
        &RowInsertPacing::instant(),
    )
    .await
}

/// (1) легаси-доска без Schedule: громкий отказ, мутаций не было.
#[tokio::test]
#[serial_test::serial]
async fn set_bails_loud_before_mutations_when_board_lacks_schedule_key() {
    let kernel = Arc::new(Kernel::default());
    *kernel.cron_keys_full.lock().unwrap() = false;
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;

    let err = set_via_flow(&server, "ночной крон")
        .await
        .expect_err("доска без Schedule должна давать отказ");

    assert!(
        err.to_string().contains("Schedule"),
        "сообщение должно называть недостающую колонку: {err}"
    );
    assert!(
        kernel.created_docs.lock().unwrap().is_empty(),
        "тело не должно создаваться до валидации колонок"
    );
    assert!(
        kernel.item_adds.lock().unwrap().is_empty(),
        "строка не должна создаваться до валидации колонок"
    );
}

/// (2) полноценная доска: bound-строка, Schedule/Last Run текстом, Status mSelect.
#[tokio::test]
#[serial_test::serial]
async fn set_creates_bound_row_and_fills_cells() {
    let kernel = Arc::new(Kernel::default());
    *kernel.cron_keys_full.lock().unwrap() = true;
    *kernel.render_confirms_insert.lock().unwrap() = true;
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;

    set_via_flow(&server, "ночной крон")
        .await
        .expect("set на полной доске");

    let created = kernel.created_docs.lock().unwrap();
    assert_eq!(created.len(), 1, "ровно одно тело");
    assert!(
        created[0].contains("ночной крон"),
        "тело по hpath тайтла: {:?}",
        created[0]
    );

    let adds = kernel.item_adds.lock().unwrap();
    assert_eq!(adds.len(), 1, "одна вставка строки (verify с первого раза)");
    let bound = adds[0]
        .get("blockID")
        .and_then(|b| b.as_str())
        .unwrap_or("");
    assert!(
        !bound.is_empty(),
        "строка привязана к телу (blockID), не detached: {:?}",
        adds[0]
    );
    assert!(
        adds[0].get("detached").is_none(),
        "флаг detached не используется: {:?}",
        adds[0]
    );

    let updates = kernel.item_updates.lock().unwrap();
    let by_key = |name: &str| updates.iter().find(|(_, k, _, _)| k == name);
    let status = by_key("key-status").expect("Status записан");
    assert_eq!(
        status.3,
        json!({ "mSelect": [{ "content": "active" }] }),
        "Status через mSelect"
    );
    let schedule = by_key("key-schedule").expect("Schedule записан");
    assert_eq!(
        schedule.3,
        json!({ "text": { "content": "10m" } }),
        "Schedule текстом"
    );
    assert!(
        by_key("key-lastrun").is_some(),
        "Last Run записан (пустое значение)"
    );
}

/// (3) тайтл уже жив строкой: отказ без мутаций (дедуп по строкам, не только телам).
#[tokio::test]
#[serial_test::serial]
async fn set_rejects_duplicate_title_in_live_rows_without_mutation() {
    let kernel = Arc::new(Kernel::default());
    *kernel.cron_keys_full.lock().unwrap() = true;
    kernel.cron_rows.lock().unwrap().push(CronRow {
        item: "20261105110000-live1".into(),
        title: "ночной крон",
        last_run: "",
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;

    let err = set_via_flow(&server, "ночной крон")
        .await
        .expect_err("дубль-тайтл в живых строках");

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

/// (4) ядро «потеряло» вставку: после ретрая ×1 — громкий отказ.
#[tokio::test]
#[serial_test::serial]
async fn set_bails_when_row_never_appears_in_render() {
    let kernel = Arc::new(Kernel::default());
    *kernel.cron_keys_full.lock().unwrap() = true;
    *kernel.render_confirms_insert.lock().unwrap() = false;
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;

    let err = set_via_flow(&server, "призрачный крон")
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

/// (5) pause с --project пишет Status на доске указанного проекта.
#[tokio::test]
#[serial_test::serial]
async fn pause_targets_project_board() {
    let kernel = Arc::new(Kernel::default());
    *kernel.cron_keys_full.lock().unwrap() = true;
    kernel.cron_rows.lock().unwrap().push(CronRow {
        item: "20261105110000-kc1".into(),
        title: "крон foilcap",
        last_run: "",
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    cron_run(CronSub::Pause {
        id: "kc1".into(),
        project: Some("foilcap".into()),
    })
    .await
    .expect("pause на доске foilcap");

    let updates = kernel.item_updates.lock().unwrap();
    let status = updates
        .iter()
        .find(|(av, k, _, _)| *av == FOIL_AV && *k == "key-status")
        .expect("Status записан на доске foilcap");
    assert_eq!(
        status.3,
        json!({ "mSelect": [{ "content": "paused" }] }),
        "paused через mSelect"
    );
}

/// (6) таблица list несёт значения Schedule и Last Run.
#[test]
fn list_table_carries_schedule_and_last_run_values() {
    let rows = vec![SyTask {
        item_id: "20261105110000-kc9".into(),
        title: "утренний дайджест".into(),
        status: "active".into(),
        branch: String::new(),
        pr: String::new(),
        schedule: "09:30".into(),
        last_run: "2026-11-05 09:30".into(),
        dev_session: String::new(),
        body_doc: None,
    }];
    let table = format_cron_table(&rows);
    assert!(table.contains("09:30"), "Schedule виден: {table}");
    assert!(
        table.contains("2026-11-05 09:30"),
        "Last Run виден: {table}"
    );
    assert!(
        table.contains("утренний дайджест"),
        "заголовок виден: {table}"
    );
}

/// (7) парс: --project у всей семьи, дефолт — None (uwuwu).
#[test]
fn cron_family_accepts_project_flag() {
    use super::cli::{Cli, Commands};
    use clap::Parser as _;

    let mut argv: Vec<String> = vec!["uwuwu-cli".into()];
    argv.extend(["cron", "pause", "kc1", "--project", "foilcap"].map(String::from));
    let Commands::Cron { command } = Cli::try_parse_from(argv).unwrap().command else {
        panic!("expected cron");
    };
    let super::cli::CronSub::Pause { id, project } = command else {
        panic!("expected pause");
    };
    assert_eq!(id, "kc1");
    assert_eq!(project.as_deref(), Some("foilcap"));

    let mut argv: Vec<String> = vec!["uwuwu-cli".into()];
    argv.extend(["cron", "list"].map(String::from));
    let Commands::Cron { command } = Cli::try_parse_from(argv).unwrap().command else {
        panic!("expected cron");
    };
    let super::cli::CronSub::List { project } = command else {
        panic!("expected list");
    };
    assert!(project.is_none(), "дефолт — глобальная uwuwu");
}

// ——— cron rm (срез покрытия 20261105220435): журнал → строка → тело ———

fn rm_cmd(id: &str, project: Option<&str>) -> CronSub {
    CronSub::Rm {
        id: id.into(),
        project: project.map(String::from),
    }
}

fn seeded_row() -> CronRow {
    CronRow {
        item: "20261105120000-item01".into(),
        title: "ночной крон",
        last_run: "",
    }
}

/// cron rm: write-ahead undo-журнал ДО мутаций → item_remove строки →
/// тело (живая привязка) в трэш.
#[tokio::test]
#[serial_test::serial]
async fn rm_removes_row_and_trashes_body_with_undo_journal() {
    let kernel = Arc::new(Kernel::default());
    *kernel.bodies_alive.lock().unwrap() = true;
    kernel.cron_rows.lock().unwrap().push(seeded_row());
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    cron_run(rm_cmd("20261105120000-item01", None))
        .await
        .expect("rm на живом кроне");

    let removes = kernel.item_removes.lock().unwrap();
    assert_eq!(
        *removes,
        vec!["20261105120000-item01".to_string()],
        "строка снята item_remove"
    );
    let removed_docs = kernel.removed_docs.lock().unwrap();
    assert_eq!(
        *removed_docs,
        vec![format!("body-{}", removes[0])],
        "привязанное тело в трэш (removeDocByID)"
    );
    let journals = kernel.journal_files.lock().unwrap();
    assert_eq!(journals.len(), 1, "write-ahead журнал один");
    assert!(
        journals[0].ends_with("-uwuwu-cron-rm.json"),
        "журнал kind=cron-rm: {:?}",
        journals
    );
}

/// cron rm при мёртвой привязке тела: строка снимается, трэш не зовётся
/// (тела нет — удалять нечего), журнал всё равно пишется.
#[tokio::test]
#[serial_test::serial]
async fn rm_skips_body_trash_when_binding_is_dead() {
    let kernel = Arc::new(Kernel::default());
    kernel.cron_rows.lock().unwrap().push(seeded_row());
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    cron_run(rm_cmd("20261105120000-item01", None))
        .await
        .expect("rm проходит и с мёртвой привязкой");

    assert_eq!(
        *kernel.item_removes.lock().unwrap(),
        vec!["20261105120000-item01".to_string()],
        "строка снята"
    );
    assert!(
        kernel.removed_docs.lock().unwrap().is_empty(),
        "мёртвая привязка — removeDocByID не зовётся"
    );
    assert_eq!(
        kernel.journal_files.lock().unwrap().len(),
        1,
        "журнал написан"
    );
}
