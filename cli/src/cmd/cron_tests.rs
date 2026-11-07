//! cron list на wiremock-ядре (задача 20261106182051-o5ejlte): инвентарь
//! мастеров крон-задач — строки задачной доски с custom-schedule в доке-теле;
//! last run/сессия из state демона (UWUWU_CRON_STATE_DIR песочница).
//!
//! Сценарии:
//! 1. Вейт-строка с расписанием в атрибутах → строка таблицы с last run из state
//! 2. строки без атрибута расписания → в инвентарь не попадают
//! 3. пустой state (нет файла) → last run пуст, инвентарь работает
//! 4. атрибут расписания отсутствует у всех → «Крон-задач нет»
//! 5. сбои чтения атрибутов — громкая ошибка (инвентарь честный)

use crate::cmd::cli::{CronSub, TaskSub};
use crate::cmd::sy_dispatch::{cron_run, task_run};
use serde_json::json;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NB: &str = "nb-projects";
const BOARD_DOC: &str = "t-doc";
const BOARD_AV: &str = "av-t";

/// Атрибуты тел по docId (мок /api/attr/getBlockAttrs).
type Attrs = Vec<(String, String)>;

struct Kernel {
    /// docId тела → [(имя атрибута, значение)]
    attrs: Mutex<Vec<(String, Attrs)>>,
    /// (docId, name, value) записей /api/attr/setBlockAttrs.
    attr_writes: Mutex<Vec<(String, String, String)>>,
}

impl Kernel {
    fn attrs_of(&self, doc: &str) -> Option<Attrs> {
        self.attrs
            .lock()
            .unwrap()
            .iter()
            .find(|(d, _)| d == doc)
            .map(|(_, a)| a.clone())
    }
}

fn render_view() -> serde_json::Value {
    let columns = json!([
        { "id": "key-title", "name": "Первичный ключ" },
        { "id": "key-status", "name": "Status" }
    ]);
    let rows = json!([
        {
            "id": "20261107090000-master1",
            "cells": [
                { "value": { "keyID": "key-title", "type": "block", "block": { "content": "stable-ватчер SiYuan", "refSubtype": "d", "id": "body-master1" } } },
                { "value": { "keyID": "key-status", "type": "mSelect", "mSelect": [{ "content": "Вейт" }] } }
            ]
        },
        {
            "id": "20261107090000-plain2",
            "cells": [
                { "value": { "keyID": "key-title", "type": "block", "block": { "content": "обычная задача", "refSubtype": "d", "id": "body-plain2" } } },
                { "value": { "keyID": "key-status", "type": "mSelect", "mSelect": [{ "content": "Todo" }] } }
            ]
        }
    ]);
    json!({ "action": "render", "data": { "view": { "columns": columns, "rowCount": 2, "rows": rows } } })
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
    // SQL: доки по hpath (доска) + av-блок по root_id
    Mock::given(method("POST"))
        .and(path("/api/query/sql"))
        .respond_with(|req: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
            let data = if stmt.contains("getIDsByHPath-mock") {
                json!([])
            } else if let Some(root) =
                stmt.split("root_id='").nth(1).map(|s| s.split('\'').next().unwrap_or(""))
            {
                if root == BOARD_DOC {
                    json!([{ "id": "av-block", "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{BOARD_AV}\"></div>") }])
                } else {
                    json!([])
                }
            } else {
                json!([])
            };
            ResponseTemplate::new(200).set_body_string(json!({ "code": 0, "data": data }).to_string())
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/notebook/lsNotebooks"))
        .respond_with(|_req: &Request| {
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
            let ids: Vec<&str> = if p == "/uwuwu/Задачи uwuwu" {
                vec![BOARD_DOC]
            } else {
                vec![]
            };
            ResponseTemplate::new(200)
                .set_body_string(json!({ "code": 0, "data": ids }).to_string())
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/attr/setBlockAttrs"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let id = body
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let attrs = body.get("attrs").cloned().unwrap_or_default();
                if let Some((name, value)) =
                    attrs.as_object().map(|m| m.into_iter().next()).flatten()
                {
                    kernel.attr_writes.lock().unwrap().push((
                        id,
                        name.clone(),
                        value.as_str().unwrap_or("").to_string(),
                    ));
                }
                ResponseTemplate::new(200)
                    .set_body_string(json!({ "code": 0, "msg": "", "data": null }).to_string())
            }
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/attr/getBlockAttrs"))
        .respond_with({
            let kernel = kernel.clone();
            move |req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
                match kernel.attrs_of(id) {
                    Some(attrs) => {
                        let map: serde_json::Map<String, serde_json::Value> =
                            attrs.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
                        ResponseTemplate::new(200)
                            .set_body_string(json!({ "code": 0, "data": map }).to_string())
                    }
                    None => ResponseTemplate::new(200).set_body_string(
                        json!({ "code": -1, "msg": "block not found" }).to_string(),
                    ),
                }
            }
        })
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(move |req: &Request| {
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
            let parsed: serde_json::Value = body.parse().unwrap_or_default();
            let action = parsed
                .pointer("/params/arguments/action")
                .and_then(|a| a.as_str())
                .unwrap_or("");
            if action == "render" {
                return mcp_text(render_view());
            }
            if action == "get" {
                // block get для doc_block_exists: привязанные тела живы
                return mcp_text(json!({ "action": "get", "data": "Type: NodeDocument" }));
            }
            mcp_text(json!({ "action": "unknown", "data": null }))
        })
        .mount(server)
        .await;
}

