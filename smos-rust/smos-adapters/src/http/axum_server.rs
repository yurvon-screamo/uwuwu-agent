//! axum server wiring: `AppState`, router, serve + graceful shutdown.
//!
//! `AppState` owns the concrete adapters (SurrealStore, OllamaEmbedding,
//! LlamaCppReranker, ReqwestUpstream, …) plus the active config snapshots the
//! handlers need. Slice-4 wires the full chat-completion pipeline through
//! `HandleChatCompletion`; the per-request use-case struct is built inline in
//! the handler so this state remains flat and easy to assemble in tests.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use smos_application::ports::Clock;
use smos_domain::config::{ConfidenceConfig, ExtractionConfig, HeatConfig, RetrievalConfig};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::SurrealStore;
use crate::config::SmosConfig;
use crate::http::routes;
use crate::providers::{LlamaCppReranker, OllamaEmbedding, OllamaExtractor};
use crate::runtime::ExtractionSupervisor;
use crate::upstream::ReqwestUpstream;

/// Shared state handed to every handler via axum's `State` extractor.
///
/// The concrete adapter types are baked in (no `dyn` traits) because the
/// `async fn` port traits are not object-safe and a single-binary proxy does
/// not need the indirection. Tests and the production binary assemble this
/// from the same concrete pieces; changing an adapter means changing this
/// struct (and its call sites) — that is the intended trade-off.
pub struct AppState {
    pub config: Arc<SmosConfig>,
    pub store: SurrealStore,
    pub embedder: OllamaEmbedding,
    pub reranker: LlamaCppReranker,
    /// Slice-5 response extractor (Ollama Qwen3.5-2B via `/api/chat`).
    pub extractor: OllamaExtractor,
    pub upstream: ReqwestUpstream,
    pub clock: Arc<dyn Clock + Send + Sync>,
    pub retrieval_cfg: Arc<RetrievalConfig>,
    pub heat_cfg: Arc<HeatConfig>,
    pub confidence_cfg: Arc<ConfidenceConfig>,
    /// Semantic-dedup safety net for the background extractor
    /// (`persist_facts` step 2). Owned by the shared state so every
    /// [`ResponseExtractionSpawner`] clone hands the same snapshot to the
    /// background task.
    pub extraction_cfg: Arc<ExtractionConfig>,
    /// Tracks background extraction tasks so `serve` can drain them on
    /// shutdown (see `shutdown_extraction_grace_seconds`).
    pub extraction_supervisor: ExtractionSupervisor,
}

/// Build the router with both routes, permissive CORS (OpenAI clients are
/// browser-callable), and HTTP tracing.
///
/// # CORS — known security trade-off (tracked for a follow-up slice)
///
/// `CorsLayer::permissive()` emits `Access-Control-Allow-Origin: *`, which in
/// `rules-security` is a red flag (OWASP A05 Security Misconfiguration). We
/// keep the permissive layer for Slice-4 because:
///
/// 1. The default bind is `127.0.0.1` (localhost-only) — the cross-origin
///    attack surface is empty unless an operator overrides `host`.
/// 2. OpenAI clients are routinely browser-driven and a strict default would
///    break first-contact usage.
///
/// **Production hardening (before any non-localhost deploy):** add a
/// `[server].allowed_origins` config field and emit an explicit origin list.
/// If `host = "0.0.0.0"` is configured together with the permissive layer,
/// the proxy would let any browser origin drive both the proxy and its
/// upstream bearer token — fail-secure there before flipping the host.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/v1/chat/completions",
            post(routes::chat_completions::handle),
        )
        .route("/health", get(routes::health::handle))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Bind `host:port` and serve the router until graceful shutdown completes.
///
/// # Shutdown drain
/// `axum::serve`'s graceful shutdown waits for in-flight HTTP connections to
/// finish (bounded by `upstream.timeout_seconds`). After the HTTP layer
/// drains, this function ALSO drains background extraction tasks for
/// `shutdown_extraction_grace_seconds` — so a Ctrl+C does not silently cancel
/// half-finished fact extraction (the response already reached the client, so
/// a cancelled extraction is an unrecoverable loss).
pub async fn serve(
    router: Router,
    host: &str,
    port: u16,
    extraction_supervisor: ExtractionSupervisor,
    extraction_grace: Duration,
) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind((host, port)).await?;
    tracing::info!(host, port, "SMOS HTTP server listening");
    serve_with_shutdown(
        listener,
        router,
        extraction_supervisor,
        extraction_grace,
        shutdown_signal(),
    )
    .await
}

/// Serve + drain core, parameterised over the shutdown signal so the wiring
/// (axum shutdown → supervisor drain) is integration-testable without firing
/// a real OS signal.
///
/// Public since Slice-7: the watcher-aware main binary composes this with the
/// `SessionWatcher` drain — the HTTP module still does not know the watcher
/// type, the main binary just calls `serve_with_shutdown` first and then
/// drives the watcher shutdown channel + join handle.
pub async fn serve_with_shutdown<F>(
    listener: tokio::net::TcpListener,
    router: Router,
    extraction_supervisor: ExtractionSupervisor,
    extraction_grace: Duration,
    shutdown: F,
) -> Result<(), std::io::Error>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await?;
    tracing::info!(
        ?extraction_grace,
        "draining background extraction tasks before shutdown"
    );
    extraction_supervisor.drain(extraction_grace).await;
    Ok(())
}

/// Wait for Ctrl+C (all platforms) or SIGTERM (unix). Returns on the first
/// signal so `axum::serve` stops accepting new connections and drains.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Ctrl+C received, initiating graceful shutdown"),
        _ = terminate => tracing::info!("SIGTERM received, initiating graceful shutdown"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The serve()→drain wiring: after axum's graceful shutdown completes,
    /// `serve_with_shutdown` MUST drain the extraction supervisor before
    /// returning. We inject an immediate-resolving shutdown future so no OS
    /// signal is needed, spawn one tracked task, and assert it ran to
    /// completion before `serve_with_shutdown` returned.
    #[tokio::test]
    async fn serve_with_shutdown_drains_extraction_supervisor() {
        let supervisor = ExtractionSupervisor::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let tracked = counter.clone();
        supervisor.spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tracked.fetch_add(1, Ordering::SeqCst);
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let router = Router::new();
        // Shutdown resolves immediately → axum stops accepting → drain runs.
        serve_with_shutdown(
            listener,
            router,
            supervisor,
            Duration::from_secs(2),
            async {},
        )
        .await
        .unwrap();
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "tracked extraction task must complete during drain before serve returns"
        );
    }
}
