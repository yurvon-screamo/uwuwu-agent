//! Контракт-тест формы executor-API (#1254): дескрипторы ответов мока
//! сверяются с пином живой формы opencode 2.3 (эталоны пробы #1249
//! поле-в-поле). Рассинхрон мок↔пин — красный с диффом: класс
//! инцидента «строки стоят, сессии спят» (#1249) ловится тестом, а не
//! ночной охотой.
//!
//! Цепочка правды: живая проба (ранбук «Проба формы executor-API
//! (пин-файл)») → пин-файл → дескриптор ответа мока. Каждая ось пина
//! закрыта собственной пробой; пин описывает дефолтную форму —
//! env-ручки-режимы стенда вне контракта.

mod common;

use common::form::{time_keys, Diff, Pin};
use common::{create_session_with, spawn_mock, wait_ready};
use serde_json::{json, Value};

async fn get_json(http: &reqwest::Client, url: &str) -> Value {
    http.get(url)
        .send()
        .await
        .expect("probe request")
        .json()
        .await
        .expect("probe response json")
}

#[tokio::test]
async fn reply_tail_matches_pin_assistant_shape() {
    let (base, _mock) = spawn_mock(&[]);
    wait_ready(&base).await;
    let http = reqwest::Client::new();
    let pin = Pin::load();

    let id = create_session_with(&base, &http, json!({ "title": "form probe" })).await;
    http.post(format!("{base}/api/session/{id}/prompt"))
        .json(&json!({ "text": "финальный ответ формы" }))
        .send()
        .await
        .expect("prompt");
    let tail = get_json(
        &http,
        &format!("{base}/api/session/{id}/message?limit=1&order=desc"),
    )
    .await;

    let mut d = Diff::default();
    d.expect(
        &format!("message_tail.wrapper[{}]", pin.message_wrapper),
        tail["data"].is_array(),
        "хвост не обёрнут в {data:[...]}",
    );
    let msgs = tail["data"].as_array().cloned().unwrap_or_default();
    d.expect(
        "message_tail.assistant_present",
        !msgs.is_empty(),
        "хвост пуст",
    );
    for msg in &msgs {
        let leaked: Vec<String> = time_keys(msg)
            .into_iter()
            .filter(|k| pin.forbidden_time.contains(k))
            .collect();
        d.field_set("message_tail.forbidden_time_fields", &[], &leaked);
    }
    let assistant = msgs.iter().find(|m| m["type"] == "assistant").cloned();
    if let Some(msg) = assistant {
        d.field_set(
            "message_tail.assistant_final_time",
            &pin.assistant_final_time,
            &time_keys(&msg),
        );
        d.expect(
            "message_tail.assistant_finish",
            msg["finish"] == "stop",
            "assistant-ответ без finish=stop",
        );
    } else {
        d.expect(
            "message_tail.assistant_present",
            false,
            "assistant-события нет в хвосте",
        );
    }
    d.finish("message-хвост (reply)");
}

#[tokio::test]
async fn idle_tail_matches_pin_idle_shape() {
    let (base, _mock) = spawn_mock(&[]);
    wait_ready(&base).await;
    let http = reqwest::Client::new();
    let pin = Pin::load();

    // сессия без промпта: хвост несёт idle-событие (ответа ещё нет)
    let id = create_session_with(&base, &http, json!({ "title": "idle probe" })).await;
    let tail = get_json(
        &http,
        &format!("{base}/api/session/{id}/message?limit=1&order=desc"),
    )
    .await;

    let mut d = Diff::default();
    d.expect(
        &format!("message_tail.wrapper[{}]", pin.message_wrapper),
        tail["data"].is_array(),
        "хвост не обёрнут в {data:[...]}",
    );
    let head = tail["data"].as_array().and_then(|m| m.first()).cloned();
    d.expect(
        "message_tail.idle_present",
        head.as_ref().map(|m| m["type"] == "idle").unwrap_or(false),
        "хвост без ответа не открывается idle-событием",
    );
    if let Some(msg) = head {
        d.field_set("message_tail.idle_time", &pin.idle_time, &time_keys(&msg));
        let leaked: Vec<String> = time_keys(&msg)
            .into_iter()
            .filter(|k| pin.forbidden_time.contains(k))
            .collect();
        d.field_set("message_tail.forbidden_time_fields", &[], &leaked);
    }
    d.finish("message-хвост (idle)");
}

