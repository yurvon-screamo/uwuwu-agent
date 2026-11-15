//! mock-opencode: эмулятор исполнителя для безопасного дебага конвейера
//! uwuwu-server (без LLM, без Railway, на лаптопе). Контракты — 1:1 с
//! OpencodeClient (crates/server/src/opencode.rs, openapi 2.3.0).
//!
//! Сценарии (MOCK_MODE):
//!   plain  — сессии живут, промпты обновляют активность (wakeup-тесты)
//!   agent  — «агент» отрабатывает по роли: коммент+task_edit через MCP
//!            uwuwu (полный цикл конвейера без LLM), outcome succeeded
//!   storm  — сессии мгновенно прерываются с 0 токенов (пустышки —
//!            проверка SpawnLoop/attention/дример-фильтра)
//!   dead   — созданные сессии сразу 404 (dead-respawn путь)
//!
//! Env: MOCK_MODE (деф. plain), MOCK_PORT (8099), MOCK_STEP_MS (1500),
//!      MOCK_MCP_URL (http://127.0.0.1:8090), MOCK_MCP_TOKEN («»),
//!      MOCK_VERDICTS («plan:request_changes;result:request_changes;...») —
//!      сценарные вердикты ревью-стадий по порядку (ре-энтри-циклы).
//!
//! Запуск рядом с локальным uwuwu-server (executor.url = http://127.0.0.1:8099).

