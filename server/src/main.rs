//! uwuwu-server — единый сервер контура uwuwu (hub#1133).
//! Срез 1: каркас (config/clock/http) + observe-тик конвейера.
//! Срезы: 1b — спавны/будильники/автодон (live), 2 — MCP, 3 — триаж/дример/кроны,
//! 4 — семантика (LanceDB + OpenRouter), 5 — снос легаси cli.

mod clock;
mod config;
mod domain;
mod fm;
mod http;
mod hub;
mod pipeline;

use crate::clock::SystemClock;
use crate::config::{Config, Mode, ENV_CONFIG};
use crate::hub::GitHubHub;
use crate::pipeline::SharedTick;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    // JSON-логи (Railway ingest), уровень — env RUST_LOG (дефолт info)
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();
    let config_path = std::env::var(ENV_CONFIG)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("server/config.toml"));
    let cfg = Config::load(&config_path)?;
    let secrets = cfg.secrets()?;
    tracing::info!(
        event = "server.started",
        mode = ?cfg.mode,
        projects = cfg.project.len(),
        poll_sec = cfg.poll_sec,
        config = %config_path.display(),
        "Server started"
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(run(cfg, secrets))?;
    Ok(())
}

async fn run(cfg: Config, secrets: config::Secrets) -> anyhow::Result<()> {
    let hub = Arc::new(GitHubHub::new(
        cfg.hub.repo.clone(),
        secrets.gh_token.clone(),
    ));
    let shared: SharedTick = Arc::new(std::sync::RwLock::default());
    let mode_label: &'static str = match cfg.mode {
        Mode::Observe => "observe",
        Mode::Live => "live",
    };
    let state = Arc::new(http::AppState {
        bearer: Arc::new(secrets.bearer.clone()),
        mode: mode_label,
        tick: shared.clone(),
    });

    let http_task = tokio::spawn(serve(cfg.port(), http::router(state)));
    let tick_task = tokio::spawn(tick_loop(cfg, hub, shared));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm() => {},
    }
    tracing::info!(event = "server.stopping", "Server stopping");
    http_task.abort();
    tick_task.abort();
    Ok(())
}

/// SIGTERM (Railway stop): graceful без паники вне unix-фич tokio.
async fn sigterm() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!(event = "sigterm.setup_failed", error = %e, "Failed to listen SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    }
    #[cfg(not(unix))]
    std::future::pending::<()>().await;
}

async fn serve(port: u16, app: axum::Router) {
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(event = "http.bind_failed", port, error = %e, "Failed to bind http port");
            std::process::exit(1);
        }
    };
    tracing::info!(event = "http.listening", port, "Http listening");
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!(event = "http.failed", error = %e, "Http server failed");
    }
}

async fn tick_loop(cfg: Config, hub: Arc<GitHubHub>, shared: SharedTick) {
    let clock = SystemClock;
    loop {
        let started = std::time::Instant::now();
        match pipeline::tick(&cfg, hub.as_ref(), &clock).await {
            Ok(summary) => {
                if let Ok(mut guard) = shared.write() {
                    *guard = summary;
                }
            }
            Err(e) => {
                // тик не роняет сервер: следующий проход через poll_sec
                tracing::error!(
                    event = "tick.failed",
                    error = %e,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "Tick failed"
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(cfg.poll_sec)).await;
    }
}
