//! env-gc (#1431): снос env терминальных строк по гардам — серверный
//! аналог погибшего с ADR-104 cleanupEnv. Паттерн — клин холодного
//! ящика (sweeper.rs): отбор из снапшота доски, батч-кап, observe.
//! Исполнение — детерминированный shell-канал без LLM: bootstrap-сессия
//! в чекауте проекта → один POST /session/:id/shell со скриптом
//! ops/task-env-gc.sh sweep <rows> → вердикты из stdout shell-сообщения
//! (parse-all). Никаких slash-команд и user-сообщений: команда
//! task-env-gc вырезана (#1526 — её user-сообщение «ENV-GC … / Служебная
//! команда…» плодило мусорные сессии в листинге исполнителя и жгло
//! LLM-поворот на проход).

use crate::error::UsecaseResult;
use crate::ports::{SessionInfo, SessionSpec, TaskBoardRepo};
use crate::router::Gateway;
use crate::spawn_models::SpawnModels;
use uwuwu_domain::{AgentSlot, SessionSighting, Task};

mod row;
use row::{collect_verdicts, preflight_row, revalidate_row};

/// Батч-кап прохода (путь клина): rm мульти-ГБ каталогов медленный,
/// остаток переезжает следующими проходами.
pub const ENV_GC_BATCH_CAP: usize = 20;

/// Бюджет прохода (мс): пост-батчинг это цена прохода, а не строки —
/// скрипт батча исполняется внутри синхронного POST shell (проба
/// #1526: ответ после полного завершения shell-блока), per-request
/// таймаут команды обязан его покрывать; недождённые строки переезжают
/// следующим проходом.
pub const ENV_GC_VERDICT_DEADLINE_MS: u64 = 15 * 60_000;

/// Прямой вызов скрипта сноса в cwd bootstrap-сессии (чекаут проекта).
/// Абсолютный $HOME-путь — как было в шаблоне slash-команды: носитель
/// скрипта не зависит от проекта доски, cwd (чекаут) нужен только
/// ownership-гарду worktree. Slash-команда вырезана (#1526): её
/// user-сообщение «ENV-GC … / Служебная команда конвейера…» плодило
/// мусорные сессии в листинге исполнителя и жгло LLM-поворот на проход.
pub const TASK_ENV_GC_SCRIPT: &str = "$HOME/uwuwu-agent/server/ops/task-env-gc.sh";

/// Титул bootstrap-сессии прохода по доске. Точный ключ рипера
/// протухших сессий: матчинг только по нему (SessionInfo.title с
/// носителя), каталог/агент для опознания не годятся.
pub fn env_gc_bootstrap_title(board_name: &str) -> String {
    format!("env-gc {board_name}")
}

/// Политика прохода: возраст-порог и observe-гейт (срез 4 плана #1431:
/// проба перед live-флипом).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvGcPolicy {
    pub now_ms: u64,
    pub age_days: u32,
    pub observe: bool,
}

/// Итог прохода по одной доске (наблюдаемость в логах).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnvGcOutcome {
    /// Кандидатов отобрано (доменное правило).
    pub candidates: usize,
    /// Строк, удержанных живой сессией в env (переедут следующим тиком).
    pub held_live: usize,
    /// Строк батча, принятых исполнителем одной командой (растёт только
    /// после Ok команды; транспортный отказ — все failed, dispatched = 0).
    pub dispatched: usize,
    pub swept: usize,
    pub dirty: usize,
    pub norepo: usize,
    pub foreign: usize,
    pub absent: usize,
    /// Команда не принята / вердикт FAIL / вердикта нет / лента не
    /// прочитана.
    pub failed: usize,
    /// Строк, возрождённых после снапшота (ревалидация + перепроба).
    pub revoked: usize,
}

/// Контекст прохода: всё, что нужно одной доске, — не дублируется
/// аргументами (qlty function-parameters).
pub struct EnvGcCtx<'a> {
    pub board_name: &'a str,
    /// Мастер-чекаут проекта на исполнителе (cwd bootstrap-сессии).
    pub checkout: &'a str,
    /// Корень env задач на исполнителе.
    pub env_root: &'a str,
    pub tasks: &'a [Task],
    pub sessions: &'a Gateway,
    pub models: &'a SpawnModels,
    /// Носитель доски — свежие пробы статуса строк (ревалидация ревайва).
    pub board: &'a dyn TaskBoardRepo,
}

