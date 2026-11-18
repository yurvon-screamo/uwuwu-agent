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
pub const SWEEP_BATCH_CAP: usize = 50;

/// Бюджет-гайд: мутации клина живут в том же часовом окне, что фича
/// спасает — при remaining ниже порога архивации приостанавливаются
/// до следующего тика (остаток волны переезжает, ничего не теряется).
pub const BUDGET_FLOOR: u64 = 500;

/// Итог прохода по одной доске (наблюдаемость первой волны).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    /// Кандидатов отобрано (доменное правило).
    pub candidates: usize,
    /// Архивировано в этом проходе.
    pub archived: usize,
    /// Отложено бюджет-гайдом (переедут следующими тиками).
    pub deferred_budget: usize,
    /// Не удалось архивировать (строка переедет следующим тиком).
    pub failed: usize,
}

/// Проход клина по одной доске. `observe` — **внутренний** гейт клина:
/// кандидаты логируются, мутаций нет (деплой наблюдаем, мутации после
/// флипа live). Это не общий no-op-гейт сайдкаров main.rs — в observe
/// фаза обязана показывать кандидатов, иначе ничего не доказывает.
pub async fn sweep_board(
    board: &dyn TaskBoardRepo,
    board_name: &str,
    tasks: &[Task],
    now_ms: u64,
    age_days: u32,
    observe: bool,
) -> UsecaseResult<SweepOutcome> {
    let mut out = SweepOutcome::default();
    let mut candidates: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.is_sweep_candidate(now_ms, age_days))
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
        observe,
        "Клин холодного ящика: кандидаты на архивацию"
    );
    if observe {
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
    for t in candidates.into_iter().take(SWEEP_BATCH_CAP) {
        if board
            .gql_budget_remaining()
            .is_some_and(|remaining| remaining < BUDGET_FLOOR)
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
