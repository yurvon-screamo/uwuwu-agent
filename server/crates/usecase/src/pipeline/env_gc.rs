//! env-gc (#1431): снос env терминальных строк по гардам — серверный
//! аналог погибшего с ADR-104 cleanupEnv. Паттерн — клин холодного
//! ящика (sweeper.rs): отбор из снапшота доски, батч-кап, observe.
//! Исполнение — детерминированный shell-канал спавна: bootstrap-сессия
//! в чекауте проекта → slash-команда task-env-gc → ops/task-env-gc.sh;
//! вердикт строки сервер ждёт в message-ленте (wait_user_message).

use crate::error::UsecaseResult;
use crate::ports::SessionSpec;
use crate::router::Gateway;
use crate::spawn_models::SpawnModels;
use uwuwu_domain::{AgentSlot, SessionSighting, Task};

mod row;
use row::sweep_row;

/// Батч-кап прохода (путь клина): rm мульти-ГБ каталогов медленный,
/// остаток переезжает следующими проходами.
pub const ENV_GC_BATCH_CAP: usize = 20;

/// Дедлайн ожидания вердикта одной строки (мс): rm гигабайтных
/// target/node_modules занимает минуты — щедро; недождённая строка
/// переезжает следующим проходом.
pub const ENV_GC_VERDICT_DEADLINE_MS: u64 = 15 * 60_000;

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
    /// Команд сноса, принятых исполнителем.
    pub dispatched: usize,
    pub swept: usize,
    pub dirty: usize,
    pub norepo: usize,
    pub foreign: usize,
    pub absent: usize,
    /// Команда не принята / вердикт FAIL или за дедлайном.
    pub failed: usize,
}

/// Проход env-gc по одной доске: отбор → гарды сессий → shell-снос.
pub async fn sweep_envs(
    board_name: &str,
    checkout: &str,
    env_root: &str,
    tasks: &[Task],
    sessions: &Gateway,
    models: &SpawnModels,
    policy: EnvGcPolicy,
) -> UsecaseResult<EnvGcOutcome> {
    let mut out = EnvGcOutcome::default();
    let mut candidates: Vec<&Task> = tasks
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
        board = board_name,
        candidates = out.candidates,
        observe = policy.observe,
        "env-gc: кандидаты на снос"
    );
    if policy.observe {
        for t in &candidates {
            tracing::info!(
                event = "env.gc.observe",
                board = board_name,
                number = t.number(),
                "observe: env строки ушёл бы в снос"
            );
        }
        return Ok(out);
    }
    // Гард 2: скан исполнителя — отказ = no-op прохода (ADR-058:
    // «не знаю» ≠ «можно сносить»).
    let Ok(list) = sessions.list_sessions().await else {
        tracing::warn!(
            event = "env.gc.blind",
            board = board_name,
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
    // Bootstrap-сессия на проход: cwd = чекаут проекта — в нём живёт
    // ownership-гард скрипта; slash-команды = shell без LLM-решений.
    let boot_model = models.for_default();
    let bootstrap = match sessions
        .create_session(&SessionSpec {
            directory: checkout.to_string(),
            title: format!("env-gc {board_name}"),
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
                board = board_name,
                error = %e,
                "Bootstrap-сессия не создана — проход no-op"
            );
            return Ok(out);
        }
    };
    let ctx = row::RowCtx {
        board_name,
        env_root,
        bootstrap: &bootstrap,
        sessions,
        sightings: &sightings,
        policy,
    };
    for t in &targets {
        sweep_row(&ctx, t, &mut out).await;
    }
    // Любой исход: bootstrap гасится (нет утечки сессий, как у спавна).
    let _ = sessions.delete_session(&bootstrap).await;
    tracing::info!(
        event = "env.gc.completed",
        board = board_name,
        candidates = out.candidates,
        held = out.held_live,
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