use axum::{
    extract::Path,
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
struct MockSession {
    id: String,
    agent: String,
    title: String,
    updated_ms: u64,
    outcome: String,
    tokens_in: u64,
    tokens_out: u64,
    /// «работа» ещё идёт (agent-режим).
    busy: bool,
    dead: bool,
}

#[derive(Default)]
struct Store {
    sessions: Mutex<HashMap<String, MockSession>>,
    /// Сценарные вердикты ревью (MOCK_VERDICTS), расходуются по порядку.
    verdicts: Mutex<Vec<String>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn cfg(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Стадия по спавн-промпту (маркеры из spawn_prompt юзкейса) →
/// канон-переход, который делает «агент» этой стадии.
/// Возвращает (ключ стадии, статус-цель, вердикт-строка).
fn stage_action_of_prompt(prompt: &str) -> Option<(&'static str, &'static str, &'static str)> {
    // (стадия, статус-цель, вердикт-строка для коммента)
    Some(if prompt.contains("стадия утряски") {
        ("design", "Ревью плана", "план собран, на ревью (мок)")
    } else if prompt.contains("ревью плана") {
        ("plan", "In Progress", "план approve (мок)")
    } else if prompt.contains("реализация по утверждённому плану") {
        ("dev", "Ревью результата", "реализация завершена (мок)")
    } else if prompt.contains("ревью результата") {
        ("result", "Тестирование", "ревью approve (мок)")
    } else if prompt.contains("тестирование") {
        ("test", "Review", "тест pass (мок)")
    } else if prompt.contains("Триаж черновика") {
        ("triage", "Todo", "триаж: валидна (мок)")
    } else {
        // wakeup/крон/дример/прочее: без перехода статуса
        ("other", "", "сводка (мок)")
    })
}

/// Сценарный вердикт ревью-стадии из MOCK_VERDICTS (очередь
/// «stage:verdict» через «;», напр. «plan:request_changes;result:approve»).
/// Расходуется по порядку; пустая очередь/нет записи — дефолт (approve).
/// request_changes: plan → Design (возврат на утряску), result →
/// In Progress (на доработку) — кейс ре-энтри (инцидент #1046).
fn scripted_verdict(verdicts: &Mutex<Vec<String>>, stage: &str) -> Option<String> {
    let mut q = verdicts.lock().expect("verdicts");
    let prefix = format!("{stage}:");
    let pos = q.iter().position(|v| v.starts_with(&prefix))?;
    Some(q.remove(pos))
}

/// Действие «агента» при wakeup по ТЕКУЩЕМУ СТАТУСУ строки (стадию из
/// промпта не извлечь — wakeup-промпт общий). Дев дорабатывает и ведёт
/// на ревью, ревью-стадии подтверждают, тест проходит.
fn action_for_status(status: &str) -> (&'static str, &'static str) {
    match status.trim() {
        "Design" => ("Ревью плана", "план доработан (мок)"),
        "In Progress" => ("Ревью результата", "доработка завершена (мок)"),
        "Ревью результата" => ("Тестирование", "ревью approve (мок)"),
        "Тестирование" => ("Review", "тест pass (мок)"),
        "Ревью плана" => ("In Progress", "план approve (мок)"),
        _ => ("", "сводка (мок)"),
    }
}

/// Номер задачи из title сессии («#42 …»).
fn task_number_of(title: &str) -> Option<u64> {
    title
        .strip_prefix('#')?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// MCP-вызов uwuwu-server (tools/call). Ошибки — в лог, не роняют мок.
async fn mcp_call(base: &str, token: &str, tool: &str, args: Value) -> String {
    let client = reqwest::Client::new();
    let mut req = client
        .post(format!("{base}/mcp"))
        .timeout(Duration::from_secs(45))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        }));
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            // JSON-RPC ошибки живут в теле при HTTP 200 — логируем результат
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            let is_err = body["result"]["isError"].as_bool().unwrap_or(false);
            let text = body["result"]["content"][0]["text"].as_str().unwrap_or("");
            if is_err {
                tracing::warn!(event = "mock.mcp_rejected", tool, %status, reply = %text,
                    "uwuwu отклонил вызов");
            } else {
                tracing::info!(event = "mock.mcp_call", tool, %status, reply = %text,
                    "→ uwuwu");
            }
            text.to_string()
        }
        Err(e) => {
            tracing::warn!(event = "mock.mcp_failed", tool, error = %e, "uwuwu недоступен");
            String::new()
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mode = cfg("MOCK_MODE", "plain");
    let step_ms: u64 = cfg("MOCK_STEP_MS", "1500").parse().unwrap_or(1500);
    let mcp_url = cfg("MOCK_MCP_URL", "http://127.0.0.1:8090");
    let mcp_token = cfg("MOCK_MCP_TOKEN", "");
    let port: u16 = cfg("MOCK_PORT", "8099").parse().unwrap_or(8099);

    let store = Arc::new(Store {
        verdicts: Mutex::new(
            cfg("MOCK_VERDICTS", "")
                .split(';')
                .filter(|v| v.contains(':'))
                .map(str::to_string)
                .collect(),
        ),
        ..Store::default()
    });
    let verdicts_left = store.verdicts.lock().expect("verdicts").len();
    tracing::info!(event = "mock.started", %mode, port, mcp_url = %mcp_url,
        verdicts_left, "mock-opencode up");
    let counter = Arc::new(AtomicU64::new(1));

    let app = Router::new()
        .route(
            "/api/session",
            post({
                let store = store.clone();
                let counter = counter.clone();
                let mode = mode.clone();
                move |headers: HeaderMap, Json(body): Json<Value>| {
                    let store = store.clone();
                    let counter = counter.clone();
                    let mode = mode.clone();
                    async move { create_session(store, counter, mode, headers, body).await }
                }
            }),
        )
        .route(
            "/api/session",
            get({
                let store = store.clone();
                move || list_sessions(store)
            }),
        )
        .route(
            "/api/session/{id}",
            get({
                let store = store.clone();
                move |Path(id): Path<String>| get_session(store, id)
            }),
        )
        .route(
            "/api/session/{id}",
            delete({
                let store = store.clone();
                move |Path(id): Path<String>| delete_session(store, id)
            }),
        )
        .route(
            "/api/session/{id}/prompt",
            post({
                let store = store.clone();
                let mode = mode.clone();
                let step = step_ms;
                let mcp_url = mcp_url.clone();
                let mcp_token = mcp_token.clone();
                move |Path(id): Path<String>, Json(body): Json<Value>| {
                    let store = store.clone();
                    let mode = mode.clone();
                    let mcp_url = mcp_url.clone();
                    let mcp_token = mcp_token.clone();
                    async move { prompt(store, mode, step, mcp_url, mcp_token, id, body).await }
                }
            }),
        )
        .route(
            "/api/session/{id}/command",
            post(|Path(id): Path<String>| async move {
                // bootstrap-команды детерминированы: принимаем, не эмулируя shell
                tracing::info!(event = "mock.command", id = %id, "POST /command");
                (StatusCode::OK, Json(json!({})))
            }),
        )
        .route(
            "/api/session/{id}/message",
            get({
                let store = store.clone();
                move |Path(id): Path<String>| message_tail(store, id)
            }),
        )
        .route(
            "/api/experimental/session/{id}/export",
            get({
                let store = store.clone();
                move |Path(id): Path<String>| export(store, id)
            }),
        );

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind mock port");
    axum::serve(listener, app).await.expect("mock serve");
}

type Resp = (StatusCode, Json<Value>);

async fn create_session(
    store: Arc<Store>,
    counter: Arc<AtomicU64>,
    mode: String,
    _headers: HeaderMap,
    body: Value,
) -> Resp {
    let n = counter.fetch_add(1, Ordering::SeqCst);
    let id = format!("ses_mock{n:04}");
    let session = MockSession {
        id: id.clone(),
        agent: body["agent"].as_str().unwrap_or_default().to_string(),
        title: body["title"].as_str().unwrap_or_default().to_string(),
        updated_ms: now_ms(),
        outcome: String::new(),
        tokens_in: 0,
        tokens_out: 0,
        busy: false,
        dead: mode == "dead",
    };
    tracing::info!(event = "mock.session_created", id = %id,
        agent = %session.agent, title = %session.title, "POST /session");
    store
        .sessions
        .lock()
        .expect("store")
        .insert(id.clone(), session);
    (StatusCode::OK, Json(json!({ "data": { "id": id } })))
}

async fn list_sessions(store: Arc<Store>) -> Resp {
    let sessions = store.sessions.lock().expect("store");
    let list: Vec<Value> = sessions.values().map(session_json).collect();
    (StatusCode::OK, Json(Value::Array(list)))
}

fn session_json(s: &MockSession) -> Value {
    json!({
        "id": s.id,
        "agent": if s.agent.is_empty() { Value::Null } else { json!(s.agent) },
        "title": s.title,
        "time": { "created": s.updated_ms, "updated": s.updated_ms },
        "outcome": if s.outcome.is_empty() { Value::Null } else { json!(s.outcome) },
        "tokens": { "input": s.tokens_in, "output": s.tokens_out },
    })
}

async fn get_session(store: Arc<Store>, id: String) -> Resp {
    let sessions = store.sessions.lock().expect("store");
    match sessions.get(&id) {
        Some(s) if !s.dead => (StatusCode::OK, Json(session_json(s))),
        _ => (StatusCode::NOT_FOUND, Json(json!({ "status": "404" }))),
    }
}

async fn delete_session(store: Arc<Store>, id: String) -> Resp {
    let removed = store.sessions.lock().expect("store").remove(&id);
    tracing::info!(event = "mock.session_deleted", %id, "DELETE /session");
    if removed.is_some() {
        (StatusCode::OK, Json(json!({ "data": { "id": id } })))
    } else {
        (StatusCode::NOT_FOUND, Json(json!({ "status": "404" })))
    }
}

async fn prompt(
    store: Arc<Store>,
    mode: String,
    step_ms: u64,
    mcp_url: String,
    mcp_token: String,
    id: String,
    body: Value,
) -> Resp {
    let text = body["text"].as_str().unwrap_or_default().to_string();
    let Some(session) = store.sessions.lock().expect("store").get(&id).cloned() else {
        return (StatusCode::NOT_FOUND, Json(json!({ "status": "404" })));
    };
    tracing::info!(event = "mock.prompt", %id, len = text.len(), "POST /prompt");
    let mut sessions = store.sessions.lock().expect("store");
    if let Some(s) = sessions.get_mut(&id) {
        s.updated_ms = now_ms();
    }
    drop(sessions);

    match mode.as_str() {
        // шторм: мгновенное прерывание, ноль токенов (пустышка)
        "storm" => {
            let mut sessions = store.sessions.lock().expect("store");
            if let Some(s) = sessions.get_mut(&id) {
                s.outcome = "interrupted".into();
            }
            tracing::warn!(event = "mock.storm_interrupted", %id, "шторм-сценарий");
        }
        // агент: отработать по промпту через MCP uwuwu (один «прогон»)
        "agent" => {
            let free = {
                let mut sessions = store.sessions.lock().expect("store");
                sessions
                    .get_mut(&id)
                    .map(|s| {
                        if s.busy {
                            false
                        } else {
                            s.busy = true;
                            true
                        }
                    })
                    .unwrap_or(false)
            };
            if free {
                let store2 = store.clone();
                let mcp_url = mcp_url.clone();
                let mcp_token = mcp_token.clone();
                let prompt_text = text.clone();
                tokio::spawn(async move {
                    work_like_agent(store2, mcp_url, mcp_token, session, prompt_text, step_ms)
                        .await;
                });
            }
        }
        _ => {}
    }
    (StatusCode::OK, Json(json!({ "data": { "id": id } })))
}

/// «Работа агента»: sleep → стадия из промпта → коммент + канон-переход
/// через MCP uwuwu → succeeded.
async fn work_like_agent(
    store: Arc<Store>,
    mcp_url: String,
    mcp_token: String,
    session: MockSession,
    prompt_text: String,
    step_ms: u64,
) {
    tokio::time::sleep(Duration::from_millis(step_ms)).await;
    let (stage, mut status, mut verdict) =
        stage_action_of_prompt(&prompt_text).unwrap_or(("other", "", "сводка (мок)"));
    // wakeup: стадию из промпта не извлечь — действуем по текущему
    // статусу строки (дев дорабатывает, ревью подтверждают)
    if stage == "other" && prompt_text.starts_with("Wakeup") {
        if let Some(number) = task_number_of(&session.title) {
            let reply = mcp_call(
                &mcp_url,
                &mcp_token,
                "task_get",
                json!({ "number": number }),
            )
            .await;
            if let Some(line) = reply.lines().next() {
                if let Some(cur) = line.split("— ").nth(1) {
                    let (s, v) = action_for_status(cur);
                    status = s;
                    verdict = v;
                }
            }
        }
    }
    // сценарный вердикт ревью-стадии (MOCK_VERDICTS): request_changes
    // возвращает строку по канону (plan → Design, result → In Progress);
    // суффикс «+die» одновременно убивает сессии строки (кроме текущей) —
    // ре-энтри в форме инцидента #1046: dev-ячейка мертва → respawn
    if let Some(v) = scripted_verdict(&store.verdicts, stage) {
        let die = v.ends_with("+die");
        if v.trim_end_matches("+die").ends_with(":request_changes") {
            match stage {
                "plan" => {
                    status = "Design";
                    verdict = "план request_changes (мок) — возврат на утряску";
                }
                "result" => {
                    status = "In Progress";
                    verdict = "ревью request_changes (мок) — на доработку";
                }
                _ => {}
            }
            if die {
                let keep = session.id.clone();
                store
                    .sessions
                    .lock()
                    .expect("store")
                    .retain(|id, _| *id == keep);
                tracing::info!(
                    event = "mock.env_died",
                    "сценарий +die: сессии строки убиты (respawn-путь)"
                );
            }
        }
    }
    if let Some(number) = task_number_of(&session.title) {
        tracing::info!(event = "mock.agent_work", id = %session.id,
            agent = %session.agent, number, status = %status, "агент действует");
        mcp_call(
            &mcp_url,
            &mcp_token,
            "task_comment",
            json!({ "number": number, "text": format!("[мок] {verdict}") }),
        )
        .await;
        if !status.is_empty() {
            mcp_call(
                &mcp_url,
                &mcp_token,
                "task_edit",
                json!({ "number": number, "status": status }),
            )
            .await;
        }
    }
    let mut sessions = store.sessions.lock().expect("store");
    if let Some(s) = sessions.get_mut(&session.id) {
        s.busy = false;
        s.updated_ms = now_ms();
        s.outcome = "succeeded".into();
        s.tokens_in = 900;
        s.tokens_out = 100;
    }
    tracing::info!(event = "mock.agent_done", id = %session.id, "сессия завершена");
}

async fn message_tail(store: Arc<Store>, id: String) -> Resp {
    let sessions = store.sessions.lock().expect("store");
    match sessions.get(&id) {
        Some(s) => (
            StatusCode::OK,
            Json(json!([{ "time": { "updated": s.updated_ms }, "role": "assistant" }])),
        ),
        None => (StatusCode::NOT_FOUND, Json(json!({ "status": "404" }))),
    }
}

async fn export(store: Arc<Store>, id: String) -> Resp {
    let sessions = store.sessions.lock().expect("store");
    match sessions.get(&id) {
        Some(s) => (
            StatusCode::OK,
            Json(json!({ "data": format!(
                "# экспорт {}\n\nagent: {}\ntitle: {}\noutcome: {}\n",
                s.id, s.agent, s.title, s.outcome
            )})),
        ),
        None => (StatusCode::NOT_FOUND, Json(json!({ "status": "404" }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_number_from_title() {
        assert_eq!(task_number_of("#42 Ревью результата"), Some(42));
        assert_eq!(task_number_of("#7 Ответ на вопрос"), Some(7));
        assert_eq!(task_number_of("без номера"), None);
        assert_eq!(task_number_of("⏰ крон · штамп"), None);
    }

    #[test]
    fn stage_markers_map_to_canon_transitions() {
        // маркеры спавн-промптов → канон-переходы конвейера
        let cases = [
            ("стадия утряски: изучи постановку", "Ревью плана"),
            ("ревью плана: парафриз", "In Progress"),
            ("реализация по утверждённому плану", "Ревью результата"),
            ("ревью результата: код/CI", "Тестирование"),
            ("тестирование: качество фичи", "Review"),
            ("Триаж черновика #5", "Todo"),
        ];
        for (prompt, want) in cases {
            let (_, status, _) = stage_action_of_prompt(prompt).unwrap();
            assert_eq!(status, want, "промпт «{prompt}»");
        }
        // wakeup/крон/дример — без перехода статуса
        assert_eq!(
            stage_action_of_prompt("Wakeup: новый коммент").unwrap().1,
            ""
        );
        assert_eq!(stage_action_of_prompt("Крон-ран «X»").unwrap().1, "");
    }
}
