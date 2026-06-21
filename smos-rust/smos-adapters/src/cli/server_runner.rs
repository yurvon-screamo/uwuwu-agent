//! `smos serve` — proxy server runner.
//!
//! Owns the §12 drain ordering (HTTP → extraction → watcher) and the
//! optional-watcher degrade behaviour.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::cli::shutdown::shutdown_signal;
use crate::cli::tracing_setup::init_tracing_for_server;
use crate::config::SmosConfig;
use crate::http::axum_server::{AppState, build_router, is_loopback_host, serve_with_shutdown};
use crate::nli::build_classifier;
use crate::runtime::{ExtractionSupervisor, SessionWatcher};
use crate::upstream::ReqwestUpstreamPool;
use crate::{
    LlamaCppReranker, OllamaEmbedding, OllamaExtractor, SurrealStore, SystemClock,
    SystemIdGenerator,
};
use smos_application::ports::{Clock, IdGenerator};

/// Handle returned by [`spawn_watcher`] so [`run_server`] can drive the
/// §12 drain ordering. The watcher task + its shutdown sender live or die
/// together; `None` means no NLI backend was available so the watcher
/// never started.
type WatcherHandle = Option<(tokio::task::JoinHandle<()>, mpsc::Sender<()>)>;

/// Start the SMOS proxy (default `smos serve` mode).
pub async fn run_server(config_path: &str) -> Result<()> {
    let config = SmosConfig::load(config_path)?;
    init_tracing_for_server(&config.server);

    warn_on_insecure_config(&config);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        upstream_providers = config.upstream.providers.len(),
        upstream_strategy = %config.upstream.strategy.mode,
        extraction_url = %config.llm_extraction.url,
        embedding_url = %config.embedding.url,
        reranker = %config.reranker.url,
        host = %config.server.host,
        port = config.server.port,
        "starting SMOS proxy"
    );

    let store = SurrealStore::connect(
        &config.surreal.path,
        &config.surreal.namespace,
        &config.surreal.database,
    )
    .await?;
    store.run_migrations().await?;

    // ExtractionSupervisor is `#[derive(Clone)]` with shared `Arc` interior,
    // so both clones observe the same in-flight counter — required for the
    // §12 drain to wait on tasks spawned through the AppState clone.
    let extraction_supervisor = ExtractionSupervisor::new();

    let state = build_app_state(&config, store.clone(), extraction_supervisor.clone())?;
    let watcher_handle = spawn_watcher(&config, store.clone()).await;

    let router = build_router(Arc::new(state));
    let listener =
        tokio::net::TcpListener::bind((config.server.host.as_str(), config.server.port)).await?;
    tracing::info!(
        host = %config.server.host,
        port = config.server.port,
        "SMOS HTTP server listening"
    );

    let extraction_grace =
        std::time::Duration::from_secs(config.server.shutdown_extraction_grace_seconds);
    serve_with_shutdown(
        listener,
        router,
        extraction_supervisor,
        extraction_grace,
        shutdown_signal(),
    )
    .await?;

    drain_watcher(watcher_handle).await;

    tracing::info!("SMOS proxy stopped");
    Ok(())
}

/// Emit a startup warning when the operator is about to ship a request
/// whose bearer token is the built-in placeholder, or when permissive
/// CORS meets a non-localhost bind.
fn warn_on_insecure_config(config: &SmosConfig) {
    let is_loopback = is_loopback_host(&config.server.host);

    // Inspect every configured provider's api_key. A placeholder key is
    // acceptable on loopback (local Ollama); on a non-localhost bind it is
    // an outright insecure configuration and gets an ERROR-level log so
    // the operator notices before going to production.
    for provider in &config.upstream.providers {
        if is_placeholder_key(&provider.api_key) {
            if is_loopback {
                tracing::warn!(
                    provider = %provider.name,
                    api_key = %provider.api_key,
                    "upstream api_key is a known placeholder; set SMOS__UPSTREAM__API_KEY \
                     before exposing the proxy on a non-localhost interface"
                );
            } else {
                tracing::error!(
                    provider = %provider.name,
                    host = %config.server.host,
                    "api_key is a known placeholder AND host is non-localhost — this is \
                     insecure. Set a real api_key before deploying."
                );
            }
        }
    }

    let is_wildcard_host = matches!(config.server.host.as_str(), "0.0.0.0" | "::" | "[::]" | "*");
    if is_wildcard_host {
        tracing::warn!(
            host = %config.server.host,
            "server.host binds to a non-localhost interface; the router ships an \
             EMPTY CORS layer (no Access-Control-Allow-* headers are emitted, so \
             browsers block cross-origin requests by default). Same-origin requests \
             and non-browser clients (curl) keep working. Add an explicit origin \
             allow-list (`[server].allowed_origins`) if browser-driven cross-origin \
             access is needed."
        );
    }
}

