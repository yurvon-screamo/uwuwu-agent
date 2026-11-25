//! mock-opencode: эмулятор исполнителя для безопасного дебага конвейера
//! uwuwu-server (без LLM, без Railway, на лаптопе). Контракты — 1:1 с
//! OpencodeClient (crates/server/src/opencode.rs, openapi 2.3.0 + пин
//! агента слота из 2.3.2, #1300).
//!
//! Пин дрейфа (#1329): маршруты пинятся tests/mock_route_coverage.rs
//! (манифест src/mock_routes.rs + проб реального бинаря) — новый вызов
//! клиента без мок-роута красный на self-check, стенд не доходит.
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
//!   busy   — сессии не отвечают: ход «висит» (message_tail без finish →
//!            busy по контракту сервера, #1231) — носитель busy-жертв
//!            преемпшена CUP (#1259 e2e); abort снимает ход
//!
//! lost-ручки стенда (#1288, dev-ручки тестировщика):
//!   MOCK_PROMPT_404_AFTER=N — после N успешных доставок все следующие
//!     prompt → 404 при живом list (serve-рассинхрон инцидента #16:
//!     «проба Alive, доставка отказывает») — десинк-ветка lost-контура;
//!   POST /api/lost {"directory": dir} — сессии с этой directory в
//!     классе lost: GET 200 (probe Alive), list скрывает, prompt/
//!     switch_agent → 404 — lost-resurrection-ветка (юнит-класс T2
//!     живьём); включается ПОСЛЕ успешного спавна.
//!
//! Env: MOCK_MODE (деф. plain), MOCK_PORT (8099 — дефолт standalone-ручного
//!      запуска; скрипты стенда порт подбирают/передают явно, #1304),
//!      MOCK_STEP_MS (1500), MOCK_MCP_URL (http://127.0.0.1:8090),
//!      MOCK_MCP_TOKEN («»),
//!      MOCK_VERDICTS («plan:request_changes;result:request_changes;...») —
//!      сценарные вердикты ревью-стадий по порядку (ре-энтри-циклы);
//!      ключ «test-review:request_changes» — находки тест-ревьювера
//!      (#1413), ключ «test:review» — pass теста без тест-коммитов.
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
    /// Модель сессии `provider/id` из create-тела — каталог /model (#1221).
    model: String,
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
    /// User-сообщения команд (created_ms, текст): канал вердиктов env-gc
    /// (#1431) — POST /command с name task-env-gc кладёт вердикт-строку,
    /// wait_user_message конвейера находит её в хвосте.
    user_messages: Vec<(u64, String)>,
}

