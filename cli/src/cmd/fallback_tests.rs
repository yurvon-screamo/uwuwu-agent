//! Резолв task-команд: доска задач проекта (задача 20261106182051-o5ejlte,
//! крон-доски сняты) + глобальный резолв без --project (доски всех проектов,
//! уникальность обязательна — инцидент 05.11 «потерянный вопрос»). Легаси
//! крон-доска в мок-ядре остаётся намеренно: тесты доказывают, что она не
//! обыскивается ни явным, ни глобальным резолвом.
//!
//! Сценарии:
//! 1. get точного id задачи с --project — резолв по «Задачи», чужие доски не читаются
//! 2. строка только на легаси крон-доске — НЕ находится (ошибка называет «Задачи»)
//! 3. полный промах с --project — ошибка перечисляет только доску задач
//! 4. неоднозначный префикс на «Задачи» — громкая раскладка без крон-досок
//! 5. доска «Задачи» отсутствует — отказ «доска не найдена»
//! 6. сбой SQL ядра — громкая ошибка, не «не найдена»
//! 7. get точного id без --project — глобальный резолв (фаза A)
//! 8. comment без --project на подстроку чужой доски — приземлился (инцидент 05.11)
//! 9. подстрока на двух досках — громкая ошибка с раскладкой обеих
//! 10. неоднозначность на одной доске без --project — громкая ошибка
//! 11. полный промах без --project — раскладка обысканных досок
//! 12. list без --project — прежний отказ «--project обязателен»

use crate::cmd::cli::TaskSub;
use crate::cmd::sy_dispatch::task_run;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NB: &str = "nb-projects";

/// Доски мок-ядра: doc → (av, av-носитель). CRON_* — легаси-доска для
/// негативных тестов (в воркспейсе до сноса). ORIGA_* — вторая задачная
/// доска (глобальный резолв без --project).
const TASKS_DOC: &str = "t-doc";
const TASKS_AV: &str = "av-t";
const ORIGA_DOC: &str = "o-doc";
const ORIGA_AV: &str = "av-o";
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

const ORIGA_ROWS: &[Row] = &[Row {
    item: "20261105110000-otask1",
    title: "origa-only задача",
    status: "Todo",
    doc: Some("20261105000003-obodyx3"),
}];

fn rows_for_av(av: &str) -> &'static [Row] {
    match av {
        TASKS_AV => TASKS_ROWS,
        ORIGA_AV => ORIGA_ROWS,
        _ => CRON_ROWS,
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

/// id дока → doc-id-цепочка для getIDsByHPath: все три доски существуют
/// (легаси-воркспейс до сноса крон-досок + вторая задачная доска origa),
/// тела задач — саб-доки досок.
fn doc_ids_by_path(p: &str) -> Vec<&'static str> {
    match p {
        "/uwuwu/Задачи uwuwu" => vec![TASKS_DOC],
        "/origa/Задачи origa" => vec![ORIGA_DOC],
        "/uwuwu/Кроны uwuwu" => vec![CRON_DOC],
        "/uwuwu/Задачи uwuwu/обычная задача" => vec!["20261105000001-tbodyx1"],
        "/origa/Задачи origa/origa-only задача" => vec!["20261105000003-obodyx3"],
        _ => vec![],
    }
}

