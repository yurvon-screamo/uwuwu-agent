//! Клин холодного ящика доски (#1293): терминальные строки, чей issue
//! закрыт строго старше N суток, уходят в архив Projects v2 — снапшот
//! несёт активный контур, а не накопленную историю. Проход механический
//! (sidecar тика, без LLM): отбор — доменное правило
//! [`Task::is_sweep_candidate`], исполнение — мутация порта.

use crate::error::UsecaseResult;
use crate::ports::TaskBoardRepo;
use uwuwu_domain::Task;

/// Батч-кап прохода: первая волна порядка сотен кандидатов размазывается
/// по тикам (полсотни мутаций за проход при poll 180 c ≈ полчаса на
/// первую волну), а не бьёт по GraphQL-бюджету залпом.
pub fn sweep_batch_cap() -> usize {
    crate::runtime_cfg::pipeline::sweep_cap()
}

/// Бюджет-гайд: мутации клина живут в том же часовом окне, что фича
/// спасает — при remaining ниже порога архивации приостанавливаются
/// до следующего тика (остаток волны переезжает, ничего не теряется).
pub fn budget_floor() -> u64 {
    crate::runtime_cfg::pipeline::budget_floor()
}

/// Итог прохода по одной доске (наблюдаемость первой волны).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    /// Кандидатов отобрано (доменное правило).
    pub candidates: usize,
    /// Архивировано в этом проходе.
    pub archived: usize,
    /// Отложено бюджет-гайдом (переедут следующими тиками).
    pub deferred_budget: usize,
    /// Не влезло в батч-кап прохода (переедут следующими тиками).
    pub deferred_cap: usize,
    /// Не удалось архивировать (строка переедет следующим тиком).
    pub failed: usize,
}

/// Политика прохода клина: часы и пороги одного тика.
#[derive(Debug, Clone, Copy)]
pub struct SweepPolicy {
    /// Сейчас (unix ms) — возраст закрытия меряется от него.
    pub now_ms: u64,
    /// Порог «строго старше N суток» (конфиг sweep_days).
    pub age_days: u32,
    /// Внутренний observe-гейт: кандидаты логируются, мутаций нет.
    pub observe: bool,
}

/// Проход клина по одной доске. `observe` — **внутренний** гейт клина:
/// кандидаты логируются, мутаций нет (деплой наблюдаем, мутации после
/// флипа live). Это не общий no-op-гейт сайдкаров main.rs — в observe
/// фаза обязана показывать кандидатов, иначе ничего не доказывает.
pub async fn sweep_board(
    board: &dyn TaskBoardRepo,
    board_name: &str,
    tasks: &[Task],
    policy: SweepPolicy,
) -> UsecaseResult<SweepOutcome> {
    let mut out = SweepOutcome::default();
    let mut candidates: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.is_sweep_candidate(policy.now_ms, policy.age_days))
        .collect();
    out.candidates = candidates.len();
    if candidates.is_empty() {
        return Ok(out);
    }
    // старейшие первыми: свежие Done дольше остаются видными на доске
    candidates.sort_by_key(|t| t.closed_at());
    tracing::info!(
        event = "sweep.candidates",
        board = board_name,
        candidates = out.candidates,
        observe = policy.observe,
        "Клин холодного ящика: кандидаты на архивацию"
    );
    if policy.observe {
        for t in &candidates {
            tracing::info!(
                event = "sweep.observe",
                board = board_name,
                number = t.number(),
                "observe: строка ушла бы в архив"
            );
        }
        return Ok(out);
    }
    for t in candidates.into_iter().take(sweep_batch_cap()) {
        if board
            .gql_budget_remaining()
            .is_some_and(|remaining| remaining < budget_floor())
        {
            out.deferred_budget = out.candidates - out.archived - out.failed;
            tracing::warn!(
                event = "sweep.budget_floor",
                board = board_name,
                deferred = out.deferred_budget,
                "GraphQL-бюджет ниже порога — архивации приостановлены до следующего тика"
            );
            break;
        }
        match board.archive_row(board_name, t.number()).await {
            Ok(()) => out.archived += 1,
            Err(e) => {
                out.failed += 1;
                tracing::warn!(
                    event = "sweep.failed",
                    board = board_name,
                    number = t.number(),
                    error = %e,
                    "Архивация строки не удалась — переедет следующим тиком, волну продолжаем"
                );
            }
        }
    }
    // Свежий срез — один на волну, не на строку (High-1 ревью #1293):
    // построчный ре-фетч полного снимка выжег бы бюджет первой волной
    if out.archived > 0 {
        board.invalidate_snapshot(board_name);
    }
    // остаток за батч-капом: не отобран бюджетом и не упал — ждёт тика
    out.deferred_cap = out.candidates - out.archived - out.failed - out.deferred_budget;
    tracing::info!(
        event = "sweep.completed",
        board = board_name,
        candidates = out.candidates,
        archived = out.archived,
        deferred_budget = out.deferred_budget,
        failed = out.failed,
        "Клин холодного ящика: проход завершён"
    );
    Ok(out)
}
