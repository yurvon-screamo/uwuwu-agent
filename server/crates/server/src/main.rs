//! uwuwu-server — внешний слой контура uwuwu (hub#1133):
//! реализации портов (gh-hub, опенкод, PR-шлюз, state, часы) +
//! presentation (HTTP; MCP) + композиция. Структура: api/ executor/
//! github/ search/ wiki/ + main/config/state/clock/mockhub.
//! Зависимости: server → usecase → domain, только внутрь.

mod api;
mod bots;
mod clean_journal;
mod clock;
mod config;
mod diag;
mod executor;
#[cfg(test)]
mod executor_signal_tests;
mod gherr;
#[cfg(test)]
mod gherr_tests;
mod gitea;
#[cfg(test)]
mod gitea_tests;
mod github;
mod mockhub;
mod prgate;
mod s3;
mod search;
mod state;
mod tg;
mod wiki;

use config::{Config, ConfigError, ENV_CONFIG};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use uwuwu_usecase::{
    Clock as _, ExecutorLoudness, IncidentLoudness, TickOutcome, TickUseCase,
    EXECUTOR_UNAVAILABLE_COOLDOWN_MS, EXECUTOR_UNAVAILABLE_THRESHOLD,
    EXECUTOR_UNAVAILABLE_WINDOW_MS, STALL_INCIDENT_COOLDOWN_MS, STALL_INCIDENT_THRESHOLD,
    STALL_INCIDENT_WINDOW_MS,
};

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
    let mut cfg = Config::load(&config_path)?;
    // env-оверрайды прод-окружения (Railway): executor через внутренний
    // rathole-релей (relay.railway.internal:2335) — без публичного домена,
    // Caddy-прокси и внешних куки (снимает класс «гейт недоступен»).
    if let Ok(url) = std::env::var("UWUWU_EXECUTOR_URL") {
        if !url.is_empty() {
            cfg.executor.url = url;
        }
    }
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
    // Срез A #1294: THP-контекст процесса — вердикт THP-пробы прямым
    // наблюдением (Railway-метрика = cgroup, THP раздувает cgroup-учёт).
    let thp = diag::snapshot();
    tracing::info!(
        event = "server.thp",
        thp = thp.thp.as_deref().unwrap_or("n/a"),
        anon_hugepages_kb = thp
            .anon_hugepages_kb
            .map(|v| v.to_string())
            .as_deref()
            .unwrap_or("n/a"),
        "THP snapshot"
    );
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| BootError::Runtime(e.to_string()))?;
    rt.block_on(run(cfg, secrets, plan, config_path))
        .map_err(BootError::Runtime)
}

/// Композиция исполнителей и роутера «проект → executor» (#1203):
/// дефолтный [executor]/UWUWU_EXECUTOR_URL + клиенты именованных
/// [[executors]]; basic-пароль и гейт-кука общие (redmi использует тот же
/// UWUWU_OPENCODE_PASSWORD). Спавн-бюджет — один на все per-project
/// хэндлы; UWUWU_SPAWNS_MAX_PER_HOUR=0 — kill-switch (деф. 10).
fn build_sessions(
    cfg: &Config,
    secrets: &config::Secrets,
    clock: Arc<dyn uwuwu_usecase::Clock>,
) -> Result<
    (
        Arc<dyn uwuwu_usecase::AgentSessionGateway>,
        Arc<uwuwu_usecase::SessionRouter>,
    ),
    String,
