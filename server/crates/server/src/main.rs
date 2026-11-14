//! uwuwu-server — внешний слой контура uwuwu (hub#1133):
//! реализации портов (gh-hub, опенкод, PR-шлюз, state, часы) +
//! presentation (HTTP; MCP — срез 2) + композиция.
//! Зависимости: server → usecase → domain, только внутрь.

mod clock;
mod config;
mod fm;
mod gitwiki;
mod http;
mod hub;
mod mcp;
mod opencode;
mod prgate;
mod semantic;
mod semantic_backend;
mod state;
mod tools;
mod wiki;

use config::{Config, ConfigError, ENV_CONFIG};
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use uwuwu_usecase::{Clock, TickOutcome, TickUseCase};

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
        mode = ?cfg.mode(),
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
        secrets.opencode_gate_cookie.clone(),
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
    // Query-вьюхи для MCP-инструментов (чтения)
    let boards_cfg: Vec<(String, String)> = plan
        .boards
        .iter()
        .map(|b| (b.project.clone(), b.board.clone()))
        .collect();
    let task_view = Arc::new(uwuwu_usecase::TaskView::new(board.clone(), boards_cfg));
    let git_writer = Arc::new(gitwiki::GitWikiWriter::new(
        cfg.hub.repo.clone(),
        "main".to_string(),
        secrets.gh_token.clone(),
    ));
    let wiki = Arc::new(wiki::GitHubWiki::with_writer(
        cfg.hub.repo.clone(),
        "main".to_string(),
        secrets.gh_token.clone(),
        Some(git_writer),
    ));
    // Семантика: плоский индекс + OpenRouter free-эмбеддинги (ключ env,
    // нет — поиск честно без семантики)
    let openrouter_key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|v| !v.is_empty());
    let semantic_backend: Option<Arc<dyn uwuwu_usecase::search::SemanticBackend>> = openrouter_key
        .map(|key| {
            Arc::new(semantic_backend::FlatSemanticBackend {
                wiki: wiki.clone(),
                store: Arc::new(semantic::FileSemanticStore::load(
                    cfg.state_path(&config_path).with_file_name("semantic.json"),
                )),
                embeddings: Arc::new(semantic::OpenRouterEmbeddings::new(key)),
            }) as Arc<dyn uwuwu_usecase::search::SemanticBackend>
        });
    let search_usecase = Arc::new(uwuwu_usecase::search::SearchUseCase {
        wiki: wiki.clone(),
        semantic: semantic_backend.clone(),
        openrouter_key: None,
    });

    let tool_hub = Arc::new(tools::ToolHub {
        tasks: task_view,
        docs: Arc::new(uwuwu_usecase::DocsView::new(wiki.clone())),
        person: Arc::new(uwuwu_usecase::PersonView::new(wiki.clone())),
        writer: Arc::new(uwuwu_usecase::TaskWriteUseCase {
            board: board.clone(),
            board_name: Arc::new(
                plan.boards
                    .first()
                    .map(|b| b.board.clone())
                    .unwrap_or_default(),
            ),
        }),
        wiki: wiki.clone(),
        sessions: sessions.clone(),
        search: search_usecase.clone(),
    });

    let http_state = Arc::new(http::AppState {
        bearer: Arc::new(secrets.bearer.clone()),
        mode: mode_label,
        tick: shared.clone(),
    });
    let mcp_state = Arc::new(mcp::McpState {
        bearer: Arc::new(secrets.bearer.clone()),
        tools: tool_hub,
    });
    let app = http::router(http_state).merge(mcp::router(mcp_state));

    // Сателлиты конвейера: кроны (вики-носитель), триаж черновиков, дример
    let spawn_model_cfg = cfg
        .executor
        .spawn_model
        .as_ref()
        .map(|m| (m.provider.clone(), m.id.clone()));
    let boards_info: Vec<(String, String, String)> = plan
        .boards
        .iter()
        .map(|b| (b.project.clone(), b.board.clone(), b.checkout.clone()))
        .collect();
    let mut sidecars: Vec<Sidecar> = Vec::new();
    for (project, _, checkout) in boards_info.iter() {
        let cron = Arc::new(uwuwu_usecase::crons::CronUseCase {
            wiki: wiki.clone(),
            sessions: sessions.clone(),
            clock: clock.clone(),
            spawn_model: spawn_model_cfg.clone(),
            checkout: checkout.clone(),
        });
        let project = project.clone();
        sidecars.push(Sidecar {
            name: format!("cron:{project}"),
            tick: Box::new(move || {
                let cron = cron.clone();
                let project = project.clone();
                Box::pin(async move { cron.tick(&project).await.map(|_| ()) })
            }),
        });
    }
    if let Some((_, board_title, checkout)) = boards_info.first().cloned() {
        let triage = Arc::new(uwuwu_usecase::triage::TriageUseCase {
            board: board.clone(),
            board_name: Arc::new(board_title),
            sessions: sessions.clone(),
            spawn_model: spawn_model_cfg.clone(),
            checkout: checkout.clone(),
        });
        let dreamer = Arc::new(uwuwu_usecase::dreamer::DreamerUseCase {
            sessions: sessions.clone(),
            state: state.clone(),
            clock: clock.clone(),
            spawn_model: spawn_model_cfg.clone(),
            checkout,
        });
        sidecars.push(Sidecar {
            name: "triage".into(),
            tick: Box::new(move || {
                let t = triage.clone();
                Box::pin(async move { t.tick().await.map(|_| ()) })
            }),
        });
        if let Some(backend) = semantic_backend.clone() {
            let clock2 = clock.clone();
            sidecars.push(Sidecar {
                name: "semantic-sync".into(),
                tick: Box::new(move || {
                    let b = backend.clone();
                    let clock = clock2.clone();
                    Box::pin(async move {
                        if !uwuwu_usecase::search::sync_allowed(clock.utc_minute()) {
                            return Ok(());
                        }
                        // бюджет дока за тик: 8 (≈ 1-2 запроса эмбеддингов)
                        b.sync_index(8).await.map(|_| ())
                    })
                }),
            });
        }
        sidecars.push(Sidecar {
            name: "dreamer".into(),
            tick: Box::new(move || {
                let d = dreamer.clone();
                Box::pin(async move { d.tick().await.map(|_| ()) })
            }),
        });
    }

    let http_task = tokio::spawn(serve(cfg.port(), app));
    let tick_task = tokio::spawn(tick_loop(
        cfg.poll_sec,
        plan,
        usecase,
        shared,
        Arc::new(sidecars),
    ));

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

/// Сателлит тика: имя + замыкание-проход (мягкая изоляция отказов).
struct Sidecar {
    name: String,
    #[allow(clippy::type_complexity)]
    tick: Box<
        dyn Fn() -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = Result<(), uwuwu_usecase::UsecaseError>>
                        + Send,
                >,
            > + Send
            + Sync,
    >,
}

async fn sidecar_ticks(sidecars: &[Sidecar]) -> Result<(), uwuwu_usecase::UsecaseError> {
    for s in sidecars {
        if let Err(e) = (s.tick)().await {
            tracing::error!(event = "sidecar.tick_failed", sidecar = %s.name, error = %e, "Sidecar failed");
        }
    }
    Ok(())
}

async fn tick_loop(
    poll_sec: u64,
    plan: Arc<uwuwu_usecase::TickPlan>,
    usecase: Arc<TickUseCase>,
    shared: Arc<std::sync::RwLock<TickOutcome>>,
    sidecars: Arc<Vec<Sidecar>>,
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
        if let Err(e) = sidecar_ticks(&sidecars).await {
            tracing::error!(event = "sidecar.failed", error = %e, "Sidecar tick failed");
        }
        tokio::time::sleep(std::time::Duration::from_secs(poll_sec)).await;
    }
}