fn set_env(server: &MockServer) {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
}

fn list_cmd(project: Option<&str>) -> CronSub {
    CronSub::List {
        project: project.map(String::from),
    }
}

fn state_dir_with(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    std::fs::write(dir.path().join("state/state.json"), content).unwrap();
    std::env::set_var("UWUWU_CRON_STATE_DIR", dir.path().join("state"));
    dir
}

/// (1) Вейт-строка с расписанием → инвентарь без отказа; state с живой
/// записью мастера читается (last run/сессия дополняют таблицу).
#[tokio::test]
#[serial_test::serial]
async fn list_shows_master_with_schedule_and_last_run_from_state() {
    let kernel = Arc::new(Kernel {
        attr_writes: Mutex::new(vec![]),
        attrs: Mutex::new(vec![(
            "body-master1".into(),
            vec![("custom-schedule".to_string(), "6h".to_string())],
        )]),
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);
    let _state = state_dir_with(
        r#"{ "crons": { "20261107090000-master1": { "sessionID": "ses_abc123def", "lastFiredAt": 1762498200000, "firedAtIso": "2026-11-07T09:30:00+09:00" } } }"#,
    );

    cron_run(list_cmd(None))
        .await
        .expect("list: мастер найден, state прочитан");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (2) Строка без атрибута расписания в инвентарь не попадает: таблица
/// печатает только мастеров (мок: у обеих строк атрибутов нет → пусто).
#[tokio::test]
#[serial_test::serial]
async fn list_skips_rows_without_schedule_attr() {
    let kernel = Arc::new(Kernel {
        attr_writes: Mutex::new(vec![]),
        attrs: Mutex::new(vec![(
            "body-plain2".into(),
            vec![("custom-title".to_string(), "не расписание".to_string())],
        )]),
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    cron_run(list_cmd(None)).await.expect("list ok");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (3) State отсутствует — инвентарь работает без last run.
#[tokio::test]
#[serial_test::serial]
async fn list_works_without_state_file() {
    let kernel = Arc::new(Kernel {
        attr_writes: Mutex::new(vec![]),
        attrs: Mutex::new(vec![(
            "body-master1".into(),
            vec![("custom-schedule".to_string(), "*/5 * * * *".to_string())],
        )]),
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("UWUWU_CRON_STATE_DIR", dir.path().join("несуществующее"));

    cron_run(list_cmd(None)).await.expect("list без state ok");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (4) Ни у одной строки нет расписания → честное «нет» вместо пустой таблицы.
#[tokio::test]
#[serial_test::serial]
async fn list_reports_empty_inventory_without_masters() {
    let kernel = Arc::new(Kernel {
        attr_writes: Mutex::new(vec![]),
        attrs: Mutex::new(vec![]),
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    cron_run(list_cmd(None)).await.expect("пустой инвентарь ok");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (5) Мёртвая привязка тела одной строки — скип с предупреждением,
/// остальной инвентарь живёт (инвентарь не роняется одной битой строкой).
#[tokio::test]
#[serial_test::serial]
async fn list_skips_row_with_dead_binding_and_lists_the_rest() {
    // attrs-мок знает только тело обычной строки: master1 (unknown attrs)
    // пропускается, plain2 с расписанием попадает в инвентарь
    let kernel = Arc::new(Kernel {
        attr_writes: Mutex::new(vec![]),
        attrs: Mutex::new(vec![(
            "body-plain2".into(),
            vec![("custom-schedule".to_string(), "30m".to_string())],
        )]),
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    cron_run(list_cmd(None))
        .await
        .expect("битая строка не роняет инвентарь");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (6) Доска проекта не найдена — отказ с именем доски.
#[tokio::test]
#[serial_test::serial]
async fn list_bails_when_board_missing() {
    let kernel = Arc::new(Kernel {
        attr_writes: Mutex::new(vec![]),
        attrs: Mutex::new(vec![]),
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = cron_run(list_cmd(Some("неттакого")))
        .await
        .expect_err("доска не найдена — отказ");
    assert!(
        err.to_string().contains("Задачи неттакого"),
        "ошибка называет доску: {err}"
    );
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (7) Формат last run: локальное «YYYY-MM-DD HH:MM» из unix-ms state.
#[test]
fn format_local_renders_local_time() {
    // Формат не зависит от зоны прогона: проверяем шейп (длина, разделители)
    let s = crate::cmd::sy_dispatch::format_local(1762477800000);
    assert_eq!(s.len(), 16, "«YYYY-MM-DD HH:MM»: {s}");
    assert_eq!(s.as_bytes()[10], b' ', "пробел между датой и временем: {s}");
    assert_eq!(s.as_bytes()[13], b':', "двоеточие в времени: {s}");
}

/// (8) task edit --schedule "6h": грамматика валидирована, атрибут записан.
#[tokio::test]
#[serial_test::serial]
async fn edit_schedule_writes_custom_attr() {
    let kernel = Arc::new(Kernel {
        attr_writes: Mutex::new(vec![]),
        attrs: Mutex::new(vec![(
            "body-plain2".into(),
            vec![("custom-title".to_string(), "не расписание".to_string())],
        )]),
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Edit {
            id: "20261107090000-plain2".into(),
            status: None,
            title: None,
            pr: None,
            branch: None,
            schedule: Some("6h".into()),
        },
    )
    .await
    .expect("edit --schedule ok");

    let writes = kernel.attr_writes.lock().unwrap();
    assert_eq!(writes.len(), 1, "ровно одна запись атрибута: {writes:?}");
    assert_eq!(writes[0].0, "body-plain2", "атрибут на теле строки");
    assert_eq!(writes[0].1, "custom-schedule");
    assert_eq!(writes[0].2, "6h");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (9) task edit --schedule "": снятие без валидации грамматики — контракт
/// «"" — снять» (ревью Common-1: ветка сидела за validate_schedule и была
/// недостижима).
#[tokio::test]
#[serial_test::serial]
async fn edit_schedule_empty_removes_attr_without_grammar_check() {
    let kernel = Arc::new(Kernel {
        attr_writes: Mutex::new(vec![]),
        attrs: Mutex::new(vec![]),
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Edit {
            id: "20261107090000-plain2".into(),
            status: None,
            title: None,
            pr: None,
            branch: None,
            schedule: Some("".into()),
        },
    )
    .await
    .expect("снятие расписания работает");

    let writes = kernel.attr_writes.lock().unwrap();
    assert_eq!(
        writes.len(),
        1,
        "снятие = запись пустого значения: {writes:?}"
    );
    assert_eq!(writes[0].1, "custom-schedule");
    assert_eq!(
        writes[0].2, "",
        "пустое значение — ядро удаляет ключ из IAL"
    );
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (10) task edit --schedule "мусор": грамматика отвергает ДО записи —
/// атрибут не тронут.
#[tokio::test]
#[serial_test::serial]
async fn edit_schedule_garbage_rejected_before_write() {
    let kernel = Arc::new(Kernel {
        attr_writes: Mutex::new(vec![]),
        attrs: Mutex::new(vec![]),
    });
    let server = MockServer::start().await;
    mount_kernel(&server, &kernel).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Edit {
            id: "20261107090000-plain2".into(),
            status: None,
            title: None,
            pr: None,
            branch: None,
            schedule: Some("раз в минутку".into()),
        },
    )
    .await
    .expect_err("мусорное расписание — отказ");

    assert!(
        err.to_string().contains("не разобрано"),
        "ошибка валидации грамматики: {err}"
    );
    assert!(
        kernel.attr_writes.lock().unwrap().is_empty(),
        "записи не было: атрибут не тронут"
    );
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}