/// Надёжное гашение bootstrap-сессии прохода: до 3 попыток, итог громко
/// в лог (тихий `let _` давал утечку при занятой сессии — класс
/// протухших «env-gc …» в листинге исполнителя, #1526). Без slash-команд
/// в сессии LLM-ход не стартует — гашение штатное, ретрай на форс-мажор.
async fn delete_bootstrap(sessions: &Gateway, bootstrap: &str, board_name: &str) {
    for attempt in 1..=3u32 {
        match sessions.delete_session(bootstrap).await {
            Ok(()) => return,
            Err(e) if attempt < 3 => {
                tracing::warn!(
                    event = "env.bootstrap_delete_retry",
                    board = board_name,
                    attempt,
                    error = %e,
                    "Гашение bootstrap не прошло — ретрай"
                );
            }
            Err(e) => {
                tracing::warn!(
                    event = "env.bootstrap_not_deleted",
                    board = board_name,
                    session = bootstrap,
                    error = %e,
                    "Bootstrap-сессия не погашена — снесёт рипер следующего прохода"
                );
            }
        }
    }
}
/// Рипер протухших bootstrap-сессий прохода (#1526): user-сообщения
/// slash-команды плодили висяки «env-gc …» в листинге исполнителя
/// (61 шт. в проде), тихий `let _` delete их не гасил. Точный ключ —
/// титул прохода; чужие титулы и занятые сессии не трогаем.
async fn reap_stale_bootstraps(sessions: &Gateway, board_name: &str, list: &[SessionInfo]) {
    let title = env_gc_bootstrap_title(board_name);
    for s in list {
        if s.title != title || s.busy {
            continue;
        }
        if let Err(e) = sessions.delete_session(&s.id).await {
            tracing::warn!(
                event = "env.gc_reaper_failed",
                board = board_name,
                session = %s.id,
                error = %e,
                "Протухшая bootstrap-сессия не снесена — попробует следующий проход"
            );
        } else {
            tracing::info!(
                event = "env.gc_reaped",
                board = board_name,
                session = %s.id,
                "Протухшая bootstrap-сессия снесена"
            );
        }
    }
}
/// Bootstrap-сессия прохода: cwd = чекаут проекта (в нём живёт
/// ownership-гард скрипта); прямой shell, БЕЗ slash-команд и LLM.
/// Пустой батч — None, команды нет.
async fn ensure_bootstrap(ctx: &EnvGcCtx<'_>, batch_is_empty: bool) -> Option<String> {
    if batch_is_empty {
        return None;
    }
    let boot_model = ctx.models.for_default();
    match ctx
        .sessions
        .create_session(&SessionSpec {
            directory: ctx.checkout.to_string(),
            title: env_gc_bootstrap_title(ctx.board_name),
            custom_agent: None,
            agent: AgentSlot::Default,
            model: boot_model.model,
            variant: boot_model.variant,
            bootstrap: true,
        })
        .await
    {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::warn!(
                event = "env.gc.blind",
                board = ctx.board_name,
                error = %e,
                "Bootstrap-сессия не создана — проход no-op"
            );
            None
        }
    }
}

