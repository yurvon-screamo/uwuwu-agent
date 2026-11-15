//! Тик конвейера: чтение досок → домен решает → гейты → исполнение
//! через порты. Оркестрация знает слова (логи/промпты) — домен нет.

use crate::error::{UsecaseError, UsecaseResult};
use crate::ports::{AgentSessionGateway, Clock, PrGateway, TaskBoardRepo, TickStateStore};
use serde::Serialize;
use std::sync::Arc;
use uwuwu_domain::{
    decide, gate, Action, Awaits, Gate, LoopOutcome, ModelRef, SpawnLoop, SpawnPolicy, Status, Task,
};

/// Режим конвейера: observe — только решения в лог; live — исполнение.
/// Кулдаун спавна слота строки: 15 минут (анти-шторм любого происхождения).
pub const SPAWN_COOLDOWN_MS: u64 = 15 * 60_000;

pub(crate) fn slot_name(slot: uwuwu_domain::Slot) -> &'static str {
    match slot {
        uwuwu_domain::Slot::Design => "design",
        uwuwu_domain::Slot::Dev => "dev",
        uwuwu_domain::Slot::PlanReview => "plan-review",
        uwuwu_domain::Slot::ResultReview => "result-review",
        uwuwu_domain::Slot::Test => "test",
    }
}

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
    pub actions_failed: usize,
    pub rows_total: usize,
    pub spawns_allowed: usize,
    pub spawns_done: usize,
    pub held_cup: usize,
    pub deferred_peak: usize,
    pub observed: usize,
    pub wakeups: usize,
    pub autodone_done: usize,
    /// Строк, переведённых в Needs attention спавн-петлёй.
    pub attention_flagged: usize,
}

/// Оркестратор тика: порты внедряются извне (композиция в uwuwu-server).
pub struct TickUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub prs: Arc<dyn PrGateway>,
    pub clock: Arc<dyn Clock>,
    pub state: Arc<dyn TickStateStore>,
    /// Модель спавна (provider, id); None — дефолт исполнителя.
    pub spawn_model: Option<ModelRef>,
}

impl TickUseCase {
    pub async fn run(&self, plan: &TickPlan) -> UsecaseResult<TickOutcome> {
        let mut out = TickOutcome::default();
        // CUP-кап глобальный по проектам (канон #1133): активные строки
        // считаются по всем доскам до обхода, не per-board
        let mut boards = Vec::with_capacity(plan.boards.len());
        for board in &plan.boards {
            let tasks = self.board.read_board(&board.board).await?;
            out.rows_total += tasks.len();
            boards.push((board, tasks));
        }
        let global_active: usize = boards
            .iter()
            .map(|(_, tasks)| tasks.iter().filter(|t| t.status().is_active()).count())
            .sum();
        for (board, tasks) in boards {
            out = self
                .run_board(plan, board, tasks, global_active, out)
                .await?;
        }
        if let Err(e) = self.state.save() {
            tracing::error!(event = "state.save_failed", error = %e, "Failed to save tick state");
        }
        tracing::info!(
            event = "tick.completed",
            rows = out.rows_total,
            spawns = out.spawns_done,
            held_cup = out.held_cup,
            deferred_peak = out.deferred_peak,
            wakeups = out.wakeups,
            autodone = out.autodone_done,
            mode = ?plan.mode,
            failed = out.actions_failed,
            "Tick completed"
        );
        Ok(out)
    }