#[tokio::test]
async fn turn_streaming_window_matches_pin_time_set() {
    // окна turn-режима растянуты env-ручкой: проба детерминированно
    // попадает в стрим-фазу (до завершения хода)
    let (base, _mock) = spawn_mock(&[
        ("MOCK_TURN_STREAM_MS", "20000"),
        ("MOCK_TURN_TRAP_MS", "30000"),
    ]);
    wait_ready(&base).await;
    let http = reqwest::Client::new();
    let pin = Pin::load();

    let id = create_session_with(&base, &http, json!({ "title": "turn probe" })).await;
    http.post(format!("{base}/api/session/{id}/prompt"))
        .json(&json!({ "text": "turn probe" }))
        .send()
        .await
        .expect("prompt");
    let tail = get_json(
        &http,
        &format!("{base}/api/session/{id}/message?limit=1&order=desc"),
    )
    .await;

    let mut d = Diff::default();
    d.expect(
        &format!("message_tail.wrapper[{}]", pin.message_wrapper),
        tail["data"].is_array(),
        "хвост не обёрнут в {data:[...]}",
    );
    let head = tail["data"].as_array().and_then(|m| m.first()).cloned();
    d.expect(
        "message_tail.streaming_present",
        head.as_ref()
            .map(|m| m["type"] == "assistant")
            .unwrap_or(false),
        "стрим-фаза не несёт assistant-события",
    );
    if let Some(msg) = head {
        if msg["finish"] != "stop" {
            d.field_set(
                "message_tail.assistant_streaming_time",
                &pin.assistant_streaming_time,
                &time_keys(&msg),
            );
            let leaked: Vec<String> = time_keys(&msg)
                .into_iter()
                .filter(|k| pin.forbidden_time.contains(k))
                .collect();
            d.field_set("message_tail.forbidden_time_fields", &[], &leaked);
        }
    }
    d.finish("message-хвост (turn стрим)");
}

#[tokio::test]
async fn turn_final_window_matches_pin_completed_time() {
    // окна сжаты env-ручкой: ход завершается за время пробы — хвост
    // несёт финал (finish=stop, completed в time)
    let (base, _mock) = spawn_mock(&[("MOCK_TURN_STREAM_MS", "100"), ("MOCK_TURN_TRAP_MS", "400")]);
    wait_ready(&base).await;
    let http = reqwest::Client::new();
    let pin = Pin::load();

    let id = create_session_with(&base, &http, json!({ "title": "turn final probe" })).await;
    http.post(format!("{base}/api/session/{id}/prompt"))
        .json(&json!({ "text": "turn probe" }))
        .send()
        .await
        .expect("prompt");
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let tail = get_json(
        &http,
        &format!("{base}/api/session/{id}/message?limit=1&order=desc"),
    )
    .await;

    let mut d = Diff::default();
    d.expect(
        &format!("message_tail.wrapper[{}]", pin.message_wrapper),
        tail["data"].is_array(),
        "хвост не обёрнут в {data:[...]}",
    );
    let head = tail["data"].as_array().and_then(|m| m.first()).cloned();
    let is_final = head
        .as_ref()
        .map(|m| m["type"] == "assistant" && m["finish"] == "stop")
        .unwrap_or(false);
    d.expect(
        "message_tail.turn_final_present",
        is_final,
        "финал turn-хода не пришёл (finish=stop у головы нет)",
    );
    if let Some(msg) = head {
        if msg["finish"] == "stop" {
            d.field_set(
                "message_tail.assistant_final_time",
                &pin.assistant_final_time,
                &time_keys(&msg),
            );
            let leaked: Vec<String> = time_keys(&msg)
                .into_iter()
                .filter(|k| pin.forbidden_time.contains(k))
                .collect();
            d.field_set("message_tail.forbidden_time_fields", &[], &leaked);
        }
    }
    d.finish("message-хвост (turn финал)");
}