/// Живые дока-блока (MCP block get): тела задач трёх досок.
fn doc_block_alive(id: &str) -> bool {
    matches!(
        id,
        "20261105000001-tbodyx1" | "20261105000002-kbodyx2" | "20261105000003-obodyx3"
    )
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
                // дискавери досок задач (глобальный резолв без --project):
                // обе задачные доски, легаси-крон-доска не «Задачи %»
                if stmt.contains("content like 'Задачи %'") {
                    let data = serde_json::json!([
                        { "id": TASKS_DOC, "hpath": "/uwuwu/Задачи uwuwu" },
                        { "id": ORIGA_DOC, "hpath": "/origa/Задачи origa" }
                    ]);
                    return ResponseTemplate::new(200)
                        .set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string());
                }
                let data = if let Some(root) = stmt
                    .split("root_id='")
                    .nth(1)
                    .map(|s| s.split('\'').next().unwrap_or(""))
                {
                    let (doc, av) = match root {
                        TASKS_DOC => (TASKS_DOC, TASKS_AV),
                        ORIGA_DOC => (ORIGA_DOC, ORIGA_AV),
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
                if action == "get" {
                    // проба живости привязки строки (doc_block_exists):
                    // живой док — дамп с «Type: NodeDocument», мёртвый — промах
                    let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    if doc_block_alive(id) {
                        return mcp_text(serde_json::json!({
                            "action": "get", "data": format!("Type: NodeDocument\nID: {id}")
                        }));
                    }
                    return ResponseTemplate::new(200).set_body_string(
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": 2,
                            "result": { "content": [{ "type": "text",
                                "text": "{\"code\":-1,\"msg\":\"block not found\"}" }] }
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

/// Общий пролог сценариев: мок-ядро с досками файла + env. Сервер живёт
/// в возвращаемой паре до конца теста (drop убил бы мок до task_run).
async fn spawn_kernel() -> (std::sync::Arc<Kernel>, MockServer) {
    let kernel = Kernel::new();
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);
    (kernel, server)
}

/// (1) get по точному id задачи — резолв по «Задачи»; крон-доска не читается.
#[tokio::test]
#[serial_test::serial]
async fn get_resolves_on_tasks_board_without_cron_reads() {
    let (kernel, _server) = spawn_kernel().await;

    task_run(
        "uwuwu",
        TaskSub::Get {
            id: "20261105100000-ttask1".into(),
            no_body: true,
        },
    )
    .await
    .expect("задача резолвится штатно");

    assert!(kernel.render_count(TASKS_AV) > 0, "доска задач прочитана");
    assert_eq!(
        kernel.render_count(CRON_AV),
        0,
        "легаси крон-доска не обыскивается"
    );
    assert_eq!(
        kernel.render_count(ORIGA_AV),
        0,
        "чужая задачная доска при явном --project не читается"
    );
}

/// (2) Строка только на легаси крон-доске — НЕ находится: ошибка называет
/// доску задач (крон-строки мертвы вместе с контуром).
#[tokio::test]
#[serial_test::serial]
async fn cron_board_row_is_not_resolved_anymore() {
    let (kernel, _server) = spawn_kernel().await;

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
    let (kernel, _server) = spawn_kernel().await;

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
    assert!(
        msg.contains("Задачи uwuwu"),
        "обысканная доска в ошибке: {msg}"
    );
    assert!(
        !msg.contains("Кроны"),
        "крон-доски в обыске не участвуют: {msg}"
    );
}

/// (4) Неоднозначный префикс на «Задачи» — громкая раскладка; крон-доски
/// в раскладке не фигурируют.
#[tokio::test]
#[serial_test::serial]
async fn ambiguous_prefix_is_loud_without_cron_boards() {
    let (kernel, _server) = spawn_kernel().await;

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
    let (kernel, _server) = spawn_kernel().await;

    let err = task_run(
        "foilcap",
        TaskSub::Get {
            id: "20261105100000-ttask1".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("доски задач foilcap нет");

    let msg = err.to_string();
    assert!(
        msg.contains("Задачи foilcap") && msg.contains("не найдена"),
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
    let (kernel, _server) = spawn_kernel().await;
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

/// (7) Глобальный резолв: точный id без --project — резолвится (фаза A).
#[tokio::test]
#[serial_test::serial]
async fn global_resolve_exact_id_without_project() {
    let (kernel, _server) = spawn_kernel().await;

    task_run(
        "",
        TaskSub::Get {
            id: "20261105100000-ttask1".into(),
            no_body: true,
        },
    )
    .await
    .expect("точный id резолвится глобально");
}

/// (8) comment без --project на подстроку задачи чужой доски — коммент
/// приземлился в её тело (юзкейс инцидента 05.11: вопрос потерялся).
#[tokio::test]
#[serial_test::serial]
async fn global_comment_without_project_lands_on_owning_board() {
    let (kernel, _server) = spawn_kernel().await;

    task_run(
        "",
        TaskSub::Comment {
            id: "origa-only".into(),
            reply: None,
            stdin: false,
            text: vec!["вопрос владельцу: ЧТО РЕШИТЬ?".into()],
        },
    )
    .await
    .expect("коммент приземлился глобально");

    let appends = kernel.appends_snapshot();
    // секция «## Комменты» создаётся лениво (первый append) + строка коммента
    assert_eq!(appends.len(), 2, "секция + коммент");
    assert!(
        appends
            .iter()
            .all(|a| a.contains("20261105000003-obodyx3")),
        "оба appendBlock — в тело origa-задачи: {appends:?}"
    );
    assert!(
        appends.iter().any(|a| a.contains("вопрос владельцу")),
        "текст коммента в append: {appends:?}"
    );
}

/// (9) Подстрока матчится на двух досках — громкая ошибка с раскладкой
/// обеих (тихий wrong-target исключён конструктивно).
#[tokio::test]
#[serial_test::serial]
async fn global_substring_on_two_boards_is_loud_error() {
    let (kernel, _server) = spawn_kernel().await;

    let err = task_run(
        "",
        TaskSub::Get {
            id: "задача".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("подстрока на двух досках — неоднозначно");

    let msg = err.to_string();
    assert!(
        msg.contains("найдено на") && msg.contains("Задачи uwuwu") && msg.contains("Задачи origa"),
        "раскладка называет обе доски: {msg}"
    );
}

/// (10) Неоднозначность на одной доске без --project — громкая ошибка
/// с раскладкой, чужая доска в ней не фигурирует.
#[tokio::test]
#[serial_test::serial]
async fn global_ambiguity_on_single_board_is_loud_error() {
    let (kernel, _server) = spawn_kernel().await;

    let err = task_run(
        "",
        TaskSub::Get {
            id: "alpha".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("двусмысленный фрагмент на одной доске");

    let msg = err.to_string();
    assert!(
        msg.contains("неоднозначно на") && msg.contains("Задачи uwuwu"),
        "раскладка с доской и причиной: {msg}"
    );
    assert!(
        !msg.contains("найдено на"),
        "чужих матчей нет — «найдено на» не выводится: {msg}"
    );
}

/// (11) Полный промах без --project — ошибка перечисляет обысканные доски.
#[tokio::test]
#[serial_test::serial]
async fn global_miss_lists_searched_boards() {
    let (kernel, _server) = spawn_kernel().await;

    let err = task_run(
        "",
        TaskSub::Get {
            id: "несуществующаяxyz".into(),
            no_body: true,
        },
    )
    .await
    .expect_err("полный промах");

    let msg = err.to_string();
    assert!(
        msg.contains("не найдена") && msg.contains("обысканы доски"),
        "честный промах с перечнем: {msg}"
    );
    assert!(
        msg.contains("Задачи uwuwu") && msg.contains("Задачи origa"),
        "обе задачные доски обысканы: {msg}"
    );
    assert!(
        !msg.contains("Кроны"),
        "крон-доски в глобальном обыске не участвуют: {msg}"
    );
}

/// (12) list без --project — прежний отказ (board-bound, глобальной
/// семантики нет).
#[tokio::test]
#[serial_test::serial]
async fn list_without_project_still_refuses() {
    let (kernel, _server) = spawn_kernel().await;

    let err = task_run(
        "",
        TaskSub::List {
            status: None,
            r#type: None,
            project: None,
        },
    )
    .await
    .expect_err("list без проекта — отказ");

    let msg = err.to_string();
    assert!(
        msg.contains("--project"),
        "отказ требует флаг проекта: {msg}"
    );
    assert_eq!(
        kernel.render_count(TASKS_AV) + kernel.render_count(ORIGA_AV),
        0,
        "до отказа доски не читаются"
    );
}