> {
    let raw_default: Arc<dyn uwuwu_usecase::AgentSessionGateway> =
        Arc::new(executor::OpencodeClient::new(
            cfg.executor.url.clone(),
            secrets.opencode_password.clone().unwrap_or_default(),
            secrets.opencode_gate_cookie.clone(),
        ));
    let spawn_cap: usize = std::env::var("UWUWU_SPAWNS_MAX_PER_HOUR")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(10);
    let shared_budget = Arc::new(std::sync::Mutex::new(uwuwu_domain::SpawnBudget::new(
        spawn_cap,
    )));
    let budgeted_default: Arc<dyn uwuwu_usecase::AgentSessionGateway> =
        Arc::new(executor::BudgetedSessions::with_shared(
            raw_default.clone(),
            shared_budget.clone(),
            clock.clone(),
        ));
    let mut named_gateways: HashMap<String, Arc<dyn uwuwu_usecase::AgentSessionGateway>> =
        HashMap::new();
    for e in &cfg.executors {
        let raw: Arc<dyn uwuwu_usecase::AgentSessionGateway> =
            Arc::new(executor::OpencodeClient::new(
                e.url.clone(),
                secrets.opencode_password.clone().unwrap_or_default(),
                secrets.opencode_gate_cookie.clone(),
            ));
        named_gateways.insert(
            e.name.clone(),
            Arc::new(executor::BudgetedSessions::with_shared(
                raw,
                shared_budget.clone(),
                clock.clone(),
            )),
        );
    }
    let mut by_project: HashMap<String, Arc<dyn uwuwu_usecase::AgentSessionGateway>> =
        HashMap::new();
    for p in &cfg.project {
        // именованный маршрут — только явная ссылка на НЕ дефолтного
        // исполнителя (None/дефолт/пусто → бюджетный дефолт-хэндл)
        let handle = match p.executor.as_deref().map(str::trim) {
            Some(target) if !target.is_empty() && target != cfg.executor.name => {
                let gateway = named_gateways.get(target).ok_or_else(|| {
                    format!(
                        "конфиг: проект «{}»: executor «{}» не собран (валидация пропустила?)",
                        p.name, target
                    )
                })?;
                gateway.clone()
            }
            _ => budgeted_default.clone(),
        };
        by_project.insert(p.name.trim().to_string(), handle);
    }
    let named: Vec<(String, Arc<dyn uwuwu_usecase::AgentSessionGateway>)> = cfg
        .executors
        .iter()
        .map(|e| match named_gateways.get(&e.name) {
            Some(gateway) => Ok((e.name.clone(), gateway.clone())),
            None => Err(format!(
                "конфиг: executor «{}» не собран (валидация пропустила?)",
                e.name
            )),
        })
        .collect::<Result<Vec<_>, String>>()?;
    let router = Arc::new(uwuwu_usecase::SessionRouter::new(
        &cfg.executor.name,
        budgeted_default,
        named,
        by_project,
    ));
    Ok((raw_default, router))
}

/// Blob-канал (#1292): приватный бакет Tigris через env UWUWU_S3_*.
/// Частично заданы — тулы дают громкий отказ с перечнем недостающих
/// (ADR-010); кривые значения — громкая ошибка старта через Result
/// (конвенция бута main → BootError, без panic в бут-пути).
fn build_blob_usecase() -> Result<Arc<uwuwu_usecase::BlobUseCase>, String> {
    let missing: Vec<String> = s3::S3_ENV_NAMES
        .iter()
        .filter(|name| config::non_empty_env(name).is_none())
        .map(|name| (*name).to_string())
        .collect();
    if !missing.is_empty() {
        return Ok(Arc::new(uwuwu_usecase::BlobUseCase::missing(missing)));
    }
    let config = s3::TigrisConfig {
        // Все пять проверены выше — env не меняется в рантайме.
        endpoint: config::non_empty_env("UWUWU_S3_ENDPOINT").unwrap_or_default(),
        region: config::non_empty_env("UWUWU_S3_REGION").unwrap_or_default(),
        bucket: config::non_empty_env("UWUWU_S3_BUCKET").unwrap_or_default(),
        access_key: config::non_empty_env("UWUWU_S3_ACCESS_KEY").unwrap_or_default(),
        secret: config::non_empty_env("UWUWU_S3_SECRET").unwrap_or_default(),
    };
    let store = s3::TigrisBlobStore::new(config)?;
    Ok(Arc::new(uwuwu_usecase::BlobUseCase::ready(Arc::new(store))))
}