/// Known placeholder api_keys that MUST NOT be used outside loopback.
///
/// `ollama` is the canonical placeholder for local Ollama (which ignores
/// the key). `changeme`, `test`, `password`, `secret`, and the `sk-test*`
/// family are the textbook examples operators reach for when they "just
/// want to get it running" — flagging them prevents a copy-paste from a
/// tutorial ending up in production.
const PLACEHOLDER_API_KEYS: &[&str] = &[
    "ollama", "changeme", "sk-test", "test", "password", "secret", "",
];

fn is_placeholder_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    PLACEHOLDER_API_KEYS.iter().any(|p| lower == *p) || lower.starts_with("sk-test")
}

/// Wire every concrete adapter into [`AppState`] so the axum router can
/// reach storage, providers, upstream, and the extraction supervisor.
fn build_app_state(
    config: &SmosConfig,
    store: SurrealStore,
    extraction_supervisor: ExtractionSupervisor,
) -> Result<AppState> {
    let upstream = ReqwestUpstreamPool::new(&config.upstream)?;
    let embedder = OllamaEmbedding::new(Arc::new(config.embedding.clone()))?;
    let reranker = LlamaCppReranker::new(Arc::new(config.reranker.clone()))?;
    let extractor = OllamaExtractor::new(Arc::new(config.llm_extraction.clone()))?;
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(SystemClock);
    let id_generator: Arc<dyn IdGenerator + Send + Sync> = Arc::new(SystemIdGenerator);
    let retrieval_cfg = Arc::new(config.retrieval.clone());
    let heat_cfg = Arc::new(config.heat.clone());
    let confidence_cfg = Arc::new(config.confidence.clone());
    let extraction_cfg = Arc::new(config.extraction.clone());

    Ok(AppState {
        config: Arc::new(config.clone()),
        store,
        embedder,
        reranker,
        extractor,
        upstream,
        clock,
        id_generator,
        retrieval_cfg,
        heat_cfg,
        confidence_cfg,
        extraction_cfg,
        extraction_supervisor,
    })
}

/// Spawn the NLI backend (optional) and the [`SessionWatcher`] that uses
/// it. Returns `None` when the backend failed to start so the caller can
/// keep serving HTTP without NLI — chat completions never need NLI, so a
/// failed startup degrades to "watcher disabled" rather than crashing.
async fn spawn_watcher(config: &SmosConfig, store: SurrealStore) -> WatcherHandle {
    let classifier = match build_classifier(config).await {
        Ok(c) => {
            tracing::info!(
                model = %config.nli_backend.model,
                "NLI backend started for session watcher"
            );
            c
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "NLI backend failed to start; session watcher disabled \
                 (HTTP server still serves chat completions). Restart the \
                 proxy once the model / interpreter is available."
            );
            return None;
        }
    };

    let watcher = SessionWatcher::new(
        store.clone(),
        store.clone(),
        classifier,
        Arc::new(config.confidence.clone()),
        Arc::new(config.nli.clone()),
        Arc::new(config.merge.clone()),
        Arc::new(config.session.clone()),
        Arc::new(config.server.clone()),
    );
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
    // Spawn at a concrete-type call site so the `Send` bound on
    // `tokio::spawn` discharges against `SurrealStore` +
    // `NativeNliClassifier` (both return `Send` futures).
    let handle = tokio::spawn(watcher.into_loop(shutdown_rx));
    Some((handle, shutdown_tx))
}

/// §12 ordering step 4: stop the watcher scan loop and drain every
/// still-tracked session through FinalizeSession so pending facts reach
/// `Accepted` / `Rejected` before the process exits.
async fn drain_watcher(watcher_handle: WatcherHandle) {
    if let Some((handle, shutdown_tx)) = watcher_handle {
        let _ = shutdown_tx.send(()).await;
        let _ = handle.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_loopback_host_recognises_canonical_loopback() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));
    }

    #[test]
    fn is_loopback_host_rejects_wildcard_and_public() {
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("192.168.0.1"));
        assert!(!is_loopback_host("smos.example.com"));
    }

    #[test]
    fn is_placeholder_key_flags_known_placeholders() {
        for k in ["ollama", "changeme", "test", "password", "secret", ""] {
            assert!(
                is_placeholder_key(k),
                "expected {k:?} to be flagged as a placeholder"
            );
        }
    }

    #[test]
    fn is_placeholder_key_flags_sk_test_prefix() {
        assert!(is_placeholder_key("sk-test-abc"));
        assert!(is_placeholder_key("SK-TEST-UPPER"));
    }

    #[test]
    fn is_placeholder_key_passes_through_real_keys() {
        assert!(!is_placeholder_key("sk-or-1234567890abcdef"));
        assert!(!is_placeholder_key("live-key-XYZ"));
    }
}