#[derive(Default)]
struct Store {
    sessions: Mutex<HashMap<String, MockSession>>,
    /// Сценарные вердикты ревью (MOCK_VERDICTS), расходуются по порядку.
    verdicts: Mutex<Vec<String>>,
    /// lost-ручка стенда #1288 (динамическая, POST /api/lost): сессии с
    /// этой directory в классе lost — GET 200 (probe Alive), list
    /// скрывает, prompt/switch_agent → 404. Живой T2-стенд
    /// lost-маршрутизации («сессии нет в списке, доставка отказывает»);
    /// включается ПОСЛЕ успешного спавна — начальный промпт не задет.
    lost_directory: Mutex<String>,
    /// MOCK_PROMPT_404_AFTER: после N успешных доставок все следующие
    /// prompt → 404 при живом list — serve-рассинхрон инцидента #16
    /// (десинк-стенд lost-контура).
    prompt_404_after: u64,
    prompt_ok: AtomicU64,
    /// Адмиссии нативного компакта (#1221): id сессий по порядку —
    /// наблюдаемость e2e-фазы компакта.
    compacts: Mutex<Vec<String>>,
    /// Окна фаз turn-хвоста (мс): MOCK_TURN_STREAM_MS / MOCK_TURN_TRAP_MS.
    /// Дефолты — под поллинг раннера 2с (reply_poll_ms); контракт-тест
    /// формы (#1254) растягивает окно стрима — проба детерминированно
    /// попадает в фазу. Форма фаз от окон не зависит (пин #1254).
    turn_stream_ms: u64,
    turn_trap_ms: u64,
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

/// Стадия по спавн-промпту → канон-переход, который делает «агент» этой
/// стадии. Маркеры — импорт из источника (STAGE_PROMPTS spawn_prompt,
/// ANALYST*_WAVE_MARKER волн): правка промпта константой доезжает сюда
/// автоматически; волны аналитика классифицируются своими маркерами.
/// Возвращает (ключ стадии, статус-цель, вердикт-строка).
fn stage_action_of_prompt(prompt: &str) -> Option<(&'static str, &'static str, &'static str)> {
    use uwuwu_usecase::{ANALYST_REVIEW_WAVE_MARKER, ANALYST_WAVE_MARKER, STAGE_PROMPTS};
    let slot_stage = STAGE_PROMPTS
        .iter()
        .find(|(_, m)| prompt.contains(m))
        .map(|(slot, _)| *slot);
    // (стадия, статус-цель, вердикт-строка для коммента)
    Some(match slot_stage {
        Some(uwuwu_domain::Slot::Design) => {
            ("design", "Plan review", "план собран, на ревью (мок)")
        }
        Some(uwuwu_domain::Slot::PlanReview) => ("plan", "In Progress", "план approve (мок)"),
        Some(uwuwu_domain::Slot::Dev) => ("dev", "Result review", "реализация завершена (мок)"),
        Some(uwuwu_domain::Slot::ResultReview) => ("result", "Testing", "ревью approve (мок)"),
        // дефолт стенда: pass с тест-коммитами → ревью тестирования (#1413)
        Some(uwuwu_domain::Slot::Test) => (
            "test",
            "Testing review",
            "тест pass, есть тест-коммиты (мок)",
        ),
        Some(uwuwu_domain::Slot::TestReview) => {
            ("test-review", "Review", "тест-ревью: чисто (мок)")
        }
        _ if prompt.contains(
            ANALYST_WAVE_MARKER
                .split("{project}")
                .next()
                .unwrap_or_default(),
        ) =>
        {
            ("analys", "Review Analys", "аналитика: проработано (мок)")
        }
        _ if prompt.contains(
            ANALYST_REVIEW_WAVE_MARKER
                .split("{project}")
                .next()
                .unwrap_or_default(),
        ) =>
        {
            ("analys-review", "Todo", "ревью аналитики: approve (мок)")
        }
        // wakeup/крон/дример/прочее: без перехода статуса
        _ => ("other", "", "сводка (мок)"),
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
/// на ревью, ревью-стадии подтверждают, тест проходит. Статусы с доски
/// резолвятся доменно (parse_status — канон и легаси до rename #1383).
fn action_for_status(status: &str) -> (&'static str, &'static str) {
    use uwuwu_usecase::parse_status;
    match parse_status(status) {
        uwuwu_domain::Status::Design => ("Plan review", "план доработан (мок)"),
        uwuwu_domain::Status::InProgress => ("Result review", "доработка завершена (мок)"),
        uwuwu_domain::Status::ReviewResult => ("Testing", "ревью approve (мок)"),
        // дефолт стенда: pass с тест-коммитами → ревью тестирования (#1413)
        uwuwu_domain::Status::Testing => ("Testing review", "тест pass, есть тест-коммиты (мок)"),
        uwuwu_domain::Status::ReviewTesting => ("Review", "тест-ревью approve (мок)"),
        uwuwu_domain::Status::ReviewPlan => ("In Progress", "план approve (мок)"),
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
    // json-формат: единый парсинг логов мока и сервера в e2e-инвариантах
    // (lifecycle-пин «create + начальный prompt — один исполнитель», #1248)
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mode = cfg("MOCK_MODE", "plain");
    let step_ms: u64 = cfg("MOCK_STEP_MS", "1500").parse().unwrap_or(1500);
    let mcp_url = cfg("MOCK_MCP_URL", "http://127.0.0.1:8090");
    let mcp_token = cfg("MOCK_MCP_TOKEN", "");
    let port: u16 = cfg("MOCK_PORT", "8099").parse().unwrap_or(8099);
    let prompt_404_after: u64 = cfg("MOCK_PROMPT_404_AFTER", "0").parse().unwrap_or(0);
    let turn_stream_ms: u64 = cfg("MOCK_TURN_STREAM_MS", "1200").parse().unwrap_or(1200);
    let turn_trap_ms: u64 = cfg("MOCK_TURN_TRAP_MS", "3500").parse().unwrap_or(3500);

    let store = Arc::new(Store {
        verdicts: Mutex::new(
            cfg("MOCK_VERDICTS", "")
                .split(';')
                .filter(|v| v.contains(':'))
                .map(str::to_string)
                .collect(),
        ),
        lost_directory: Mutex::new(String::new()),
        prompt_404_after,
        prompt_ok: AtomicU64::new(0),
        turn_stream_ms,
        turn_trap_ms,
        ..Store::default()
    });
    let verdicts_left = store.verdicts.lock().expect("verdicts").len();
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
            "/api/session/{id}/compact",
            post({
                let store = store.clone();
                move |Path(id): Path<String>| compact_session(store, id)
            }),
        )
        .route(
            "/api/model",
            get({
                let store = store.clone();
                move || model_catalog(store.clone())
            }),
        )
        .route(
            // Стендовая ручка времени (аналог MOCK_VERDICTS/MOCK_PAGE_LIMIT):
            // сдвигает updated_ms сессии назад — idle-контуры (дример #1306,
            // IDLE_MS 30 мин) проверяются стендом без реальных минут ожидания.
            // Контракта реального опенкода нет — только e2e-фазы.
            "/api/session/{id}/age",
            post({
                let store = store.clone();
                move |Path(id): Path<String>, Json(body): Json<Value>| age_session(store, id, body)
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
            // lost-ручка стенда #1288 (динамическая): {"directory": "<dir>"}
            // переводит сессии с этой directory в класс lost (prompt/agent
            // → 404, list скрывает, GET 200); пустая строка — сброс.
            // Включается ПОСЛЕ успешного спавна — начальный промпт не задет.
            "/api/lost",
            post({
                let store = store.clone();
                move |Json(body): Json<Value>| async move {
                    let dir = body["directory"].as_str().unwrap_or_default().to_string();
                    tracing::warn!(event = "mock.lost_set", directory = %dir, "POST /lost");
                    *store.lost_directory.lock().expect("lost") = dir.clone();
                    (StatusCode::OK, Json(json!({ "data": { "lost": dir } }))).into_response()
                }
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
                let store = store.clone();
                let mode = mode.clone();
                move |Path(id): Path<String>, Json(body): Json<Value>| {
                    let store = store.clone();
                    let mode = mode.clone();
                    async move {
                        if mode == "gate" {
                            return gate_post_reject();
                        }
                        // bootstrap-команды детерминированы: task-env-gc —
                        // канал вердиктов env-gc (#1431), мок эмулирует
                        // shell-исход user-сообщением «ENV-GC <row> SWEPT»;
                        // прочие принимаются без эмуляции shell. row — из
                        // $1 args (первый токен): резолв шаблона opencode
                        // подставляет в $1 только args[0] (bug #1506 —
                        // бывший next_back маскировал чужой args-контракт)
                        let name = body["name"].as_str().unwrap_or_default();
                        tracing::info!(event = "mock.command", id = %id, name, "POST /command");
                        if name == "task-env-gc" {
                            let row = body["text"]
                                .as_str()
                                .unwrap_or_default()
                                .split_whitespace()
                                .next()
                                .unwrap_or_default()
                                .to_string();
                            let mut sessions = store.sessions.lock().expect("store");
                            if let Some(s) = sessions.get_mut(&id) {
                                s.user_messages
                                    .push((now_ms(), format!("ENV-GC {row} SWEPT")));
                            }
                        }
                        (StatusCode::OK, Json(json!({}))).into_response()
                    }
                }
            }),
        )
        .route(
            "/api/session/{id}/agent",
            post({
                let store = store.clone();
                let mode = mode.clone();
                move |Path(id): Path<String>, Json(body): Json<Value>| {
                    let store = store.clone();
                    async move {
                        if mode == "gate" {
                            return gate_post_reject();
                        }
                        switch_agent(store, id, body).await.into_response()
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
    // up-лог строго после успешного бинда, с фактическим адресом: занятый
    // порт больше не маскируется «up + паника» (#1304); requested_port —
    // что просили (оверрайд/дефолт), port — что реально слушаем
    let bound = listener.local_addr().expect("mock local addr");
    tracing::info!(event = "mock.started", %mode, requested_port = port,
        port = bound.port(), mcp_url = %mcp_url,
        verdicts_left,
        prompt_404_after = store.prompt_404_after,
        "mock-opencode up");
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

/// Пин агента слота (#1300, контракт opencode 2.3.2): 204, agent
/// персистится в сессию — сталл-пинги сервера проходят на стенде как в
/// живом opencode (404 = сессия умерла, честный Err на клиенте).
/// lost-ручка #1288: сессия в классе lost по directory (GET 200, list
/// скрывает, prompt/switch_agent → 404)
fn session_is_lost(store: &Store, directory: &str) -> bool {
    let lost = store.lost_directory.lock().expect("lost").clone();
    !lost.is_empty() && directory == lost
}

async fn switch_agent(store: Arc<Store>, id: String, body: Value) -> StatusCode {
    let agent = body["agent"].as_str().unwrap_or_default().to_string();
    let mut sessions = store.sessions.lock().expect("store");
    match sessions.get_mut(&id) {
        Some(s) => {
            // lost-ручка #1288: пин агента слота — классифицированный путь
            // доставки, lost-сессия отвечает 404 как prompt
            if session_is_lost(&store, &s.directory) {
                tracing::warn!(event = "mock.agent_lost", %id, "lost-directory ручка: switch_agent → 404");
                return StatusCode::NOT_FOUND;
            }
            // AFTER-ручка (serve-рассинхрон #16): запись в сессию отклоняется
            // ДО обновления активности — иначе успешный пин «оживляет» сессию
            // и сталл-пинг замолкает (инцидент: доставка падала целиком,
            // активность не двигалась, ретрай на каждом тике)
            if store.prompt_404_after > 0 {
                let n = store.prompt_ok.fetch_add(1, Ordering::SeqCst) + 1;
                if n > store.prompt_404_after {
                    tracing::warn!(event = "mock.agent_lost", %id, attempt = n,
                        after = store.prompt_404_after, "AFTER-ручка: switch_agent → 404");
                    return StatusCode::NOT_FOUND;
                }
            }
            tracing::info!(event = "mock.agent_switched", %id, %agent, "POST /session/:id/agent");
            s.agent = agent;
            s.updated_ms = now_ms();
            StatusCode::NO_CONTENT
        }
        None => StatusCode::NOT_FOUND,
    }
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
    let model = format!(
        "{}/{}",
        body["model"]["providerID"].as_str().unwrap_or_default(),
        body["model"]["id"].as_str().unwrap_or_default()
    );
    let session = MockSession {
        id: id.clone(),
        agent: body["agent"].as_str().unwrap_or_default().to_string(),
        title: body["title"].as_str().unwrap_or_default().to_string(),
        directory: body["location"]["directory"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        model,
        updated_ms: now_ms(),
        outcome: String::new(),
        tokens_in: 0,
        tokens_out: 0,
        busy: false,
        // дев-ручка стенда (#1313 V4): busy_ms в прошлом — зомби-флаг
        // (busy > idle старше 30 мин не держит CUP-кап)
        busy_ms: body["busy_ms_ago"]
            .as_u64()
            .map(|ago| now_ms().saturating_sub(ago))
            .unwrap_or(0),
        idle_ms: 0,
        dead: mode == "dead",
        reply: None,
        turn_ms: None,
        user_messages: Vec::new(),
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
    // согласованы (контракт second source #1163). lost-ручка #1288 —
    // осознанное расхождение: сессия скрыта из list (union-скан не
    // «знает» её), GET при этом отвечает 200 (probe Alive) — стенд
    // lost-resurrection
    let lost_dir = store.lost_directory.lock().expect("lost").clone();
    let mut list: Vec<Value> = sessions
        .values()
        .filter(|s| !s.dead && (lost_dir.is_empty() || s.directory != lost_dir))
        .map(session_json)
        .collect();
    drop(lost_dir);
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
        // живая форма 2.3 (пин #1254): список всегда обёрнут {data, cursor}
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "data": list, "cursor": { "next": "" } })),
        );
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
    // живая форма 2.3 (пин #1254): agent-ключ только у role-сессий,
    // у default отсутствует (не null); outcome null у живых
    let mut v = json!({
        "id": s.id,
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
    });
    if !s.agent.is_empty() {
        v["agent"] = json!(s.agent);
    }
    v
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
/// busy-режим: abort снимает ход — сессия получает финальный ответ
/// («работа остановлена») и перестаёт держать busy-учёт сервера
/// (message_tail с finish=stop → idle); после #1231 (мок несёт
/// time.busy/time.idle) сюда ложится сброс busy.
async fn abort_session(store: Arc<Store>, id: String) -> Resp {
    tracing::info!(event = "mock.session_aborted", %id, "POST /session/{id}/abort");
    let mut sessions = store.sessions.lock().expect("store");
    match sessions.get_mut(&id) {
        Some(s) => {
            s.busy = false;
            s.reply = Some((now_ms(), "ход остановлен (abort)".into()));
            (StatusCode::OK, Json(json!({ "interrupted": true })))
        }
        None => (StatusCode::NOT_FOUND, Json(json!({ "status": "404" }))),
    }
}

/// POST /session/{id}/compact (#1221): нативный durable admission — мок
/// принимает заявку (compaction-сообщение обновляет активность сессии)
/// и записывает факт для e2e-наблюдаемости.
async fn compact_session(store: Arc<Store>, id: String) -> Resp {
    let mut sessions = store.sessions.lock().expect("store");
    match sessions.get_mut(&id) {
        Some(s) => {
            s.updated_ms = now_ms();
            store.compacts.lock().expect("compacts").push(id.clone());
            tracing::info!(event = "mock.session_compacted", %id, "POST /session/{id}/compact");
            (StatusCode::OK, Json(json!({})))
        }
        None => (StatusCode::NOT_FOUND, Json(json!({ "status": "404" }))),
    }
}

/// GET /model (#1221): каталог моделей из живых сессий мока с общим
/// лимитом окна MOCK_CONTEXT_LIMIT (деф. 100000) — компакт-проба
/// матчит providerID+modelID против лимита.
async fn model_catalog(store: Arc<Store>) -> Resp {
    let limit: u64 = cfg("MOCK_CONTEXT_LIMIT", "100000")
        .parse()
        .unwrap_or(100_000);
    let sessions = store.sessions.lock().expect("store");
    let mut models: Vec<String> = sessions
        .values()
        .map(|s| s.model.clone())
        .filter(|m| !m.is_empty())
        .collect();
    drop(sessions);
    models.sort();
    models.dedup();
    let items: Vec<Value> = models
        .iter()
        .filter_map(|m| m.split_once('/'))
        .map(|(p, i)| json!({ "providerID": p, "modelID": i, "limit": { "context": limit } }))
        .collect();
    tracing::info!(
        event = "mock.model_catalog",
        count = items.len(),
        "GET /model"
    );
    (StatusCode::OK, Json(Value::Array(items)))
}

/// Стендовая ручка времени: все отметки сессии -= ms (сатаурирующий
/// вычет). Дример-сценарии стенда: сессия «уснула» 31+ минут назад без
/// реального ожидания; revival = свежий prompt + повторный age.
/// Форма 2.3 (пин #1254): time.updated в сообщениях нет — активность
/// хвоста читается по цепочке created-полей, поэтому reply/turn сдвигаются
/// наравне с updated_ms (контракт ручки «сессия замерла N минут назад»).
async fn age_session(store: Arc<Store>, id: String, body: Value) -> Resp {
    let ms = body["ms"].as_u64().unwrap_or(0);
    let mut sessions = store.sessions.lock().expect("store");
    match sessions.get_mut(&id) {
        Some(s) => {
            s.updated_ms = s.updated_ms.saturating_sub(ms);
            if let Some((created, _)) = &mut s.reply {
                *created = created.saturating_sub(ms);
            }
            if let Some(turn) = &mut s.turn_ms {
                *turn = turn.saturating_sub(ms);
            }
            let updated = s.updated_ms;
            tracing::info!(event = "mock.session_aged", %id, by_ms = ms, updated,
                "POST /session/{id}/age");
            (
                StatusCode::OK,
                Json(json!({ "data": { "id": id, "updated": updated } })),
            )
        }
        None => (StatusCode::NOT_FOUND, Json(json!({ "status": "404" }))),
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
    // lost-ручки стенда #1288: отказ доставки до любых эффектов хода —
    // сессия остаётся в store (GET 200 → probe Alive) и в списке (десинк)
    // либо скрыта по directory (lost-resurrection). Нумерация попыток
    // доставки общая: порог AFTER отсчитывает успешные доставки.
    if session_is_lost(&store, &session.directory) {
        tracing::warn!(event = "mock.prompt_lost", %id, "lost-directory ручка: prompt → 404");
        return (StatusCode::NOT_FOUND, Json(json!({ "status": "404" })));
    }
    if store.prompt_404_after > 0 {
        let n = store.prompt_ok.fetch_add(1, Ordering::SeqCst) + 1;
        if n > store.prompt_404_after {
            tracing::warn!(event = "mock.prompt_lost", %id, attempt = n,
                after = store.prompt_404_after, "AFTER-ручка: prompt → 404");
            return (StatusCode::NOT_FOUND, Json(json!({ "status": "404" })));
        }
    }
    tracing::info!(event = "mock.prompt", %id, len = text.len(),
        head = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default(),
        "POST /prompt");
    let mut sessions = store.sessions.lock().expect("store");
    if let Some(s) = sessions.get_mut(&id) {
        s.updated_ms = now_ms();
        s.busy_ms = now_ms(); // Started: ход в полёте (CUP-учёт, #1231)

        if mode == "busy" {
            // ход «висит»: busy-носитель жертв преемпшена (#1259 e2e)
            s.busy = true;
        }
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
        // реплай бота готов сразу (storm/busy — исключения: storm прерывает
        // без ответа, busy держит ход «висящим» — #1259 e2e)
        if mode != "storm" && mode != "busy" {
            let tail = text
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default();
            s.reply = Some((now_ms(), format!("мок-ответ на: {tail}")));
            // ход мгновенно завершён — idle ≥ busy (контракт бинаря 2.3.0,
            // сверено #1231): завершённый plain-ход не держит CUP-кап
            // (#1313 V5 — иначе dev-сессии висят busy 30 мин до зомби-гарда)
            s.idle_ms = now_ms();
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
        "busy" => {
            // ход «висит» до abort — completion не нужен (#1259 e2e)
        }
        _ => {
            // plain/idle/gate: ход завершается после step_ms — idle-марка
            // снимает zombie-busy CUP-учёта, токены поворота кормят
            // компакт-пробу занятости (#1221)
            let store2 = store.clone();
            let sid = id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(step_ms)).await;
                let mut sessions = store2.sessions.lock().expect("store");
                if let Some(s) = sessions.get_mut(&sid) {
                    s.busy = false;
                    s.updated_ms = now_ms();
                    s.idle_ms = now_ms();
                    s.outcome = "succeeded".into();
                    s.tokens_in = 900;
                    s.tokens_out = 100;
                }
            });
        }
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
    // test:hold — тихий ход (#1607): пост в ленту не пишется, статус не
    // двигается; флаг — мимо безусловного task_comment ниже.
    let mut silent_hold = false;
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
                "test-review" => {
                    status = "Тестирование";
                    verdict = "тест-ревью request_changes (мок) — находки тестировщику";
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
        } else if stage == "test" && v == "test:review" {
            // pass без тест-коммитов: тестировщик ведёт в Review мимо
            // ревью тестирования (#1413 — гиркин «без тестовых коммитов»)
            status = "Review";
            verdict = "тест pass, без тест-коммитов (мок)";
        } else if stage == "test" && v == "test:hold" {
            // строка остаётся в Testing без единого поста: тестер
            // отработал ход (idle), вердикт не двигает статус и не пишет
            // в ленту — детерминированный стенд повторного входа (#1607:
            // хвост пуст, вход доставляет re-entry wake, не wakeup).
            // Статус не трогаем (дефолт стадии не применяется —
            // silent_hold выходит до task_edit).
            silent_hold = true;
        } else if stage == "dev" && v == "dev:hold" {
            // строка остаётся в In Progress без единого поста: дев
            // отработал ход, вердикт не двигает статус — стенд
            // повторного входа тестера (#1607): между двумя ручными
            // переключениями статуса мок-акторы молчат.
            silent_hold = true;
        }
    }
    if silent_hold {
        // тихий ход: завершён, постов нет — будить нечем, выходим до
        // task_comment/task_edit (лента и статус не тронуты).
        return;
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
/// ручки MOCK_TURN_STREAM_MS / MOCK_TURN_TRAP_MS (дефолты под поллинг
/// раннера 2с, reply_poll_ms); контракт-тест формы (#1254) растягивает
/// стрим-окно — проба детерминированно попадает в фазу.
/// Форма — живой опенкод 2.3 (пин #1254): обёртка {data:[...]}, у
/// assistant created/firstToken/streamed (финал + completed),
/// time.updated нет ни у одного события (в т.ч. у user).
fn turn_tail(turn_ms: u64, stream_ms: u64, trap_ms: u64) -> Value {
    const STEP_TEXT: &str = "промежуточный шаг хода";
    const FINAL_TEXT: &str = "turn final ok";
    let now = now_ms();
    let step1 = json!({
        "type": "assistant",
        "role": "assistant",
        "time": {
            "created": turn_ms,
            "firstToken": turn_ms + 10,
            "streamed": turn_ms + 20,
        },
        "content": [
            { "type": "text", "text": STEP_TEXT },
            { "type": "tool", "state": { "status": "completed" } },
        ],
    });
    let user = json!({
        "type": "user",
        "time": { "created": turn_ms - 100 },
    });
    if now < turn_ms + stream_ms {
        // стримится: без finish — не ответ
        let mut msg = step1.clone();
        msg["finish"] = Value::Null;
        return json!({ "data": [msg, user] });
    }
    if now < turn_ms + trap_ms {
        // шаг завершён, ход продолжается — ловушка finished+is_latest
        let mut msg = step1.clone();
        msg["finish"] = json!("tool-calls");
        return json!({ "data": [msg, user] });
    }
    let final_msg = json!({
        "type": "assistant",
        "role": "assistant",
        "time": {
            "created": turn_ms + trap_ms,
            "firstToken": turn_ms + trap_ms + 10,
            "streamed": turn_ms + trap_ms + 20,
            // completed — момент завершения хода, не момент запроса: старение
            // turn_ms (#1313 V5) уводит активность в прошлое, сталл-тишина копится
            "completed": (turn_ms + trap_ms).min(now),
        },
        "finish": "stop",
        "tokens": { "input": 1234, "output": 50, "reasoning": 0,
                    "cache": { "read": 0, "write": 0 } },
        "content": [{ "type": "text", "text": FINAL_TEXT }],
    });
    json!({ "data": [final_msg, step1, user] })
}

async fn message_tail(store: Arc<Store>, id: String) -> Resp {
    let sessions = store.sessions.lock().expect("store");
    match sessions.get(&id) {
        Some(s) if s.turn_ms.is_some() => {
            let tail = turn_tail(
                s.turn_ms.expect("turn_ms checked"),
                store.turn_stream_ms,
                store.turn_trap_ms,
            );
            (StatusCode::OK, Json(stamp_activity_age(tail)))
        }
        Some(s) => {
            // Форма — живой опенкод 2.3 (пин #1254): хвост обёрнут
            // {data:[...]}, time.updated нет ни у одного события; ответ
            // ассистента несёт created/firstToken/streamed/completed,
            // сессия без ответа открывается idle-событием (только
            // created — активность хвоста по нему же). User-сообщения
            // команд (вердикты env-gc, #1431) идут перед ним в desc —
            // свежайший первым, как order=desc живого API
            let mut tail: Vec<Value> = s
                .user_messages
                .iter()
                .rev()
                .map(|(created_ms, text)| {
                    json!({
                        "type": "user",
                        "role": "user",
                        "time": { "created": created_ms },
                        "content": [{ "type": "text", "text": text }],
                    })
                })
                .collect();
            let msg = if let Some((created_ms, text)) = &s.reply {
                json!({
                    "type": "assistant",
                    "role": "assistant",
                    "time": {
                        "created": created_ms,
                        "firstToken": created_ms + 10,
                        "streamed": created_ms + 20,
                        "completed": created_ms + 30,
                    },
                    "finish": "stop",
                    "tokens": {
                        "input": s.tokens_in, "output": s.tokens_out, "reasoning": 0,
                        "cache": { "read": 0, "write": 0 }
                    },
                    "content": [{ "type": "text", "text": text }],
                })
            } else {
                json!({
                    "type": "idle",
                    "time": { "created": s.updated_ms },
                })
            };
            tail.push(msg);
            (
                StatusCode::OK,
                Json(stamp_activity_age(json!({ "data": tail }))),
            )
        }
        None => (StatusCode::NOT_FOUND, Json(json!({ "status": "404" }))),
    }
}

/// MOCK_ACTIVITY_AGE_MS (стенд судьи #1364): активность насильно старая —
/// тишина сессии фиксированно глубока, сталл-контур (и судья) срабатывает
/// в первых тиках стенда. Пост-стамп на ОБА ветвления хвоста (turn-фазы и
/// idle-реплай): свежий штамп любого ветвления обнулял бы тишину —
/// silent None сталл-контура (три красных прогона e2e 14a, 20.11).
/// Форма 2.3 (пин #1254, merge раунда 4): хвост обёрнут {data:[...]},
/// time.updated в событиях нет — проба активности читает цепочку
/// completed/streamed/firstToken/created (приоритет пробы), штампуются
/// все отметки времени каждого сообщения.
fn stamp_activity_age(mut tail: Value) -> Value {
    let age = cfg("MOCK_ACTIVITY_AGE_MS", "0").parse::<u64>().unwrap_or(0);
    if age == 0 {
        return tail;
    }
    let old = now_ms().saturating_sub(age);
    // обе формы хвоста: голый массив (turn-ветка исторически) и обёртка
    // {data:[...]} (пин #1254) — неожиданная форма проходит без штампа
    let msgs = if tail.is_array() {
        tail.as_array_mut()
    } else {
        tail.get_mut("data").and_then(Value::as_array_mut)
    };
    if let Some(msgs) = msgs {
        for m in msgs.iter_mut() {
            if let Some(t) = m.get_mut("time") {
                for key in ["updated", "completed", "streamed", "firstToken", "created"] {
                    t[key] = json!(old);
                }
            }
        }
    }
    tail
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
        // маркеры приходят из источника (STAGE_PROMPTS spawn_prompt,
        // ANALYST*_WAVE_MARKER волн) — копий нет, связка механическая;
        // проверяется сам маппинг «маркер → канон-статус» мока
        let stage_status = |slot| match slot {
            uwuwu_domain::Slot::Design => "Plan review",
            uwuwu_domain::Slot::Dev => "Result review",
            uwuwu_domain::Slot::PlanReview => "In Progress",
            uwuwu_domain::Slot::ResultReview => "Testing",
            uwuwu_domain::Slot::Test => "Testing review",
            uwuwu_domain::Slot::TestReview => "Review",
        };
        for (slot, marker) in uwuwu_usecase::STAGE_PROMPTS {
            let (_, status, _) = stage_action_of_prompt(marker).unwrap();
            assert_eq!(status, stage_status(*slot), "маркер «{marker}»");
        }
        let wave = uwuwu_usecase::ANALYST_WAVE_MARKER.replace("{project}", "p");
        let (_, status, _) = stage_action_of_prompt(&wave).unwrap();
        assert_eq!(status, "Review Analys", "волна аналитика");
        let review_wave = uwuwu_usecase::ANALYST_REVIEW_WAVE_MARKER.replace("{project}", "p");
        let (_, status, _) = stage_action_of_prompt(&review_wave).unwrap();
        assert_eq!(status, "Todo", "волна ревью аналитики");
        // триаж выведен (#1135): старого маркера нет; русские легаси-маркеры
        // круга 1 больше не матчатся (мёртвый контракт не пинится)
        assert_ne!(
            stage_action_of_prompt("Триаж черновика #5").unwrap().0,
            "triage"
        );
        assert_ne!(
            stage_action_of_prompt("стадия утряски: изучи постановку")
                .unwrap()
                .0,
            "design",
            "легаси-русский маркер мёртв после #1383"
        );
        // wakeup/крон/дример — без перехода статуса
        assert_eq!(
            stage_action_of_prompt("Wakeup: new comments on task")
                .unwrap()
                .1,
            ""
        );
        assert_eq!(stage_action_of_prompt("Cron run «X»").unwrap().1, "");
    }

    #[test]
    fn test_review_scripted_verdicts_follow_canon() {
        // #1413: сценарные вердикты тест-контура — гиркин-ветки; маркеры —
        // из STAGE_PROMPTS-констант (канон #1383), не копии
        let test_marker = uwuwu_usecase::STAGE_PROMPTS
            .iter()
            .find(|(slot, _)| *slot == uwuwu_domain::Slot::Test)
            .map(|(_, m)| *m)
            .unwrap();
        let (stage, status, _) = stage_action_of_prompt(test_marker).unwrap();
        assert_eq!((stage, status), ("test", "Testing review"));
        let test_review_marker = uwuwu_usecase::STAGE_PROMPTS
            .iter()
            .find(|(slot, _)| *slot == uwuwu_domain::Slot::TestReview)
            .map(|(_, m)| *m)
            .unwrap();
        let (stage, status, _) = stage_action_of_prompt(test_review_marker).unwrap();
        assert_eq!((stage, status), ("test-review", "Review"));
        // action_for_status: wakeup-путь по текущему статусу строки —
        // доменный парсер принимает и канон, и легаси-русское имя
        let (next, _) = action_for_status("Testing");
        assert_eq!(next, "Testing review");
        let (next, _) = action_for_status("Testing review");
        assert_eq!(next, "Review");
        let (next_legacy, _) = action_for_status("Тестирование");
        assert_eq!(
            next_legacy, "Testing review",
            "легаси-имя резолвится в тот же переход"
        );
    }

    #[test]
    fn every_mock_status_assignment_is_canonical_key() {
        // Регресс-пин тест-круга #1413: RC-ветка test-review слала "Testing" —
        // строка не является ключом parse_status (канон «Тестирование»):
        // серверный гард резал правку («переход ReviewTesting → Empty вне
        // канона конвейера»), строка зависала в «Ревью тестирования»,
        // сценарный ключ "test-review:request_changes" не работал вживую
        // (дефолтный стенд ветку не гоняет — только approve-путь). Каждый
        // литерал присваивания статуса в драйвере обязан быть канон-ключом
        // носителя; скан по исходнику ловит класс целиком (прецедент —
        // mock_route_coverage: скан литералов вместо ручной таблицы).
        let src = include_str!("mock_opencode.rs");
        let needle = "status = \"";
        let mut scanned: Vec<&str> = Vec::new();
        let mut i = 0;
        while let Some(pos) = src[i..].find(needle) {
            let start = i + pos + needle.len();
            let Some(end) = src[start..].find('"') else {
                panic!("литерал статуса оборван (offset {start})");
            };
            scanned.push(&src[start..start + end]);
            i = start + end;
        }
        assert!(
            scanned.len() >= 5,
            "скан должен находить все сайты присваивания: {scanned:?}"
        );
        for s in &scanned {
            assert!(
                !matches!(uwuwu_usecase::parse_status(s), uwuwu_domain::Status::Empty),
                "мок шлёт невалидный статус-ключ «{s}» — гард режет правку"
            );
        }
    }
}
