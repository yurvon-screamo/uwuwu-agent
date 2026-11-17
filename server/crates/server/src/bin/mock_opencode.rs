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
//!   gate   — гейт-заглушка #1160: исполнитель жив, но без-cookie гейт
//!            отдаёт 200 + text/html «Войти» на GET и 405 без тела на
//!            POST (живая проба Caddy @nologin) — окно заглушки: скан
//!            не «успешно пуст», а Err → слепой hold спавна
//!
//! Env: MOCK_MODE (деф. plain), MOCK_PORT (8099), MOCK_STEP_MS (1500),
//!      MOCK_MCP_URL (http://127.0.0.1:8090), MOCK_MCP_TOKEN («»),
//!      MOCK_VERDICTS («plan:request_changes;result:request_changes;...») —
//!      сценарные вердикты ревью-стадий по порядку (ре-энтри-циклы).
//!
//! Запуск рядом с локальным uwuwu-server (executor.url = http://127.0.0.1:8099).

use axum::{
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
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
    /// Директория сессии (location из create) — гард спавна (#1163).
    directory: String,
    updated_ms: u64,
    outcome: String,
    tokens_in: u64,
    tokens_out: u64,
    /// «работа» ещё идёт (agent-режим).
    busy: bool,
    /// time.busy/time.idle API (Started/idle): busy ⟺ busy_ms > idle_ms
    /// (нулевое значение = нет события) — семантика CUP-учёта (#1231).
    busy_ms: u64,
    idle_ms: u64,
    dead: bool,
    /// Реплай для message_tail: (created_ms, текст) — бот-контур (#1150)
    /// читает хвост как ответ сессии.
    reply: Option<(u64, String)>,
    /// Многосообщный ход (#1209): момент промпта с маркером «turn probe».
    /// Хвост строится фазами пробы: стрим → ловушка (шаг finish='tool-calls')
    /// → финал (finish='stop').
    turn_ms: Option<u64>,
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
    } else if prompt.contains("Аналитика волны") {
        ("analys", "Review Analys", "аналитика: проработано (мок)")
    } else if prompt.contains("Ревью аналитики") {
        ("analys-review", "Todo", "ревью аналитики: approve (мок)")
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

/// Номера строк волн аналитики из промпта (строки списка «- #N «…»»).
fn wave_numbers(prompt: &str) -> Vec<u64> {
    prompt
        .lines()
        .filter_map(|l| {
            let l = l.trim().strip_prefix("- #")?;
            l.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
        })
        .collect()
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
    // случайный префикс id: инстансы на стенде не должны делить id-пространство
    // (second source #1163 матчит сессии по id в union-скане — коллизия
    // детерминированных id двух моков ложноположительно «знает» чужую сессию)
    let id_salt = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();

    let app = Router::new()
        .route(
            "/api/session",
            post({
                let store = store.clone();
                let counter = counter.clone();
                let salt = id_salt;
                let mode = mode.clone();
                move |headers: HeaderMap, Json(body): Json<Value>| {
                    let store = store.clone();
                    let counter = counter.clone();
                    let salt = salt;
                    let mode = mode.clone();
                    async move {
                        if mode == "gate" {
                            return gate_post_reject();
                        }
                        create_session(store, counter, salt, mode, headers, body)
                            .await
                            .into_response()
                    }
                }
            }),
        )
        .route(
            "/api/session",
            get({
                let store = store.clone();
                let mode = mode.clone();
                move |q: Query<ListQuery>| {
                    let store = store.clone();
                    let mode = mode.clone();
                    async move {
                        if mode == "gate" {
                            return gate_stub();
                        }
                        list_sessions(store, q).await.into_response()
                    }
                }
            }),
        )
        .route(
            "/api/session/{id}",
            get({
                let store = store.clone();
                let mode = mode.clone();
                move |Path(id): Path<String>| {
                    let store = store.clone();
                    let mode = mode.clone();
                    async move {
                        if mode == "gate" {
                            return gate_stub();
                        }
                        get_session(store, id).await.into_response()
                    }
                }
            }),
        )
        .route(
            "/api/session/{id}",
            delete({
                let store = store.clone();
                move |Path(id): Path<String>| async move {
                    delete_session(store, id).await.into_response()
                }
            }),
        )
        .route(
            "/api/session/{id}/abort",
            post({
                let store = store.clone();
                move |Path(id): Path<String>| abort_session(store, id)
            }),
        )
        .route(
            "/api/session/{id}/interrupt",
            post({
                let store = store.clone();
                move |Path(id): Path<String>| abort_session(store, id)
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
                    async move {
                        if mode == "gate" {
                            return gate_post_reject();
                        }
                        prompt(store, mode, step, mcp_url, mcp_token, id, body)
                            .await
                            .into_response()
                    }
                }
            }),
        )
        .route(
            "/api/session/{id}/command",
            post({
                let mode = mode.clone();
                move |Path(id): Path<String>| {
                    let mode = mode.clone();
                    async move {
                        if mode == "gate" {
                            return gate_post_reject();
                        }
                        // bootstrap-команды детерминированы: принимаем, не эмулируя shell
                        tracing::info!(event = "mock.command", id = %id, "POST /command");
                        (StatusCode::OK, Json(json!({}))).into_response()
                    }
                }
            }),
        )
        .route(
            "/api/session/{id}/message",
            get({
                let store = store.clone();
                let mode = mode.clone();
                move |Path(id): Path<String>| {
                    let store = store.clone();
                    let mode = mode.clone();
                    async move {
                        if mode == "gate" {
                            return gate_stub();
                        }
                        message_tail(store, id).await.into_response()
                    }
                }
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

/// Страница «Войти» гейта — упрощённая копия прод-заглушки Caddy
/// @nologin (живая проба #1160: 200 + text/html на любой GET-путь).
const GATE_HTML: &str = "<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">\
<title>OpenCode — доступ</title></head><body><main><h1>OpenCode</h1>\
<p>Приватный доступ. Нажми, чтобы войти.</p><a href=\"/gate-mock\">Войти</a></main></body></html>";

/// gate-режим: GET-путь — 200 + text/html заглушка (клиент обязан дать
/// Err сниффом, не «успешно пусто»)
fn gate_stub() -> Response {
    Html(GATE_HTML).into_response()
}

/// gate-режим: POST-путь — живой гейт отдаёт 405 (allow: GET, HEAD)
/// без content-type и тела; клиент падает статусным чеком, не сниффом
fn gate_post_reject() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [("allow", "GET, HEAD")]).into_response()
}

async fn create_session(
    store: Arc<Store>,
    counter: Arc<AtomicU64>,
    salt: u32,
    mode: String,
    _headers: HeaderMap,
    body: Value,
) -> Resp {
    let n = counter.fetch_add(1, Ordering::SeqCst);
    let id = format!("ses_mock{n:04}{salt:08x}");
    let session = MockSession {
        id: id.clone(),
        agent: body["agent"].as_str().unwrap_or_default().to_string(),
        title: body["title"].as_str().unwrap_or_default().to_string(),
        directory: body["location"]["directory"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        updated_ms: now_ms(),
        outcome: String::new(),
        tokens_in: 0,
        tokens_out: 0,
        busy: false,
        busy_ms: 0,
        idle_ms: 0,
        dead: mode == "dead",
        reply: None,
        turn_ms: None,
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

#[derive(serde::Deserialize)]
struct ListQuery {
    limit: Option<usize>,
    cursor: Option<String>,
}

async fn list_sessions(store: Arc<Store>, q: Query<ListQuery>) -> Resp {
    let sessions = store.sessions.lock().expect("store");
    // мёртвые (удалённые) в списке не числятся — семантика реального
    // опенкода, на неё опирается busy-учёт CUP (#1231); list и GET
    // согласованы (контракт second source #1163)
    let mut list: Vec<Value> = sessions
        .values()
        .filter(|s| !s.dead)
        .map(session_json)
        .collect();
    list.sort_by(|a, b| {
        a["id"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["id"].as_str().unwrap_or_default())
    });
    let page = std::env::var("MOCK_PAGE_LIMIT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0);
    let Some(page_size) = page else {
        return (StatusCode::OK, Json(Value::Array(list)));
    };
    // пагинация newest-first не моделируем — стабильный порядок по id:
    // контракт для сервера тот же (страница + cursor.next), iteration
    // по курсору e2e-фазой 4 (MOCK_PAGE_LIMIT=1)
    let offset = q
        .cursor
        .as_deref()
        .and_then(|c| c.strip_prefix("p"))
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(0);
    let chunk: Vec<Value> = list
        .iter()
        .skip(offset)
        .take(q.limit.unwrap_or(page_size).min(page_size))
        .cloned()
        .collect();
    let next = if offset + chunk.len() < list.len() {
        format!("p{}", offset + chunk.len())
    } else {
        String::new()
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({ "data": chunk, "cursor": { "next": next } })),
    )
}

fn session_json(s: &MockSession) -> Value {
    json!({
        "id": s.id,
        "agent": if s.agent.is_empty() { Value::Null } else { json!(s.agent) },
        "title": s.title,
        "location": { "directory": s.directory },
        "time": {
            "created": s.updated_ms,
            "updated": s.updated_ms,
            "busy": if s.busy_ms == 0 { Value::Null } else { json!(s.busy_ms) },
            "idle": if s.idle_ms == 0 { Value::Null } else { json!(s.idle_ms) },
        },
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

/// Abort/interrupt (преемпшен #1259): мок принимает оба пути клиента.
/// Ход в моке не эмулируется долгим стримом для этого сценария — факт
/// аборта в логе; после #1231 (мок несёт time.busy/time.idle) сюда
/// ложится сброс busy.
async fn abort_session(store: Arc<Store>, id: String) -> Resp {
    tracing::info!(event = "mock.session_aborted", %id, "POST /session/{id}/abort");
    let sessions = store.sessions.lock().expect("store");
    if sessions.contains_key(&id) {
        (StatusCode::OK, Json(json!({ "interrupted": true })))
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
        s.busy_ms = now_ms(); // Started: ход в полёте (CUP-учёт, #1231)

        // Маркер turn probe (#1209): сессия отвечает многосообщным ходом —
        // мгновенный реплай не ставится, message_tail строит фазы пробы
        if text.contains("turn probe") {
            s.turn_ms = Some(now_ms());
            s.reply = None;
            return (StatusCode::OK, Json(json!({ "data": { "id": id } })));
        }
        // любой не-turn ход гасит залипший turn-режим: фазы пробы —
        // ровно один ход, иначе следующие сообщения сессии молчат
        // (вскрыто media-сценарием #1171)
        s.turn_ms = None;
        // реплай бота готов сразу (storm — исключение: прерывание без ответа)
        if mode != "storm" {
            let tail = text
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default();
            s.reply = Some((now_ms(), format!("мок-ответ на: {tail}")));
        }
    }
    drop(sessions);

    match mode.as_str() {
        // шторм: мгновенное прерывание, ноль токенов (пустышка)
        "storm" => {
            let mut sessions = store.sessions.lock().expect("store");
            if let Some(s) = sessions.get_mut(&id) {
                s.outcome = "interrupted".into();
                s.idle_ms = now_ms(); // прерванный ход завершён (CUP-учёт)
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
    // проект MCP-вызовов: изолированные стенды (#1135 fullwave) гонят конфиг
    // без uwuwu — без project сервер дефолтит uwuwu и отклоняет вызов.
    // Дефолт пустой (прод-сценарии с uwuwu в конфиге работают как раньше).
    let mcp_project = cfg("MOCK_MCP_PROJECT", "");
    let project_of = |mut args: Value| -> Value {
        if !mcp_project.is_empty() {
            args["project"] = Value::String(mcp_project.clone());
        }
        args
    };
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
                project_of(json!({ "number": number })),
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
    // возвращает строку по канону (plan → Design, result → In Progress,
    // analys-review → Analys); суффикс «+die» одновременно убивает сессии
    // строки (кроме текущей) — ре-энтри в форме инцидента #1046
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
                "analys-review" => {
                    status = "Analys";
                    verdict = "ревью аналитики: request_changes (мок) — возврат в Analys";
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
    if wave_numbers(&prompt_text).is_empty() {
        if let Some(number) = task_number_of(&session.title) {
            tracing::info!(event = "mock.agent_work", id = %session.id,
                agent = %session.agent, number, status = %status, "агент действует");
            mcp_call(
                &mcp_url,
                &mcp_token,
                "task_comment",
                project_of(json!({ "number": number, "text": format!("[мок] {verdict}") })),
            )
            .await;
            if !status.is_empty() {
                mcp_call(
                    &mcp_url,
                    &mcp_token,
                    "task_edit",
                    project_of(json!({ "number": number, "status": status })),
                )
                .await;
            }
        }
    } else {
        // волны аналитики (#1135): номера строк из промпта. Аналитик:
        // первая строка — проработка (→ Review Analys), остальные —
        // дубли (close not_planned). Ревьювер: все строки — вердикт.
        let nums = wave_numbers(&prompt_text);
        tracing::info!(event = "mock.agent_work", id = %session.id,
            agent = %session.agent, rows = nums.len(), status = %status, "волновая работа");
        let (first, rest) = nums.split_first().expect("непустая волна");
        mcp_call(
            &mcp_url,
            &mcp_token,
            "task_comment",
            project_of(json!({ "number": first, "text": format!("[мок] {verdict}") })),
        )
        .await;
        if !status.is_empty() {
            mcp_call(
                &mcp_url,
                &mcp_token,
                "task_edit",
                project_of(json!({ "number": first, "status": status })),
            )
            .await;
        }
        for n in rest {
            if stage == "analys" {
                mcp_call(
                    &mcp_url,
                    &mcp_token,
                    "task_close",
                    project_of(json!({ "number": n, "not_planned": true })),
                )
                .await;
            } else {
                mcp_call(
                    &mcp_url,
                    &mcp_token,
                    "task_comment",
                    project_of(json!({ "number": n, "text": format!("[мок] {verdict}") })),
                )
                .await;
                if !status.is_empty() {
                    mcp_call(
                        &mcp_url,
                        &mcp_token,
                        "task_edit",
                        project_of(json!({ "number": n, "status": status })),
                    )
                    .await;
                }
            }
        }
    }
    let mut sessions = store.sessions.lock().expect("store");
    if let Some(s) = sessions.get_mut(&session.id) {
        s.busy = false;
        s.updated_ms = now_ms();
        s.idle_ms = now_ms(); // idle: ход завершён — busy снимается (CUP-учёт, #1231)
        s.outcome = "succeeded".into();
        s.tokens_in = 900;
        s.tokens_out = 100;
    }
    tracing::info!(event = "mock.agent_done", id = %session.id, "сессия завершена");
}

/// Фазы многосообщного хода (#1209, наблюдённая модель пробы): стрим
/// (finish=None) → ловушка для старого условия (шаг finish='tool-calls',
/// is_latest=true — зазор после тула) → финал finish='stop'. Окна —
/// константы под поллинг раннера 2с (reply_poll_ms).
fn turn_tail(turn_ms: u64) -> Value {
    const STREAM_MS: u64 = 1_200;
    const TRAP_MS: u64 = 3_500;
    const STEP_TEXT: &str = "промежуточный шаг хода";
    const FINAL_TEXT: &str = "turn final ok";
    let now = now_ms();
    let step1 = json!({
        "type": "assistant",
        "role": "assistant",
        "time": { "created": turn_ms, "updated": now },
        "content": [
            { "type": "text", "text": STEP_TEXT },
            { "type": "tool", "state": { "status": "completed" } },
        ],
    });
    let user = json!({
        "type": "user",
        "time": { "created": turn_ms - 100, "updated": turn_ms - 100 },
    });
    if now < turn_ms + STREAM_MS {
        // стримится: без finish — не ответ
        let mut msg = step1.clone();
        msg["finish"] = Value::Null;
        return json!([msg, user]);
    }
    if now < turn_ms + TRAP_MS {
        // шаг завершён, ход продолжается — ловушка finished+is_latest
        let mut msg = step1.clone();
        msg["finish"] = json!("tool-calls");
        return json!([msg, user]);
    }
    let final_msg = json!({
        "type": "assistant",
        "role": "assistant",
        "time": { "created": turn_ms + TRAP_MS, "updated": now },
        "finish": "stop",
        "content": [{ "type": "text", "text": FINAL_TEXT }],
    });
    json!([final_msg, step1, user])
}

async fn message_tail(store: Arc<Store>, id: String) -> Resp {
    let sessions = store.sessions.lock().expect("store");
    match sessions.get(&id) {
        Some(s) if s.turn_ms.is_some() => {
            let tail = turn_tail(s.turn_ms.expect("turn_ms checked"));
            (StatusCode::OK, Json(tail))
        }
        Some(s) => {
            // Форма — живой опенкод 2.3.0 (проба #1150): тип в поле `type`,
            // у ассистента finish/content[].text/time.created; time.updated
            // держим для last_activity_ms (wakeup-контур)
            let mut msg = json!({
                "type": "assistant",
                "role": "assistant",
                "time": { "updated": s.updated_ms },
            });
            if let Some((created_ms, text)) = &s.reply {
                msg["finish"] = json!("stop");
                msg["time"]["created"] = json!(created_ms);
                msg["time"]["completed"] = json!(created_ms);
                msg["content"] = json!([{ "type": "text", "text": text }]);
            }
            (StatusCode::OK, Json(json!([msg])))
        }
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
    fn wave_numbers_from_prompt() {
        let prompt = "Аналитика волны (проект p) — строки в Analys, пачка целиком:\n\n\
                      - #12 «фикс»\n- #15 «идея»\n\nДействуй по методичке.\n\
                      остальной текст с числами 99 не считается";
        assert_eq!(wave_numbers(prompt), vec![12, 15]);
        assert!(wave_numbers("нет списка").is_empty());
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
            (
                "Аналитика волны (проект p) — строки в Analys",
                "Review Analys",
            ),
            (
                "Ревью аналитики (проект p) — строки в Review Analys",
                "Todo",
            ),
        ];
        for (prompt, want) in cases {
            let (_, status, _) = stage_action_of_prompt(prompt).unwrap();
            assert_eq!(status, want, "промпт «{prompt}»");
        }
        // триаж выведен (#1135): старого маркера нет
        assert_ne!(
            stage_action_of_prompt("Триаж черновика #5").unwrap().0,
            "triage"
        );
        // wakeup/крон/дример — без перехода статуса
        assert_eq!(
            stage_action_of_prompt("Wakeup: новый коммент").unwrap().1,
            ""
        );
        assert_eq!(stage_action_of_prompt("Крон-ран «X»").unwrap().1, "");
    }
}
