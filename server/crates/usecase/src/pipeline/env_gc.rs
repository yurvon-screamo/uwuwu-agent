//! env-gc (#1431): снос env терминальных строк по гардам — серверный
//! аналог погибшего с ADR-104 cleanupEnv. Паттерн — клин холодного
//! ящика (sweeper.rs): отбор из снапшота доски, батч-кап, observe.
//! Исполнение — детерминированный shell-канал спавна: bootstrap-сессия
//! в чекауте проекта → одна slash-команда task-env-gc на проход с
//! батчем строк (#1526) → ops/task-env-gc.sh; вердикты строк сервер
//! собирает из батч-сообщения ленты (wait_user_message + parse-all).

use std::time::Instant;

use crate::error::UsecaseResult;
use crate::ports::SessionSpec;
use crate::router::Gateway;
use crate::spawn_models::SpawnModels;
use uwuwu_domain::{AgentSlot, SessionSighting, Task};

mod row;
use row::{collect_verdicts, preflight_row, revalidate_row};

/// Батч-кап прохода (путь клина): rm мульти-ГБ каталогов медленный,
/// остаток переезжает следующими проходами.
pub const ENV_GC_BATCH_CAP: usize = 20;

/// Бюджет прохода (мс): пост-батчинг это цена прохода, а не строки —
/// скрипт батча исполняется внутри синхронного POST /command (проба
/// #1526: ответ после полного завершения shell-блока), per-request
/// таймаут команды обязан его покрывать; недождённые строки переезжают
/// следующим проходом.
pub const ENV_GC_VERDICT_DEADLINE_MS: u64 = 15 * 60_000;

/// Страх-дедлайн сбора вердиктов (мс): при Ok команды сообщение уже в
/// ленте, штатно сбор мгновенный; запас — на ретраи Err опроса.
const VERDICT_READ_GRACE_MS: u64 = 5_000;

/// Кап ретраев Err опроса ленты: транспортный отказ возвращается
/// адаптером немедленно, пауза между ретраями в юзкейсе недоступна
/// (слой только домена) — ограничитель числа попыток.
const VERDICT_READ_MAX_RETRIES: usize = 10;

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
    pub board: &'a dyn crate::ports::TaskBoardRepo,
}

/// Проход env-gc по одной доске: отбор → pre-flight → перепроба →
/// одна команда с батчем строк → сбор вердиктов из батч-сообщения.
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
    if ready.is_empty() {
        tracing::info!(
            event = "env.gc.completed",
            board = ctx.board_name,
            candidates = out.candidates,
            held = out.held_live,
            revoked = out.revoked,
            failed = out.failed,
            "env-gc: проход завершён — готовых строк нет, диспатча нет"
        );
        return Ok(out);
    }
    // Перепроба непосредственно перед диспатчем: stalled-проба pre-flight
    // устаревает ранние строки — перепроба сужает окно «проба → снос» до
    // последнего цикла (ревью раунд 3, #1526).
    let mut batch: Vec<&Task> = Vec::new();
    for t in &ready {
        revalidate_row(ctx.board, ctx.board_name, t, &mut out, &mut batch).await;
    }
    if batch.is_empty() {
        tracing::info!(
            event = "env.gc.completed",
            board = ctx.board_name,
            candidates = out.candidates,
            revoked = out.revoked,
            failed = out.failed,
            "env-gc: проход завершён — перепроба сняла все строки"
        );
        return Ok(out);
    }
    // Bootstrap-сессия на проход: cwd = чекаут проекта — в нём живёт
    // ownership-гард скрипта; slash-команды = shell без LLM-решений.
    let boot_model = ctx.models.for_default();
    let bootstrap = match ctx
        .sessions
        .create_session(&SessionSpec {
            directory: ctx.checkout.to_string(),
            title: format!("env-gc {}", ctx.board_name),
            custom_agent: None,
            agent: AgentSlot::Default,
            model: boot_model.model,
            variant: boot_model.variant,
            bootstrap: true,
        })
        .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                event = "env.gc.blind",
                board = ctx.board_name,
                error = %e,
                "Bootstrap-сессия не создана — проход no-op"
            );
            return Ok(out);
        }
    };
    // Одна команда на проход: строки батча через пробел; скрипт циклит
    // сам и печатает вердикт на каждую (tail-join $1 шаблона — проба #1526).
    let args: String = batch
        .iter()
        .map(|t| t.number().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let started = Instant::now();
    match ctx
        .sessions
        .run_command(&bootstrap, "task-env-gc", &args, ENV_GC_VERDICT_DEADLINE_MS)
        .await
    {
        Ok(()) => {
            out.dispatched = batch.len();
            collect_verdicts(ctx.sessions, &bootstrap, &batch, &mut out).await;
        }
        Err(e) => {
            // Транспортный отказ: обрыв запроса убивает скрипт посреди
            // выполнения (проба #1526, script.log) — сообщение не сядет,
            // ждать нечего; все строки батча переедут следующим проходом.
            // elapsed у бюджета — маршрут режет длинный запрос (детектор
            // «Риски» плана), мгновенный — носитель недоступен.
            out.failed += batch.len();
            tracing::warn!(
                event = "env.gc_fail",
                board = ctx.board_name,
                elapsed_ms = started.elapsed().as_millis() as u64,
                budget_ms = ENV_GC_VERDICT_DEADLINE_MS,
                batch = batch.len(),
                error = %e,
                "Команда сноса не принята исполнителем — батч переедет следующим проходом"
            );
        }
    }
    // Любой исход: bootstrap гасится (нет утечки сессий, как у спавна).
    let _ = ctx.sessions.delete_session(&bootstrap).await;
    tracing::info!(
        event = "env.gc.completed",
        board = ctx.board_name,
        candidates = out.candidates,
        held = out.held_live,
        revoked = out.revoked,
        batch = batch.len(),
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
