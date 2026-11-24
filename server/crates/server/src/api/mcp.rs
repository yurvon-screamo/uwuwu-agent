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
    use crate::api::test_support::{blob_usecase, tool_hub, StubCronState, StubWiki};
    use tower::ServiceExt;
    use uwuwu_usecase::TaskBoardRepo;

    fn app() -> Router {
        let (tools, _memory) = tool_hub(
            Arc::new(StubWiki),
            blob_usecase(),
            Arc::new(StubCronState::default()),
        );
        let state = Arc::new(McpState {
            bearer: Arc::new("test-bearer".to_string()),
            tools,
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

    /// Приёмка #1572 на проводе — то, что видит живой MCP-клиент: мутация
    /// без контента (task_body без body) и с контентом в чужих параметрах
    /// (task_body_section find/replace — дословный инстанс foilcap #31)
    /// приезжают HTTP 200 без jsonrpc-ошибки, но с isError: true и текстом
    /// отказа (длина носителя, рецепт хода, перечень чужих ключей); носитель
    /// цел следующим чтением по тому же проводу и в самом хабе.
    #[tokio::test]
    async fn contentless_mutations_refuse_loudly_over_the_wire_and_carrier_survives() {
        let (tools, memory) = tool_hub(
            Arc::new(StubWiki),
            blob_usecase(),
            Arc::new(StubCronState::default()),
        );
        let app = router(Arc::new(McpState {
            bearer: Arc::new("test-bearer".to_string()),
            tools,
        }));

        async fn call_tool(
            app: &Router,
            name: &str,
            args: serde_json::Value,
        ) -> (StatusCode, serde_json::Value) {
            let payload = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": name, "arguments": args }
            });
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(axum::http::Method::POST)
                        .uri("/mcp")
                        .header("authorization", "Bearer test-bearer")
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(payload.to_string()))
                        .expect("request ok"),
                )
                .await
                .expect("service ok");
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), BODY_LIMIT_BYTES)
                .await
                .expect("body ok");
            let msg: serde_json::Value = serde_json::from_slice(&bytes).expect("json ok");
            (status, msg)
        }

        let body = "## Постановка\n\nДержатель массы тела — носитель не должен стереться.\n\n## Проверка\n\nНаполнитель пробы.\n";
        let (status, created) = call_tool(
            &app,
            "task_create",
            json!({ "title": "t: wire", "body": body }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            created.get("error").is_none(),
            "успешная мутация — не jsonrpc-ошибка: {created}"
        );
        assert_eq!(created["result"]["isError"], json!(false), "{created}");
        let created_text = created["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        let number: u64 = created_text
            .trim_start_matches("task #")
            .split(' ')
            .next()
            .expect("номер в квитанции")
            .parse()
            .expect("номер парсится");

        // Сценарий 1: task_body без body — громкий отказ в isError-конверте
        let (status, refused) = call_tool(&app, "task_body", json!({ "number": number })).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            refused.get("error").is_none(),
            "отказ гарда — не jsonrpc-ошибка: {refused}"
        );
        assert_eq!(
            refused["result"]["isError"],
            json!(true),
            "isError-конверт: {refused}"
        );
        let err_text = refused["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            err_text.contains("write refused"),
            "природа отказа названа: {err_text}"
        );
        assert!(
            err_text.contains("bytes"),
            "длина текущего носителя в отказе: {err_text}"
        );
        assert!(err_text.contains("task_get"), "рецепт чтения: {err_text}");
        assert!(
            err_text.contains("body: \"\""),
            "рецепт осознанной очистки: {err_text}"
        );

        // Сценарий 2: контент в чужих параметрах — отказ с перечнем до носителя
        let (_, card) = call_tool(&app, "task_get", json!({ "number": number })).await;
        let hash = card["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .find_map(|l| l.strip_prefix("body_hash: "))
            .unwrap_or_default()
            .trim()
            .to_string();
        assert!(!hash.is_empty(), "fm-шапка card несёт body_hash: {card}");
        let (status, unknown) = call_tool(
            &app,
            "task_body_section",
            json!({
                "number": number,
                "section": "Проверка",
                "find": "x",
                "replace": "y",
                "base_hash": hash
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            unknown.get("error").is_none(),
            "отказ гарда — не jsonrpc-ошибка: {unknown}"
        );
        assert_eq!(
            unknown["result"]["isError"],
            json!(true),
            "isError-конверт: {unknown}"
        );
        let unknown_text = unknown["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            unknown_text.contains("find, replace"),
            "чужие ключи названы: {unknown_text}"
        );
        assert!(
            unknown_text.contains("the carrier is untouched"),
            "прямая фраза «носитель не тронут»: {unknown_text}"
        );

        // Носитель цел следующим чтением по тому же проводу и в самом хабе
        let (_, intact) = call_tool(&app, "task_get", json!({ "number": number })).await;
        let carrier = intact["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            carrier.contains("Держатель массы тела"),
            "носитель цел: {carrier}"
        );
        let stored = memory
            .read_issue("Задачи uwuwu", number)
            .await
            .expect("read")
            .body;
        assert!(
            stored.contains("Наполнитель пробы"),
            "носитель в хабе цел: {stored}"
        );
    }
}
