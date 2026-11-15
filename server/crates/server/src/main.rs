//! uwuwu-server — внешний слой контура uwuwu (hub#1133):
//! реализации портов (gh-hub, опенкод, PR-шлюз, state, часы) +
//! presentation (HTTP; MCP — срез 2) + композиция.
//! Зависимости: server → usecase → domain, только внутрь.

mod budget;
mod clock;
mod config;
mod fields;
mod gitwiki;
mod http;
mod hub;
mod mcp;
mod mockhub;
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
use uwuwu_usecase::{Clock as _, TickOutcome, TickUseCase};

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
    let plan = Arc::new(cfg.to_tick_plan().map_err(BootError::Runtime)?);
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
    // Хаб: github (прод) | memory (E2E-стенд без GitHub/rate-limit)
    let use_memory_hub = cfg.hub.driver == "memory";
    let (board, field_store): (
        Arc<dyn uwuwu_usecase::TaskBoardRepo>,
        Arc<dyn uwuwu_usecase::IssueFieldStore>,
    ) = if use_memory_hub {
        let mem = Arc::new(mockhub::MemoryHub::from_env());
        tracing::info!(event = "hub.memory", "Хаб: memory (мок-E2E стенд)");
        (mem.clone(), mem)
    } else {
        let gh = Arc::new(hub::GitHubHub::new(
            cfg.hub.repo.clone(),
            secrets.gh_token.clone(),
        ));
        (
            gh.clone() as Arc<dyn uwuwu_usecase::TaskBoardRepo>,
            gh.fields(),
        )
    };
    let raw_sessions = Arc::new(opencode::OpencodeClient::new(
        cfg.executor.url.clone(),
        secrets.opencode_password.clone().unwrap_or_default(),
        secrets.opencode_gate_cookie.clone(),
    ));
    let prs = Arc::new(prgate::GhPrGateway::new(secrets.gh_token.clone()));
    let clock = Arc::new(clock::SystemClock);
    // Глобальный спавн-бюджет: все create_session через один потолок;
    // UWUWU_SPAWNS_MAX_PER_HOUR=0 — kill-switch (деф. 10).
    let spawn_cap: usize = std::env::var("UWUWU_SPAWNS_MAX_PER_HOUR")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(10);
    let sessions: Arc<dyn uwuwu_usecase::AgentSessionGateway> = Arc::new(
        budget::BudgetedSessions::new(raw_sessions, spawn_cap, clock.clone()),
    );
    let state_path = cfg.state_path(&config_path);
    let state = Arc::new(state::JsonStateStore::load(state_path));
    let spawn_model_cfg = cfg
        .executor
        .spawn_model
        .as_ref()
        .map(|m| uwuwu_domain::ModelRef::new(m.provider.clone(), m.id.clone()));
    let usecase = Arc::new(TickUseCase {
        board: board.clone(),
        sessions: sessions.clone(),
        prs: prs.clone(),
        clock: clock.clone(),
        state: state.clone(),
        spawn_model: spawn_model_cfg.clone(),
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
    let task_view = Arc::new(uwuwu_usecase::TaskView::new(
        board.clone(),
        boards_cfg.clone(),
    ));
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
    // Семантика: LanceDB embedded + Ollama-туннель (env UWUWU_EMBED_URL;
    // нет туннеля — поиск честно без индекса)
    let embed_url = std::env::var("UWUWU_EMBED_URL")
        .ok()
        .filter(|v| !v.is_empty());
    let semantic_backend: Option<Arc<dyn uwuwu_usecase::search::SemanticBackend>> = match embed_url
    {
        Some(url) => {
            let embed = Arc::new(semantic::OllamaEmbeddings::new(
                url,
                std::env::var("UWUWU_EMBED_MODEL")
                    .ok()
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| semantic::EMBED_MODEL.to_string()),
            ));
            match semantic_backend::LanceSemanticBackend::open(
                cfg.state_path(&config_path).with_file_name("lancedb"),
                wiki.clone(),
                embed,
            )
            .await
            {
                Ok(b) => Some(Arc::new(b)),
                Err(e) => {
                    tracing::error!(event = "semantic.open_failed", error = %e, "LanceDB open failed");
                    None
                }
            }
        }
        None => None,
    };
    let search_usecase = Arc::new(uwuwu_usecase::search::SearchUseCase {
        semantic: semantic_backend.clone(),
    });

    let tool_hub = Arc::new(tools::ToolHub {
        tasks: task_view,
        docs: Arc::new(uwuwu_usecase::DocsView::new(wiki.clone())),
        person: Arc::new(uwuwu_usecase::PersonView::new(wiki.clone())),
        writer: Arc::new(uwuwu_usecase::TaskWriteUseCase {
            board: board.clone(),
            fields: field_store,
            boards: boards_cfg.clone(),
        }),
        wiki: wiki.clone(),
        sessions: sessions.clone(),
        search: search_usecase.clone(),
        crons: Arc::new(uwuwu_usecase::crons::CronUseCase {
            wiki: wiki.clone(),
            sessions: sessions.clone(),
            clock: clock.clone(),
            spawn_model: spawn_model_cfg.clone(),
            checkout: plan
                .boards
                .first()
                .map(|b| b.checkout.clone())
                .unwrap_or_default(),
        }),
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
    let boards_info: Vec<(String, String, String)> = plan
        .boards
        .iter()
        .map(|b| (b.project.clone(), b.board.clone(), b.checkout.clone()))
        .collect();
    let mut sidecars: Vec<Sidecar> = Vec::new();
    // observe — полностью без мутаций: сайдкары (кроны/триаж/дример/семантика)
    // не исполняются, решения видны только в логе основного тика
    let is_observe = matches!(plan.mode, uwuwu_usecase::Mode::Observe);
    if is_observe {
        tracing::info!(
            event = "sidecars.observe_mode",
            "observe: кроны/триаж/дример/семантика не исполняются"
        );
    }
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
                Box::pin(async move {
                    if is_observe {
                        return Ok(());
                    }
                    cron.tick(&project).await.map(|_| ())
                })
            }),
        });
    }
    {
        // триаж: черновики всех досок (мультиборд); дример — чекаут первой
        let triage = Arc::new(uwuwu_usecase::triage::TriageUseCase {
            board: board.clone(),
            boards: boards_info
                .iter()
                .map(
                    |(project, board_title, checkout)| uwuwu_usecase::triage::TriageBoard {
                        project: project.clone(),
                        board: board_title.clone(),
                        checkout: checkout.clone(),
                    },
                )
                .collect(),
            sessions: sessions.clone(),
            state: state.clone(),
            clock: clock.clone(),
            spawn_model: spawn_model_cfg.clone(),
        });
        let dreamer = Arc::new(uwuwu_usecase::dreamer::DreamerUseCase {
            sessions: sessions.clone(),
            state: state.clone(),
            clock: clock.clone(),
            spawn_model: spawn_model_cfg.clone(),
            checkout: boards_info
                .first()
                .map(|(_, _, checkout)| checkout.clone())
                .unwrap_or_default(),
        });
        sidecars.push(Sidecar {
            name: "triage".into(),
            tick: Box::new(move || {
                let t = triage.clone();
                Box::pin(async move {
                    if is_observe {
                        return Ok(());
                    }
                    t.tick().await.map(|_| ())
                })
            }),
        });
        sidecars.push(Sidecar {
            name: "dreamer".into(),
            tick: Box::new(move || {
                let d = dreamer.clone();
                Box::pin(async move {
                    if is_observe {
                        return Ok(());
                    }
                    d.tick().await.map(|_| ())
                })
            }),
        });
    }
    if let Some(backend) = semantic_backend.clone() {
        let clock2 = clock.clone();

        // backoff: отказ туннеля/Ollama → тишина на час (не долбим каждые 3 мин)
        let backoff_until = Arc::new(std::sync::Mutex::new(0u64));
        sidecars.push(Sidecar {
            name: "semantic-sync".into(),
            tick: Box::new(move || {
                let b = backend.clone();
                let clock = clock2.clone();
                let backoff = backoff_until.clone();
                Box::pin(async move {
                    if is_observe {
                        return Ok(());
                    }
                    let now = clock.now_ms();
                    if now < *backoff.lock().expect("backoff lock") {
                        return Ok(());
                    }
                    if !uwuwu_usecase::search::sync_allowed(clock.utc_minute()) {
                        return Ok(());
                    }
                    // локальная Ollama без квот: бюджет щедрее free-плана
                    match b.sync_index(16).await {
                        Ok(_) => Ok(()),
                        Err(e) => {
                            let until = now + 3_600_000;
                            *backoff.lock().expect("backoff lock") = until;
                            tracing::warn!(event = "semantic.backoff",
                                until_ms = until, error = %e,
                                "Semantic sync: отказ, тишина на час");
                            Ok(())
                        }
                    }
                })
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
