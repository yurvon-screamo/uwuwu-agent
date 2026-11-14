//! HTTP-presentation: /health под bearer-гейтом (MCP-поверхность — срез 2).

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use std::sync::Arc;
use uwuwu_usecase::TickOutcome;

pub struct AppState {
    pub bearer: Arc<String>,
    pub mode: &'static str,
    pub tick: Arc<std::sync::RwLock<TickOutcome>>,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let token = auth.strip_prefix("Bearer ").unwrap_or_default();
    if token != *state.bearer {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
    }
    let tick = state.tick.read().expect("tick lock").clone();
    (
        StatusCode::OK,
        Json(json!({
            "service": "uwuwu-server",
            "mode": state.mode,
            "version": env!("CARGO_PKG_VERSION"),
            "tick": tick,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app() -> Router {
        let state = Arc::new(AppState {
            bearer: Arc::new("s3cret".into()),
            mode: "observe",
            tick: Arc::new(std::sync::RwLock::new(TickOutcome::default())),
        });
        router(state)
    }

    #[tokio::test]
    async fn health_requires_bearer() {
        let res = app()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn health_returns_summary_with_token() {
        let res = app()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .header("authorization", "Bearer s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["service"], "uwuwu-server");
        assert_eq!(json["mode"], "observe");
    }
}