    async fn run_board(
        &self,
        plan: &TickPlan,
        board: &BoardPlan,
        mut tasks: Vec<Task>,
        global_active: usize,
        mut out: TickOutcome,
    ) -> UsecaseResult<TickOutcome> {
        let now = self.clock.utc_minute();
        tasks.sort_by_key(|t| t.priority().rank());
        for task in &tasks {
            let action = decide(task);
            match action {
                Action::Spawn { slot } => {
                    // Анти-шторм: слот строки не спавнится чаще кулдауна
                    let cooldown_left = {
                        let last =
                            self.state
                                .last_spawn_ms(&board.board, task.number(), slot_name(slot));
                        last.map(|ts| self.clock.now_ms().saturating_sub(ts))
                            .unwrap_or(u64::MAX)
                    };
                    if cooldown_left < SPAWN_COOLDOWN_MS {
                        out.observed += 1;
                        continue;
                    }
                    // Attention-механика: >2 спавнов слота при неизменном
                    // статусе = петля → Needs attention, конвейер бросает строку
                    let mut guard = self
                        .state
                        .spawn_loop(&board.board, task.number(), slot_name(slot))
                        .map(|(s, r)| SpawnLoop::from_parts(&s, r))
                        .unwrap_or_else(|| SpawnLoop::new(task.status()));
                    if guard.register_spawn(task.status()) == LoopOutcome::Escalate {
                        out.attention_flagged += 1;
                        tracing::warn!(
                            event = "spawn.loop_escalated",
                            number = task.number(),
                            slot = slot_name(slot),
                            "Spawn loop >2: Needs attention"
                        );
                        if plan.mode == Mode::Live {
                            let _ = self
                                .board
                                .set_status(&board.board, task.number(), Status::NeedsAttention)
                                .await;
                            let _ = self
                                .board
                                .post_comment(
                                    task.number(),
                                    &format!(
                                        "⚠︎ Спавн-петля: слот `{}` спавнился >2 раз без \
                                         прогресса статуса ({}). Строка переведена в \
                                         Needs attention — разбор владельца.",
                                        slot_name(slot),
                                        task.status().as_str()
                                    ),
                                )
                                .await;
                        }
                        continue;
                    }
                    let (g_status, g_repeats) = guard.parts();
                    self.state.set_spawn_loop(
                        &board.board,
                        task.number(),
                        slot_name(slot),
                        g_status,
                        g_repeats,
                    );
                    let gated = gate(task, &action, plan.policy, global_active, now);
                    match gated {
                        Gate::Allowed => {
                            out.spawns_allowed += 1;
                            if plan.mode == Mode::Live {
                                self.state.mark_spawn(
                                    &board.board,
                                    task.number(),
                                    slot_name(slot),
                                    self.clock.now_ms(),
                                );
                                match self.spawn_stage(board, task, slot).await {
                                    Ok(session) => {
                                        if let Err(e) = self
                                            .board
                                            .set_session(
                                                &board.board,
                                                task.number(),
                                                slot,
                                                &session,
                                            )
                                            .await
                                        {
                                            out.actions_failed += 1;
                                            self.log_action_failed(task, e);
                                        } else {
                                            out.spawns_done += 1;
                                        }
                                    }
                                    Err(e) => {
                                        out.actions_failed += 1;
                                        self.log_action_failed(task, e);
                                    }
                                }
                            }
                        }
                        Gate::HeldCup { active, cap } => {
                            out.held_cup += 1;
                            tracing::info!(
                                event = "spawn.held_cup",
                                number = task.number(),
                                active,
                                cap,
                                "Spawn held: CUP cap"
                            );
                        }
                        Gate::DeferredPeak => {
                            out.deferred_peak += 1;
                            tracing::info!(
                                event = "spawn.deferred_peak",
                                number = task.number(),
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
                            number = task.number(),
                            pr = task.pr().map(|u| u.as_str()).unwrap_or(""),
                            "Autodone probe (observe)"
                        );
                    }
                }
                Action::Observe { awaits } => {
                    if plan.mode == Mode::Live && awaits == Awaits::StageSession {
                        match self.wakeup_if_fresh(board, task).await {
                            Ok(true) => out.wakeups += 1,
                            Ok(false) => {}
                            Err(e) => {
                                out.actions_failed += 1;
                                self.log_action_failed(task, e);
                            }
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
        tracing::error!(event = "action.failed", number = task.number(), error = %e, "Action failed");
        e
    }
}
