//! uwuwu-server — внешний слой контура uwuwu (hub#1133):
//! реализации портов (gh-hub, опенкод, PR-шлюз, state, часы) +
//! presentation (HTTP; MCP — срез 2) + композиция.
//! Зависимости: server → usecase → domain, только внутрь.

mod clock;
mod config;
mod fm;
mod http;
mod hub;
mod opencode;
mod prgate;
mod state;

use config::{Config, ConfigError, ENV_CONFIG};
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use uwuwu_usecase::{TickOutcome, TickUseCase};

#[derive(Debug, thiserror::Error)]
enum BootError {
    #[error("конфиг: {0}")]
    Config(#[from] ConfigError),
    #[error("runtime: {0}")]
    Runtime(String),
}

fn main() -> Result<(), BootError> {
    // JSON-логи (Railway ingest), уровень — env RUST_LOG (дефолт info)
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .init();
    let config_path = std::env::var(ENV_CONFIG)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config.toml"));
    let cfg = Config::load(&config_path)?;
    let secrets = cfg.secrets()?;
    let plan = Arc::new(cfg.to_tick_plan());
    tracing::info!(
        event = "server.started",
        mode = ?plan.mode,
        executor = %cfg.executor.name,
        projects = plan.boards.len(),
        poll_sec = cfg.poll_sec,
        config = %config_path.display(),
        "Server started"
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| BootError::Runtime(e.to_string()))?;
    rt.block_on(run(cfg, secrets, plan, config_path))
        .map_err(BootError::Runtime)
}

async fn run(
    cfg: Config,
    secrets: config::Secrets,
    plan: Arc<uwuwu_usecase::TickPlan>,
    config_path: PathBuf,
) -> Result<(), String> {
    // Композиция: реализации портов → оркестратор
    let board = Arc::new(hub::GitHubHub::new(
        cfg.hub.repo.clone(),
        secrets.gh_token.clone(),
    ));
    let sessions = Arc::new(opencode::OpencodeClient::new(
        cfg.executor.url.clone(),
        secrets.opencode_password.clone().unwrap_or_default(),
    ));
    let prs = Arc::new(prgate::GhPrGateway::new(secrets.gh_token.clone()));
    let clock = Arc::new(clock::SystemClock);
    let state_path = cfg.state_path(&config_path);
    let state = Arc::new(state::JsonStateStore::load(state_path));
    let usecase = Arc::new(TickUseCase {
        board: board.clone(),
        sessions: sessions.clone(),
        prs: prs.clone(),
        clock: clock.clone(),
        state: state.clone(),
        spawn_model: cfg
            .executor
            .spawn_model
            .as_ref()
            .map(|m| (m.provider.clone(), m.id.clone())),
    });

    let shared: Arc<std::sync::RwLock<TickOutcome>> = Arc::default();
    let mode_label: &'static str = match plan.mode {
        uwuwu_usecase::Mode::Observe => "observe",
        uwuwu_usecase::Mode::Live => "live",
    };
    let http_state = Arc::new(http::AppState {
        bearer: Arc::new(secrets.bearer.clone()),
        mode: mode_label,
        tick: shared.clone(),
    });

    let http_task = tokio::spawn(serve(cfg.port(), http::router(http_state)));
    let tick_task = tokio::spawn(tick_loop(cfg.poll_sec, plan, usecase, shared));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm() => {},
    }
    tracing::info!(event = "server.stopping", "Server stopping");
    http_task.abort();
    tick_task.abort();
    Ok(())
}

/// SIGTERM (Railway stop): graceful без паники.
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

async fn tick_loop(
    poll_sec: u64,
    plan: Arc<uwuwu_usecase::TickPlan>,
    usecase: Arc<TickUseCase>,
    shared: Arc<std::sync::RwLock<TickOutcome>>,
) {
    loop {
        let started = std::time::Instant::now();
        match usecase.run(&plan).await {
            Ok(outcome) => {
                *shared.write().expect("tick lock") = outcome;
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
        tokio::time::sleep(std::time::Duration::from_secs(poll_sec)).await;
    }
}