#[tokio::test]
async fn session_list_matches_pin_wrapper_and_agent_semantics() {
    let (base, _mock) = spawn_mock(&[]);
    wait_ready(&base).await;
    let http = reqwest::Client::new();
    let pin = Pin::load();

    create_session_with(&base, &http, json!({ "title": "default probe" })).await;
    create_session_with(
        &base,
        &http,
        json!({ "title": "role probe", "agent": "dreamer" }),
    )
    .await;
    let list = get_json(&http, &format!("{base}/api/session")).await;

    let mut d = Diff::default();
    d.expect(
        &format!("session_list.wrapper[{}]", pin.list_wrapper),
        list["data"].is_array(),
        "список не обёрнут в {data:[...]}",
    );
    d.expect(
        &format!("session_list.cursor[{}]", pin.list_cursor),
        list["cursor"].is_object(),
        "cursor-объекта нет",
    );
    let items = list["data"].as_array().cloned().unwrap_or_default();
    d.expect(
        "session_list.items_present",
        !items.is_empty(),
        "список пуст",
    );
    for item in &items {
        d.field_set("session_list.item_time", &pin.item_time, &time_keys(item));
        let agent_present = item
            .as_object()
            .map(|o| o.contains_key("agent"))
            .unwrap_or(false);
        if agent_present {
            d.expect(
                &format!(
                    "session_list.role_session.agent_key[{}]",
                    pin.role_agent_key
                ),
                item["agent"].is_string(),
                "agent-ключ есть, но не строка",
            );
        } else {
            d.expect(
                &format!(
                    "session_list.default_session.outcome[{}]",
                    pin.default_outcome
                ),
                item.get("outcome").map(Value::is_null).unwrap_or(false),
                "outcome у default-сессии не null",
            );
        }
    }
    d.expect(
        &format!(
            "session_list.role_session.agent_key[{}]",
            pin.role_agent_key
        ),
        items.iter().any(|i| i["agent"].as_str() == Some("dreamer")),
        "role-сессия без строкового agent-ключа",
    );
    // Направленные оси default-сессии: в общем цикле они живут только в
    // else-ветке (agent-ключа нет) — дрейф «строковый agent у всех
    // (включая default)» оставлял бы обе оси непроверенными (мутация
    // ревью #1254). Гвард: probe-сессия ищется по title, её отсутствие —
    // тоже красный.
    let default_item = items.iter().find(|i| i["title"] == "default probe");
    d.expect(
        "session_list.default_present",
        default_item.is_some(),
        "default-probe-сессии нет в списке",
    );
    if let Some(item) = default_item {
        let agent_present = item
            .as_object()
            .map(|o| o.contains_key("agent"))
            .unwrap_or(false);
        d.expect(
            &format!(
                "session_list.default_session.agent_key[{}]",
                pin.default_agent_key
            ),
            !agent_present,
            "agent-ключ у default-сессии (пин заявляет absent)",
        );
        d.expect(
            &format!(
                "session_list.default_session.outcome[{}]",
                pin.default_outcome
            ),
            item.get("outcome").map(Value::is_null).unwrap_or(false),
            "outcome у default-сессии не null",
        );
    }
    d.finish("session-list");
}

// ---------- юнит-пины: носитель пина и дифф-механика ----------

#[test]
fn pin_carries_every_contract_axis() {
    let pin = Pin::load();
    assert_eq!(pin.message_wrapper, "data");
    assert_eq!(pin.list_wrapper, "data");
    assert!(pin.list_cursor, "ось cursor отсутствует в пине");
    assert!(
        pin.forbidden_time.contains(&"updated".to_string()),
        "forbidden-ось без updated — страж инцидента #1249 потерян"
    );
    assert!(pin.assistant_final_time.contains(&"completed".to_string()));
    assert!(pin.assistant_final_time.contains(&"created".to_string()));
    assert!(
        !pin.assistant_streaming_time
            .contains(&"completed".to_string()),
        "стрим-ось не различает фазы хода"
    );
    assert!(pin
        .assistant_streaming_time
        .contains(&"streamed".to_string()));
    assert_eq!(pin.idle_time, vec!["created".to_string()]);
    assert!(
        pin.item_time.contains(&"busy".to_string()) && pin.item_time.contains(&"idle".to_string())
    );
    assert_eq!(pin.default_agent_key, "absent");
    assert_eq!(pin.default_outcome, "null");
    assert_eq!(pin.role_agent_key, "string");
}

#[test]
fn diff_reports_axis_with_both_sides() {
    let mut d = Diff::default();
    d.field_set("axis", &["a".into()], &["b".into()]);
    d.expect("flag", false, "нарушение");
    let report = format!("{:?}", d);
    assert!(report.contains("axis"), "имя оси потеряно: {report}");
}

#[test]
fn diff_silent_when_field_sets_equal() {
    let mut d = Diff::default();
    d.field_set("axis", &["a".into(), "b".into()], &["b".into(), "a".into()]);
    d.expect("flag", true, "нарушение");
    d.finish("поверхность");
}

#[test]
fn time_keys_sorted_or_empty() {
    let msg = json!({ "time": { "streamed": 1, "created": 2 } });
    assert_eq!(
        time_keys(&msg),
        vec!["created".to_string(), "streamed".to_string()]
    );
    assert!(time_keys(&json!({})).is_empty());
}
