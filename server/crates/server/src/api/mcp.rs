//! MCP presentation: streamable HTTP (JSON-RPC) на POST /mcp.
//! Stateless-сервер: GET/DELETE → 405 (спека допускает), сессий нет —
//! клиенты обязаны это переваривать. Auth — bearer (тот же, что /health).

use crate::api::tools::ToolHub;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;

pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Потолок тела MCP-запроса: blob_put гонит файл base64-строкой в JSON-RPC
/// (20 MiB → ~28 MiB тело) — дефолт axum (2 MiB) отрезал бы молча (#1292).
/// Цена осознана: потолок общий для всех MCP-запросов; тяжёлые тела ездят
/// только через blob_put (контекст памяти — #1294).
pub const BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;

pub struct McpState {
    pub bearer: Arc<String>,
    pub tools: Arc<ToolHub>,
}

pub fn router(state: Arc<McpState>) -> Router {
    Router::new()
        .route("/mcp", post(handle).get(reject).delete(reject))
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
        .with_state(state)
}

async fn reject() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, "stateless MCP: POST only")
}

async fn handle(
    State(state): State<Arc<McpState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if auth.strip_prefix("Bearer ").unwrap_or_default() != *state.bearer {
        return json_error(None, -32000, "unauthorized");
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return json_error(None, -32700, &format!("parse error: {e}")),
    };
    // batch — обрабатываем только первое сообщение (задокументированное
    // упрощение stateless-сервера; клиенты шлют по одному сообщению)
    let msg = if msg.is_array() {
        match msg.as_array().and_then(|a| a.first()) {
            Some(first) => first.clone(),
            None => return json_error(None, -32600, "empty batch"),
        }
    } else {
        msg
    };
    let id = msg.get("id").cloned();
    let method = msg
        .get("method")
        .and_then(|m| m.as_str())
        .unwrap_or_default();
    if id.is_none() {
        // notification (initialized и пр.) — 202 без тела
        return (
            StatusCode::ACCEPTED,
            [(axum::http::header::CONTENT_TYPE, "text/plain")],
            "",
        )
            .into_response();
    }
    let result = match method {
        "initialize" => Some(initialize_result()),
        "tools/list" => Some(json!({ "tools": state.tools.descriptors() })),
        "tools/call" => match state.tools.call(&msg).await {
            Ok(content) => Some(json!({ "content": content, "isError": false })),
            Err(user_msg) => Some(json!({
                "content": [{ "type": "text", "text": user_msg }],
                "isError": true
            })),
        },
        "ping" => Some(json!({})),
        _ => None,
    };
    match result {
        Some(result) => json_ok(id, result),
        None => json_error(id, -32601, &format!("method not found: {method}")),
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "uwuwu-server",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

fn json_ok(id: Option<Value>, result: Value) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        ))
        .expect("static response")
}

fn json_error(id: Option<Value>, code: i64, message: &str) -> Response {
    Response::builder()
        .status(if code == -32000 {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::OK
        })
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
                .to_string(),
        ))
        .expect("static response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{blob_usecase, StubSessions, StubState, StubWiki};
    use crate::clock::SystemClock;
    use crate::mockhub::MemoryHub;
    use tower::ServiceExt;
    use uwuwu_usecase::{SearchUseCase, TaskBoardRepo, TaskView, TaskWriteUseCase, WikiRepo};

    fn app() -> Router {
        let memory = Arc::new(MemoryHub::from_env());
        let board: Arc<dyn TaskBoardRepo> = memory.clone();
        let wiki: Arc<dyn WikiRepo> = Arc::new(StubWiki);
        let state = Arc::new(McpState {
            bearer: Arc::new("test-bearer".to_string()),
            tools: Arc::new(ToolHub {
                tasks: Arc::new(TaskView::new(
                    board.clone(),
                    vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())],
                )),
                docs: Arc::new(uwuwu_usecase::DocsView::new(wiki.clone())),
                person: Arc::new(uwuwu_usecase::PersonView::new(wiki)),
                writer: Arc::new(TaskWriteUseCase {
                    board,
                    fields: memory,
                    boards: vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())],
                    clock: Arc::new(SystemClock),
                    state: Arc::new(StubState),
                }),
                wiki: Arc::new(StubWiki),
                sessions: Arc::new(uwuwu_usecase::SessionRouter::uniform(Arc::new(
                    StubSessions,
                ))),
                search: Arc::new(SearchUseCase { semantic: None }),
                blobs: blob_usecase(),
                crons: Arc::new(uwuwu_usecase::CronUseCase {
                    wiki: Arc::new(StubWiki),
                    sessions: Arc::new(uwuwu_usecase::SessionRouter::uniform(Arc::new(
                        StubSessions,
                    ))),
                    clock: Arc::new(SystemClock),
                    models: Arc::new(uwuwu_usecase::SpawnModels::canonical()),
                    checkout: String::new(),
                    cup: 7,
                }),
            }),
        });
        router(state)
    }

    /// Лимит тела отрезает на extractor'е: тело сверх BODY_LIMIT_BYTES —
    /// 413 до bearer/парсинга (дефолт 2 MiB отрезал бы blob_put молча, #1292).
    #[tokio::test]
    async fn oversized_body_is_rejected_with_413() {
        let oversized = vec![b'x'; BODY_LIMIT_BYTES + 1];
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .method(axum::http::Method::POST)
                    .uri("/mcp")
                    .header("authorization", "Bearer test-bearer")
                    .body(axum::body::Body::from(oversized))
                    .expect("request ok"),
            )
            .await
            .expect("service ok");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// Тело в пределах лимита доходит до обработчика (без-cookie → 401
    /// от bearer-гарда — значит лимит пропустил).
    #[tokio::test]
    async fn body_within_limit_passes_to_handler() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec();
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .method(axum::http::Method::POST)
                    .uri("/mcp")
                    .header("authorization", "Bearer test-bearer")
                    .body(axum::body::Body::from(body))
                    .expect("request ok"),
            )
            .await
            .expect("service ok");
        assert_eq!(response.status(), StatusCode::OK);
    }
}
