//! Тик конвейера: чтение досок → домен решает → гейты → исполнение
//! через порты. Оркестрация знает слова (логи/промпты) — домен нет.

use crate::error::{UsecaseError, UsecaseResult};
use crate::ports::{AgentSessionGateway, Clock, PrGateway, TaskBoardRepo, TickStateStore};
use serde::Serialize;
use std::sync::Arc;
use uwuwu_domain::{decide, gate, Action, Awaits, Gate, SpawnPolicy, Task};

/// Режим конвейера: observe — только решения в лог; live — исполнение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Observe,
    Live,
}

/// Доска в плане тика: всё, что нужно оркестрации об окружении проекта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardPlan {
    pub project: String,
    pub board: String,
    /// Репо проекта (PR-пробы/мерж).
    pub repo: String,
    /// Мастер-чекаут проекта на исполнителе (cwd bootstrap-сессии).
    pub checkout: String,
    /// Корень env задач на исполнителе (~/agents-env).
    pub env_root: String,
}

/// План тика: режим + политика + доски.
#[derive(Debug, Clone)]
pub struct TickPlan {
    pub mode: Mode,
    pub policy: SpawnPolicy,
    pub boards: Vec<BoardPlan>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TickOutcome {
    pub rows_total: usize,
    pub spawns_allowed: usize,
    pub spawns_done: usize,
    pub held_cup: usize,
    pub deferred_peak: usize,
    pub observed: usize,
    pub wakeups: usize,
    pub autodone_done: usize,
}

/// Оркестратор тика: порты внедряются извне (композиция в uwuwu-server).
pub struct TickUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub prs: Arc<dyn PrGateway>,
    pub clock: Arc<dyn Clock>,
    pub state: Arc<dyn TickStateStore>,
    /// Модель спавна (provider, id); None — дефолт исполнителя.
    pub spawn_model: Option<(String, String)>,
}

impl TickUseCase {
    pub async fn run(&self, plan: &TickPlan) -> UsecaseResult<TickOutcome> {
        let mut out = TickOutcome::default();
        for board in &plan.boards {
            let tasks = self.board.read_board(&board.board).await?;
            out.rows_total += tasks.len();
            out = self.run_board(plan, board, tasks, out).await?;
        }
        self.state.save()?;
        tracing::info!(
            event = "tick.completed",
            rows = out.rows_total,
            spawns = out.spawns_done,
            held_cup = out.held_cup,
            deferred_peak = out.deferred_peak,
            wakeups = out.wakeups,
            autodone = out.autodone_done,
            mode = ?plan.mode,
            "Tick completed"
        );
        Ok(out)
    }

    async fn run_board(
        &self,
        plan: &TickPlan,
        board: &BoardPlan,
        mut tasks: Vec<Task>,
        mut out: TickOutcome,
    ) -> UsecaseResult<TickOutcome> {
        let active = tasks.iter().filter(|t| t.status.is_active()).count();
        let now = self.clock.utc_minute();
        tasks.sort_by_key(|t| t.priority.rank());
        for task in &tasks {
            let action = decide(task);
            match action {
                Action::Spawn { slot } => {
                    let gated = gate(task, &action, plan.policy, active, now);
                    match gated {
                        Gate::Allowed => {
                            out.spawns_allowed += 1;
                            if plan.mode == Mode::Live {
                                let session = self
                                    .spawn_stage(board, task, slot)
                                    .await
                                    .map_err(|e| self.log_action_failed(task, e))?;
                                self.board
                                    .set_session(&board.board, task.number, slot, &session)
                                    .await?;
                                out.spawns_done += 1;
                            }
                        }
                        Gate::HeldCup { active, cap } => {
                            out.held_cup += 1;
                            tracing::info!(
                                event = "spawn.held_cup",
                                number = task.number,
                                active,
                                cap,
                                "Spawn held: CUP cap"
                            );
                        }
                        Gate::DeferredPeak => {
                            out.deferred_peak += 1;
                            tracing::info!(
                                event = "spawn.deferred_peak",
                                number = task.number,
                                "Spawn deferred: peak window"
                            );
                        }
                    }
                }
                Action::Autodone => {
                    if plan.mode == Mode::Live {
                        out.autodone_done += 1;
                        self.autodone(board, task)
                            .await
                            .map_err(|e| self.log_action_failed(task, e))?;
                    } else {
                        tracing::info!(
                            event = "autodone.probe",
                            number = task.number,
                            pr = task.pr.as_deref().unwrap_or(""),
                            "Autodone probe (observe)"
                        );
                    }
                }
                Action::Observe { awaits } => {
                    if plan.mode == Mode::Live && awaits == Awaits::StageSession {
                        if self.wakeup_if_fresh(board, task).await? {
                            out.wakeups += 1;
                        }
                    } else {
                        out.observed += 1;
                    }
                }
            }
        }
        Ok(out)
    }

    fn log_action_failed(&self, task: &Task, e: UsecaseError) -> UsecaseError {
        tracing::error!(event = "action.failed", number = task.number, error = %e, "Action failed");
        e
    }
}