/// Проход env-gc по одной доске: рипер протухших bootstrap → отбор →
/// pre-flight → перепроба → один прямой shell с батчем строк → сбор
/// вердиктов из stdout shell-сообщения → надёжное гашение bootstrap.
pub async fn sweep_envs(ctx: &EnvGcCtx<'_>, policy: EnvGcPolicy) -> UsecaseResult<EnvGcOutcome> {
    let mut out = EnvGcOutcome::default();
    let mut candidates: Vec<&Task> = ctx
        .tasks
        .iter()
        .filter(|t| t.is_env_gc_candidate(policy.now_ms, policy.age_days))
        .collect();
    out.candidates = candidates.len();
    if candidates.is_empty() {
        return Ok(out);
    }
    // старейшие первыми: свежие Done дольше остаются видимыми на доске
    candidates.sort_by_key(|t| t.closed_at());
    tracing::info!(
        event = "env.gc.candidates",
        board = ctx.board_name,
        candidates = out.candidates,
        observe = policy.observe,
        "env-gc: кандидаты на снос"
    );
    if policy.observe {
        for t in &candidates {
            tracing::info!(
                event = "env.gc.observe",
                board = ctx.board_name,
                number = t.number(),
                "observe: env строки ушёл бы в снос"
            );
        }
        return Ok(out);
    }
    // Гард 2: скан исполнителя — отказ = no-op прохода (ADR-058:
    // «не знаю» ≠ «можно сносить»).
    let Ok(list) = ctx.sessions.list_sessions().await else {
        tracing::warn!(
            event = "env.gc.blind",
            board = ctx.board_name,
            "Скан исполнителя недоступен — проход no-op"
        );
        return Ok(out);
    };
    let sightings: Vec<SessionSighting> = list
        .iter()
        .map(|s| SessionSighting {
            id: s.id.clone(),
            directory: s.directory.clone(),
            agent: s.agent.clone(),
            outcome: s.outcome.clone(),
            updated_ms: s.updated_ms,
        })
        .collect();
    let targets: Vec<&Task> = candidates.into_iter().take(ENV_GC_BATCH_CAP).collect();
    // Рипер протухших bootstrap-сессий — до создания своей, чтобы не
    // задеть текущий проход (титулы совпадают, своих id ещё нет).
    reap_stale_bootstraps(ctx.sessions, ctx.board_name, &list).await;
    // Pre-flight без команд: ревалидация статуса, гарды сессий, гашение
    // завершённых; готовые строки — в батч.
    let pf = row::PreflightCtx {
        board: ctx.board,
        sessions: ctx.sessions,
        board_name: ctx.board_name,
        env_root: ctx.env_root,
        sightings: &sightings,
        now_ms: policy.now_ms,
    };
    let mut ready: Vec<&Task> = Vec::new();
    for t in &targets {
        preflight_row(&pf, t, &mut out, &mut ready).await;
    }
    // Перепроба непосредственно перед диспатчем: stalled-проба pre-flight
    // устаревает ранние строки — перепроба сужает окно «проба → снос» до
    // последнего цикла (ревью раунд 3, #1526).
    let mut batch: Vec<&Task> = Vec::new();
    for t in &ready {
        revalidate_row(ctx.board, ctx.board_name, t, &mut out, &mut batch).await;
    }
    // Bootstrap-сессия на проход: cwd = чекаут проекта — в нём живёт
    // ownership-гард скрипта. Прямой shell, БЕЗ slash-команд и LLM:
    // команда task-env-gc вырезана (#1526 — её user-сообщение «ENV-GC …
    // / Служебная команда конвейера…» плодило мусорные сессии в листинге
    // исполнителя и жгло LLM-поворот на проход).
    // Пустой батч (все строки pre-flight/перепробой сняты) — bootstrap
    // не создаётся, шелла нет.
    let bootstrap = ensure_bootstrap(ctx, batch.is_empty()).await;
    // Один прямой shell на проход: скрипт циклит строки сам и печатает
    // вердикт на каждую; итог — из stdout shell-сообщения (parse-all).
    // Без user-сообщений и LLM-поворота (живая проба opencode 2.3.2:
    // POST shell → 204, в ленте только type shell, токены 0).
    if let Some(bootstrap) = &bootstrap {
        collect_verdicts(ctx.sessions, bootstrap, ctx.board_name, &batch, &mut out).await;
    }
    // Любой исход: bootstrap гасится с ретраем (тихий `let _` давал
    // утечку при занятой сессии — класс протухших «env-gc …» в листинге).
    // Без шелла в сессии LLM-ход не стартует — гашение штатное.
    if let Some(bootstrap) = &bootstrap {
        delete_bootstrap(ctx.sessions, bootstrap, ctx.board_name).await;
    }
    tracing::info!(
        event = "env.gc.completed",
        board = ctx.board_name,
        candidates = out.candidates,
        held = out.held_live,
        revoked = out.revoked,
        batch = batch.len(),
        bootstrap_created = bootstrap.is_some(),
        dispatched = out.dispatched,
        swept = out.swept,
        dirty = out.dirty,
        norepo = out.norepo,
        foreign = out.foreign,
        absent = out.absent,
        failed = out.failed,
        "env-gc: проход завершён"
    );
    Ok(out)
}