async fn run(
    cfg: Config,
    secrets: config::Secrets,
    plan: Arc<uwuwu_usecase::TickPlan>,
    config_path: PathBuf,
) -> Result<(), String> {
    // Композиция: реализации портов → оркестратор
    // Хаб: github (прод) | memory (E2E-стенд без GitHub/rate-limit);
    // memory-стенд держит и вики (seed UWUWU_MOCK_DOCS) — полный e2e без сети
    let use_memory_hub = cfg.hub.driver == "memory";
    let memory_hub: Option<Arc<mockhub::MemoryHub>> = if use_memory_hub {
        Some(Arc::new(mockhub::MemoryHub::from_env()))
    } else {
        None
    };
    let mut github_hub: Option<Arc<github::GitHubHub>> = None;
    let (board, field_store): (
        Arc<dyn uwuwu_usecase::TaskBoardRepo>,
        Arc<dyn uwuwu_usecase::IssueFieldStore>,
    ) = match &memory_hub {
        Some(mem) => {
            tracing::info!(event = "hub.memory", "Хаб: memory (мок-E2E стенд)");
            (
                mem.clone() as Arc<dyn uwuwu_usecase::TaskBoardRepo>,
                mem.clone() as Arc<dyn uwuwu_usecase::IssueFieldStore>,
            )
        }
        None => {
            let gh = Arc::new(github::GitHubHub::new(
                cfg.hub.repo.clone(),
                secrets.gh_token.clone(),
            ));
            github_hub = Some(gh.clone());
            (
                gh.clone() as Arc<dyn uwuwu_usecase::TaskBoardRepo>,
                gh.fields() as Arc<dyn uwuwu_usecase::IssueFieldStore>,
            )
        }
    };
    // Исполнители и роутер «проект → executor» (#1203); боты — raw-дефолт
    let clock = Arc::new(clock::SystemClock);
    let (raw_default, sessions) = build_sessions(&cfg, &secrets, clock.clone())?;
    // PR-гейт: диспетчер по хосту PR-URL — GitHub + Gitea (#1269)
    let prs = Arc::new(prgate::RoutedPrGateway::new(
        github::GhPrGateway::new(secrets.gh_token.clone()),
        gitea::GiteaPrGateway::new(secrets.gitea_tokens.clone()),
    ));
    let state_path = cfg.state_path(&config_path);
    let state = Arc::new(state::JsonStateStore::load(state_path, clock.clone()));
    // undo-журнал чистки (#1158): файл рядом со state.json на volume
    let clean_journals = Arc::new(clean_journal::JsonCleanJournals::load(
        cfg.state_path(&config_path).with_file_name("undo.json"),
    ));
    let spawn_models = Arc::new(match cfg.executor.models.as_ref() {
        Some(raw) => {
            let mut map = std::collections::HashMap::new();
            for (key, model) in raw {
                map.insert(
                    key.clone(),
                    uwuwu_domain::SpawnModel::parse(model)
                        .map_err(|e| format!("[executor.models] {key}: {e}"))?,
                );
            }
            uwuwu_usecase::SpawnModels::new(map)
        }
        None => uwuwu_usecase::SpawnModels::canonical(),
    });
    let usecase = Arc::new(TickUseCase {
        board: board.clone(),
        sessions: sessions.clone(),
        prs: prs.clone(),
        clock: clock.clone(),
        state: state.clone(),
        models: spawn_models.clone(),
        boot_ms: clock.now_ms(),
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
    let wiki: Arc<dyn uwuwu_usecase::WikiRepo> = match &memory_hub {
        Some(mem) => mem.clone(),
        None => Arc::new(wiki::GitHubWiki::with_writer(
            cfg.hub.repo.clone(),
            "main".to_string(),
            secrets.gh_token.clone(),
            Some(Arc::new(wiki::GitWikiWriter::new(
                cfg.hub.repo.clone(),
                "main".to_string(),
                secrets.gh_token.clone(),
            ))),
        )),
    };
    // Ассеты ботов (#1171): прод — коммит в hub-репо (ff-only), e2e/
    // memory — приёмник стенда (полный media-флоу без GitHub).
    let asset_store: Arc<dyn uwuwu_usecase::bots::BotAssetStore> = match &memory_hub {
        Some(_) => Arc::new(bots::MemoryAssetStore::default()),
        None => Arc::new(bots::GitAssetStore::new(
            Arc::new(wiki::GitWikiWriter::new(
                cfg.hub.repo.clone(),
                "main".to_string(),
                secrets.gh_token.clone(),
            )),
            cfg.hub.repo.clone(),
            "main".to_string(),
        )),
    };
    // Семантика: LanceDB embedded + Gemini Embedding 2 (#1363; env GEMINI_API_KEY;
    // нет ключа — поиск честно без семантического индекса)
    let gemini_key = std::env::var("GEMINI_API_KEY")
        .ok()
        .filter(|v| !v.is_empty());
    let semantic_backend: Option<Arc<dyn uwuwu_usecase::SemanticBackend>> = match gemini_key {
        Some(key) => {
            let embed: Arc<dyn search::Embedder> = Arc::new(search::GeminiEmbeddings::new(
                key,
                std::env::var("UWUWU_EMBED_MODEL")
                    .ok()
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| search::EMBED_MODEL.to_string()),
            ));
            // github-хаб индексирует и задачи (юзер-стори «по вики/задачам»)
            let issues: Option<Arc<dyn uwuwu_usecase::IssueIndexSource>> = github_hub
                .clone()
                .map(|gh| gh as Arc<dyn uwuwu_usecase::IssueIndexSource>);
            match search::LanceSemanticBackend::open(
                cfg.state_path(&config_path).with_file_name("lancedb"),
                wiki.clone(),
                issues,
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
    let search_usecase = Arc::new(uwuwu_usecase::SearchUseCase {
        semantic: semantic_backend.clone(),
    });

    // Blob-канал (#1292): сборка из env — см. build_blob_usecase.
    let blobs_usecase = build_blob_usecase()?;

    let tool_hub = Arc::new(api::ToolHub {
        blobs: blobs_usecase,
        tasks: task_view,
        docs: Arc::new(uwuwu_usecase::DocsView::new(wiki.clone())),
        person: Arc::new(uwuwu_usecase::PersonView::new(wiki.clone())),
        writer: Arc::new(uwuwu_usecase::TaskWriteUseCase {
            board: board.clone(),
            fields: field_store,
            boards: boards_cfg.clone(),
            clock: clock.clone(),
            state: state.clone(),
        }),
        clean: Arc::new(uwuwu_usecase::TaskCleanUseCase {
            // driver-независимо: gh-хаб и memory-хаб несут CleanBoardRepo
            clean: match &memory_hub {
                Some(mem) => mem.clone() as Arc<dyn uwuwu_usecase::CleanBoardRepo>,
                None => github_hub.clone().expect("gh-хаб собран при driver=github")
                    as Arc<dyn uwuwu_usecase::CleanBoardRepo>,
            },
            journals: clean_journals,
            boards: boards_cfg.clone(),
            clock: clock.clone(),
        }),
        wiki: wiki.clone(),
        sessions: sessions.clone(),
        search: search_usecase.clone(),
        crons: Arc::new(uwuwu_usecase::CronUseCase {
            wiki: wiki.clone(),
            sessions: sessions.clone(),
            clock: clock.clone(),
            models: spawn_models.clone(),
            projects: plan
                .boards
                .iter()
                .map(|b| (b.project.clone(), b.checkout.clone()))
                .collect(),
            cup: plan.policy.cup,
        }),
    });

    let http_state = Arc::new(api::AppState {
        bearer: Arc::new(secrets.bearer.clone()),
        mode: mode_label,
        tick: shared.clone(),
    });
    let mcp_state = Arc::new(api::McpState {
        bearer: Arc::new(secrets.bearer.clone()),
        tools: tool_hub,
    });
    let app = api::http_router(http_state).merge(api::mcp_router(mcp_state));

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
    {
        // Кроны: один глобальный sidecar — раскладка /cron (#1214):
        // мастера cron/<имя>/README.md несут fm project, связка с
        // [[project]] (роутинг executor + checkout) — карта из конфига
        let projects_map: std::collections::HashMap<String, String> = boards_info
            .iter()
            .map(|(project, _, checkout)| (project.clone(), checkout.clone()))
            .collect();
        let cron = Arc::new(uwuwu_usecase::CronUseCase {
            wiki: wiki.clone(),
            sessions: sessions.clone(),
            clock: clock.clone(),
            models: spawn_models.clone(),
            projects: projects_map,
            cup: plan.policy.cup,
        });

        sidecars.push(Sidecar {
            name: "cron".to_string(),
            tick: Box::new(move || {
                let cron = cron.clone();
                Box::pin(async move {
                    if is_observe {
                        return Ok(());
                    }
                    cron.tick().await.map(|_| ())
                })
            }),
        });
    }
    {
        // аналитика (#1135, замена триажа): волны со всех досок; дример — чекаут первой
        let analyst_boards: Vec<uwuwu_usecase::AnalystBoard> = boards_info
            .iter()
            .map(
                |(project, board_title, checkout)| uwuwu_usecase::AnalystBoard {
                    project: project.clone(),
                    board: board_title.clone(),
                    checkout: checkout.clone(),
                },
            )
            .collect();
        let analyst = Arc::new(uwuwu_usecase::AnalystUseCase {
            board: board.clone(),
            boards: analyst_boards.clone(),
            sessions: sessions.clone(),
            state: state.clone(),
            clock: clock.clone(),
            models: spawn_models.clone(),
            cup: plan.policy.cup,
        });
        let analyst_review = Arc::new(uwuwu_usecase::AnalystReviewUseCase {
            board: board.clone(),
            boards: analyst_boards,
            sessions: sessions.clone(),
            state: state.clone(),
            clock: clock.clone(),
            models: spawn_models.clone(),
            cup: plan.policy.cup,
        });
        let dreamer = Arc::new(uwuwu_usecase::DreamerUseCase {
            sessions: sessions.clone(),
            state: state.clone(),
            clock: clock.clone(),
            models: spawn_models.clone(),
            checkout: boards_info
                .first()
                .map(|(_, _, checkout)| checkout.clone())
                .unwrap_or_default(),
            cup: plan.policy.cup,
        });
        sidecars.push(Sidecar {
            name: "analys".into(),
            tick: Box::new(move || {
                let a = analyst.clone();
                Box::pin(async move {
                    if is_observe {
                        return Ok(());
                    }
                    a.tick().await.map(|_| ())
                })
            }),
        });
        sidecars.push(Sidecar {
            name: "analys-review".into(),
            tick: Box::new(move || {
                let r = analyst_review.clone();
                Box::pin(async move {
                    if is_observe {
                        return Ok(());
                    }
                    r.tick().await.map(|_| ())
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
    {
        // боты (#1150): тик супервизора быстрый (wiki-скан) — обычный
        // сайдкар; long-poll живёт в per-bot задачах. Сессии ботов —
        // через raw-гейтвей дефолтного исполнителя (вне спавн-бюджета/CUP;
        // маршрутизация ботов по executor — вне скоупа #1203)
        let bot_sessions: Arc<dyn uwuwu_usecase::AgentSessionGateway> = raw_default.clone();
        let mut bot_supervisor = bots::BotSupervisor::new(
            wiki.clone(),
            bot_sessions,
            state.clone(),
            asset_store.clone(),
            state.clone() as Arc<dyn uwuwu_usecase::TickStateStore>,
            clock.clone(),
            Arc::new(|token| {
                Arc::new(tg::TelegramClient::new(token.to_string()))
                    as Arc<dyn uwuwu_usecase::bots::TelegramGateway>
            }),
        );
        // Автокомпакт вечных сессий (#1221): секция [compact] конфига;
        // отсутствует — off (safe default). Пороги — ms-арифметика поверх
        // минутных полей секции.
        bot_supervisor.compact = cfg
            .compact
            .clone()
            .map(|c| uwuwu_usecase::bots::CompactPolicy {
                idle_ms: c.idle_min * 60_000,
                min_context_pct: c.min_context_pct,
                cooldown_ms: c.cooldown_min * 60_000,
            });
        let bot_supervisor = Arc::new(bot_supervisor);
        sidecars.push(Sidecar {
            name: "bots".into(),
            tick: Box::new(move || {
                let s = bot_supervisor.clone();
                Box::pin(async move {
                    if is_observe {
                        return Ok(());
                    }
                    s.tick().await
                })
            }),
        });
    }

    {
        // клин холодного ящика (#1293): терминальные строки старше
        // sweep_days суток уходят в архив Projects v2 — снапшот несёт
        // активный контур. sweep_days = 0 — kill-switch: sidecar вообще
        // не создаётся (дефолт serde 0 = выкл; боевой конфиг — 7).
        // Обратите внимание: observe-гейт здесь **внутренний**
        // (sweep_board логирует кандидатов и не мутирует), не общий
        // no-op `if is_observe { return }` — observe-фаза обязана
        // показывать кандидатов
        if cfg.sweep_days > 0 {
            let sweep_board_repo = board.clone();
            let sweep_boards = plan.boards.clone();
            let sweep_clock = clock.clone();
            sidecars.push(Sidecar {
                name: "sweeper".into(),
                tick: Box::new(move || {
                    let board = sweep_board_repo.clone();
                    let boards = sweep_boards.clone();
                    let clock = sweep_clock.clone();
                    Box::pin(async move {
                        let now_ms = clock.now_ms();
                        // Дебаг-гейт одной строки (как у тика): UWUWU_ONLY_TASK
                        // — клин действует только по ней; живая проба против
                        // реального репо не запускает сотенную первую волну
                        let only = std::env::var("UWUWU_ONLY_TASK")
                            .ok()
                            .and_then(|v| v.trim().parse::<u64>().ok());
                        for b in &boards {
                            let mut tasks = board.read_board(&b.board).await?;
                            if let Some(n) = only {
                                tasks.retain(|t| t.number() == n);
                            }
                            uwuwu_usecase::sweeper::sweep_board(
                                board.as_ref(),
                                &b.board,
                                &tasks,
                                uwuwu_usecase::sweeper::SweepPolicy {
                                    now_ms,
                                    age_days: cfg.sweep_days,
                                    observe: is_observe,
                                },
                            )
                            .await?;
                        }
                        Ok(())
                    })
                }),
            });
        }
    }

    if let Some(backend) = semantic_backend.clone() {
        let clock2 = clock.clone();

        // Backoff дифференцированный (#1363): 429 — ожидаемый сигнал лимитов
        // Gemini free tier, бэкофф в минутах из RetryInfo (кап 15 мин);
        // транспортный/5xx отказ — час, как прежде. Часовой бэкофф на 429
        // запрещён — это тишина вместо работы.
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
                    if !uwuwu_usecase::sync_allowed(clock.utc_minute()) {
                        return Ok(());
                    }
                    // бюджет = производная замеров спайка #1363 (формула в теле
                    // задачи): ток-бюджет тика ≤ TPM_факт × pacing, гард 120с сайдкара
                    match b.sync_index(search::SEMANTIC_SYNC_BUDGET).await {
                        Ok(_) => Ok(()),
                        Err(uwuwu_usecase::UsecaseError::RateLimited {
                            retry_after_secs,
                            ..
                        }) => {
                            let secs =
                                retry_after_secs.unwrap_or(300).min(search::RATE_LIMIT_CAP_SECS);
                            let until = now + secs * 1000;
                            *backoff.lock().expect("backoff lock") = until;
                            tracing::warn!(event = "semantic.backoff_rate_limited",
                                until_ms = until, retry_after_secs = secs,
                                "Semantic sync: 429, пауза в минутах");
                            Ok(())
                        }
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
    let notify = cfg
        .tg_notify()
        // run() возвращает String; префикс «конфиг:» сохраняем из Display
        // ConfigError (BootError-конверсия — вне скоупа #1281)
        .map_err(|e| format!("конфиг: {e}"))?
        .map(|(token, chat)| {
            (
                Arc::new(tg::TelegramClient::new(token))
                    as Arc<dyn uwuwu_usecase::bots::TelegramGateway>,
                chat,
            )
        });
    let tick_task = tokio::spawn(tick_loop(TickLoopArgs {
        poll_sec: cfg.poll_sec,
        plan,
        usecase,
        shared,
        sidecars: Arc::new(sidecars),
        notify,
        clock,
    }));

    // Срез B' #1294: разводка «куча vs cgroup-учёт» — первая точка сразу
    // (бут), дальше каждые 30 мин (page cache от mmap LanceDB прогревается
    // работой; Railway-метрика = cgroup current, куча = RSS).
    let memory_task = tokio::spawn(memory_loop());

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm() => {},
    }
    tracing::info!(event = "server.stopping", "Server stopping");
    http_task.abort();
    tick_task.abort();
    memory_task.abort();
    Ok(())
}

/// Периодический снимок памяти (срез B' #1294): событие server.memory —
/// RSS процесса против cgroup-счётчика (Railway-метрика = cgroup current;
/// page cache от mmap LanceDB прогревается работой, не куча).
async fn memory_loop() {
    let mut iv = tokio::time::interval(std::time::Duration::from_secs(1800));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        iv.tick().await;
        let m = diag::memory_snapshot();
        tracing::info!(
            event = "server.memory",
            rss_kb = m.rss_kb.map(|v| v.to_string()).as_deref().unwrap_or("n/a"),
            cgroup_current_kb = m
                .cgroup_current_kb
                .map(|v| v.to_string())
                .as_deref()
                .unwrap_or("n/a"),
            page_cache_kb = m
                .cgroup_file_kb
                .map(|v| v.to_string())
                .as_deref()
                .unwrap_or("n/a"),
            cgroup_anon_kb = m
                .cgroup_anon_kb
                .map(|v| v.to_string())
                .as_deref()
                .unwrap_or("n/a"),
            "Memory snapshot"
        );
    }
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
        // таймаут: один зависший сайдкар не хоронит очередь остальных
        // (инцидент 16.11: bot-события пропали при живом основном тике)
        match tokio::time::timeout(std::time::Duration::from_secs(120), (s.tick)()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::error!(
                event = "sidecar.tick_failed", sidecar = %s.name, error = %e, "Sidecar failed"),
            Err(_) => tracing::error!(
                event = "sidecar.timeout", sidecar = %s.name,
                "Sidecar tick завис (>120с) — пропущен, очередь живёт"),
        }
    }
    Ok(())
}

/// Аргументы тик-лупа: группировка (qlty function-parameters #1281).
struct TickLoopArgs {
    poll_sec: u64,
    plan: Arc<uwuwu_usecase::TickPlan>,
    usecase: Arc<TickUseCase>,
    shared: Arc<std::sync::RwLock<TickOutcome>>,
    sidecars: Arc<Vec<Sidecar>>,
    /// ТГ-сигнализация инцидентов сталл-детекта (#1281); None — off.
    notify: Option<(Arc<dyn uwuwu_usecase::bots::TelegramGateway>, i64)>,
    clock: Arc<dyn uwuwu_usecase::Clock>,
}

/// Один инцидент-сигнал (#1281): регистрирует эскалацию в скользящем окне;
/// при превышении порога и истёкшем cooldown — один ТГ-сигнал владельцу
/// (канон «строка сбоя + ТГ» UC-02). [notify] не настроен — громкий лог.
async fn incident_signal(
    notify: &Option<(Arc<dyn uwuwu_usecase::bots::TelegramGateway>, i64)>,
    loudness: &mut IncidentLoudness,
    now_ms: u64,
) {
    if !loudness.register(now_ms) {
        return;
    }
    let text = format!(
        "⚠︎ uwuwu: инцидент сталл-детекта — {} эскалаций за скользящее окно \
         ({} мин): массовая парковка stage-сессий в Needs attention. \
         Разбор — на доске.",
        loudness.escalations_in_window(),
        STALL_INCIDENT_WINDOW_MS / 60_000,
    );
    match notify {
        Some((tg, chat)) => match tg.send_message(*chat, &text).await {
            Ok(()) => tracing::warn!(
                event = "stall.incident_signal",
                escalations = loudness.escalations_in_window(),
                "Инцидент-сигнал владельцу отправлен"
            ),
            Err(e) => tracing::error!(
                event = "stall.incident_signal_failed",
                error = %e,
                "Инцидент-сигнал не отправлен"
            ),
        },
        None => tracing::warn!(
            event = "stall.incident_unnotified",
            escalations = loudness.escalations_in_window(),
            "Инцидент сталл-детекта: [notify] не настроен — сигнала нет"
        ),
    }
}

/// Транспортная деградация executor'а (#1312): отказы union-скана за
/// скользящее окно — один ТГ-сигнал владельцу per-executor через тот же
/// инцидент-контур #1281. [notify] не настроен — громкий лог.
async fn executor_unavailable_signal(
    notify: &Option<(Arc<dyn uwuwu_usecase::bots::TelegramGateway>, i64)>,
    incidents: &mut ExecutorLoudness,
    executor: &str,
    now_ms: u64,
) {
    let Some(incident) = incidents.register_failure(executor, now_ms) else {
        return;
    };
    let text = format!(
        "⚠︎ uwuwu: executor «{}» недоступен — {} транспортных отказов за {} мин: \
         дример/CUP деградированы, спавны в hold. Лечение: рестарт rathole-клиента на ноде.",
        incident.executor,
        incident.failures_in_window,
        EXECUTOR_UNAVAILABLE_WINDOW_MS / 60_000,
    );
    match notify {
        Some((tg, chat)) => match tg.send_message(*chat, &text).await {
            Ok(()) => tracing::warn!(
                event = "executor.unavailable",
                executor = %incident.executor,
                failures = incident.failures_in_window,
                "Executor-сигнал владельцу отправлен"
            ),
            Err(e) => tracing::error!(
                event = "executor.unavailable_failed",
                executor = %incident.executor,
                error = %e,
                "Executor-сигнал не отправлен"
            ),
        },
        None => tracing::warn!(
            event = "executor.unavailable_unnotified",
            executor = %incident.executor,
            failures = incident.failures_in_window,
            "Executor недоступен: [notify] не настроен — сигнала нет"
        ),
    }
}

async fn tick_loop(args: TickLoopArgs) {
    let TickLoopArgs {
        poll_sec,
        plan,
        usecase,
        shared,
        sidecars,
        notify,
        clock,
    } = args;
    // Инцидент-громкость (#1281): N+ сталл-эскалаций за скользящее окно —
    // один ТГ-сигнал владельцу вместо N тихих парковок. Окно неперсистентно
    // осознанно: волна эскалаций, разнесённая через рестарт, одного сигнала
    // не даст — «рестарт = свежее окно» (философия ADR-107), персистентность
    // планом не требовалась.
    let mut loudness = IncidentLoudness::new(
        STALL_INCIDENT_THRESHOLD,
        STALL_INCIDENT_WINDOW_MS,
        STALL_INCIDENT_COOLDOWN_MS,
    );
    // Громкость деградации executor'ов (#1312): per-executor инциденты —
    // те же параметры окна, что у сталла; окно неперсистентно осознанно
    // («рестарт = свежее окно», философия ADR-107).
    let mut executor_incidents = ExecutorLoudness::new(
        EXECUTOR_UNAVAILABLE_THRESHOLD,
        EXECUTOR_UNAVAILABLE_WINDOW_MS,
        EXECUTOR_UNAVAILABLE_COOLDOWN_MS,
    );
    loop {
        let started = std::time::Instant::now();
        match usecase.run(&plan).await {
            Ok(outcome) => {
                for _ in 0..outcome.stall_flagged {
                    incident_signal(&notify, &mut loudness, clock.now_ms()).await;
                }
                for executor in &outcome.executors_failed {
                    executor_unavailable_signal(
                        &notify,
                        &mut executor_incidents,
                        executor,
                        clock.now_ms(),
                    )
                    .await;
                }
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
