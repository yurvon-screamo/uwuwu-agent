//! Тик конвейера: чтение досок → домен решает → гейты → исполнение
//! через порты. Оркестрация знает слова (логи/промпты) — домен нет.

use crate::error::{UsecaseError, UsecaseResult};
use crate::naming;
use crate::ports::{AgentSessionGateway, Clock, PrGateway, TaskBoardRepo, TickStateStore};
use serde::Serialize;
use std::sync::Arc;
use uwuwu_domain::{
    decide, gate, Action, Awaits, Gate, LoopOutcome, Slot, SpawnLoop, SpawnPolicy, Status, Task,
};

/// Режим конвейера: observe — только решения в лог; live — исполнение.
/// Кулдаун спавна слота строки: 15 минут (анти-шторм любого происхождения).
pub const SPAWN_COOLDOWN_MS: u64 = 15 * 60_000;

/// Сталл-детект: stage-сессия жива, но ход завершён (тишина) и статус
/// не переведён — строка висит до внешнего толчка (паттерн 16.11:
/// #1151, #1135, #384 — агент заканчивает ход без статусной транзакции).
/// Автопинг через час молчания, повтор через два, затем Needs attention.
pub const STALL_SILENCE_MS: u64 = 60 * 60_000;
pub const STALL_PING_EVERY_MS: u64 = 2 * 60 * 60_000;
pub const STALL_MAX_PINGS: u32 = 2;

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
    /// Автопингов сталл-детекта (ход завершён, статус не переведён).
    pub stall_pings: usize,
    /// Строк, переведённых в Needs attention сталл-детектом.
    pub stall_flagged: usize,
}

/// Результат прохода сталл-детекта по строке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StallAct {
    None,
    Pinged,
    Flagged,
}

/// Оркестратор тика: порты внедряются извне (композиция в uwuwu-server).
pub struct TickUseCase {
    pub board: Arc<dyn TaskBoardRepo>,
    pub sessions: Arc<dyn AgentSessionGateway>,
    pub prs: Arc<dyn PrGateway>,
    pub clock: Arc<dyn Clock>,
    pub state: Arc<dyn TickStateStore>,
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
        // Enrichment: snapshot Projects v2 не несёт глобальных полей issue
        // (сессии/ветка/PR) — строки действующих стадий дотягиваем одним
        // REST-зовом каждая. Без этого тик не видит сессию стадии:
        // wakeup недостижим, needs_spawn вечен → спавн-дубли и недоставленные
        // комменты (инцидент ночи 15-16.11).
        for (bp, tasks) in boards.iter_mut() {
            for t in tasks.iter_mut() {
                // stage-строки (wakeup/needs_spawn) + Review-строки
                // (автодон: PR/ветка — глобальные поля, без enrichment
                // автодон слеп — PR #338/#339 висели немержеными)
                if t.stage_slot().is_some() || t.status() == Status::UserReview {
                    match self.board.issue_fields(t.number()).await {
                        Ok(f) => t.apply_fields(f),
                        Err(e) => tracing::warn!(
                            event = "enrich.failed",
                            number = t.number(),
                            error = %e,
                            "issue_fields enrichment"
                        ),
                    }
                }
            }
            let _ = bp;
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
        // Дебаг-гейт одной строки (мок-Е2Е): UWUWU_ONLY_TASK=<number> —
        // конвейер действует только по ней, остальная доска read-only
        if let Ok(only) = std::env::var("UWUWU_ONLY_TASK") {
            if let Ok(n) = only.trim().parse::<u64>() {
                tasks.retain(|t| t.number() == n);
            }
        }
        tasks.sort_by_key(|t| t.priority().rank());
        for task_ref in &tasks {
            let mut cleared_clone: Option<Task> = None;
            // Attention-механика: счётчики слотов, сохранённые при другом
            // статусе строки, устарели (строка ушла и вернулась) — сброс.
            // Live-only: observe стейт не пишет.
            if plan.mode == Mode::Live {
                self.state.reset_stale_spawn_loops(
                    &board.board,
                    task_ref.number(),
                    task_ref.status(),
                );
                // Канон сессий: переход стадии = новый раунд для свежеглазых
                // слотов (ревью/тест — всегда новая сессия, незамыленность
                // оценки); dev-семья (Design/Dev) неприкосновенна — дев
                // сессия едина между этапами и откатами (требование владельца).
                let prev = self.state.last_seen_status(&board.board, task_ref.number());
                if prev.as_ref() != Some(&task_ref.status()) {
                    if let Some(slot) = task_ref.stage_slot() {
                        if matches!(slot, Slot::PlanReview | Slot::ResultReview | Slot::Test)
                            && task_ref.session_of(slot).is_some()
                        {
                            if let Err(e) = self
                                .board
                                .clear_session(&board.board, task_ref.number(), slot)
                                .await
                            {
                                out.actions_failed += 1;
                                self.log_action_failed(task_ref, e);
                            } else {
                                let mut t = task_ref.clone();
                                t.clear_slot_session(slot);
                                cleared_clone = Some(t);
                                // новый раунд: кулдаун прошлого не переносится
                                self.state
                                    .clear_spawn_ms(&board.board, task_ref.number(), slot);
                                tracing::info!(
                                    event = "stage.fresh_eyes",
                                    number = task_ref.number(),
                                    slot = naming::slot_key(slot),
                                    "Переход стадии: раунд свежих глаз (сессия слота снята)"
                                );
                            }
                        }
                    }
                    self.state.set_last_seen_status(
                        &board.board,
                        task_ref.number(),
                        task_ref.status(),
                    );
                }
            }
            // тень: если слот снят переходом — работаем с очищенным клоном
            let task: &Task = cleared_clone.as_ref().unwrap_or(task_ref);
            let action = decide(task);
            match action {
                Action::Spawn { slot } => {
                    // Анти-шторм: слот строки не спавнится чаще кулдауна
                    let cooldown_left = {
                        let last = self.state.last_spawn_ms(&board.board, task.number(), slot);
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
                        .spawn_loop(&board.board, task.number(), slot)
                        .map(|(status, repeats)| {
                            let mut g = SpawnLoop::new(status);
                            for _ in 0..repeats {
                                g.register_spawn(status);
                            }
                            g
                        })
                        .unwrap_or_else(|| SpawnLoop::new(task.status()));
                    if guard.register_spawn(task.status()) == LoopOutcome::Escalate {
                        out.attention_flagged += 1;
                        tracing::warn!(
                            event = "spawn.loop_escalated",
                            number = task.number(),
                            slot = naming::slot_key(slot),
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
                                         прогресса статуса ({:?}). Строка переведена в \
                                         Needs attention — разбор владельца.",
                                        naming::slot_key(slot),
                                        task.status()
                                    ),
                                )
                                .await;
                        }
                        continue;
                    }
                    let gated = gate(task, &action, plan.policy, global_active, now);
                    match gated {
                        Gate::Allowed => {
                            out.spawns_allowed += 1;
                            if plan.mode == Mode::Live {
                                match self.spawn_stage(board, task, slot).await {
                                    Ok(session) => {
                                        // счётчик/кулдаун пишем только при
                                        // реальном спавне: observe не накручивает
                                        // петли (#1046), а временный отказ
                                        // (бюджет 10/ч) — не петля и не спавн
                                        // (15.11: #1151 high ложно эскалировала
                                        // бы за 3 бюджет-отказа)
                                        self.state.set_spawn_loop(
                                            &board.board,
                                            task.number(),
                                            slot,
                                            guard.status(),
                                            guard.repeats(),
                                        );
                                        self.state.mark_spawn(
                                            &board.board,
                                            task.number(),
                                            slot,
                                            self.clock.now_ms(),
                                        );
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
                                            self.log_action_failed(task_ref, e);
                                        } else {
                                            out.spawns_done += 1;
                                        }
                                    }
                                    Err(e) => {
                                        out.actions_failed += 1;
                                        self.log_action_failed(task_ref, e);
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
                            Ok(false) => match self.stall_pass(board, task).await {
                                Ok(StallAct::None) => {}
                                Ok(StallAct::Pinged) => out.stall_pings += 1,
                                Ok(StallAct::Flagged) => out.stall_flagged += 1,
                                Err(e) => {
                                    out.actions_failed += 1;
                                    self.log_action_failed(task_ref, e);
                                }
                            },
                            Err(e) => {
                                out.actions_failed += 1;
                                self.log_action_failed(task_ref, e);
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

    /// Сталл-детект: stage-сессия жива, но молчит дольше STALL_SILENCE_MS —
    /// ход завершён без статусного перевода (паттерн #1151/#1135/#384).
    /// Пинг агенту → повтор → Needs attention. Будит через механизм
    /// wakeup (пинг = коммент, следующий тик доставит его сессии).
    async fn stall_pass(&self, board: &BoardPlan, task: &Task) -> UsecaseResult<StallAct> {
        let Some(slot) = task.stage_slot() else {
            return Ok(StallAct::None);
        };
        let Some(session) = task.session_of(slot) else {
            return Ok(StallAct::None);
        };
        let session = session.to_string();
        // нет данных/ошибка чтения — не сталл (мёртвую сессию заберёт decide)
        let Ok(Some(last_ms)) = self.sessions.last_activity_ms(&session).await else {
            return Ok(StallAct::None);
        };
        let now = self.clock.now_ms();
        let silence = now.saturating_sub(last_ms);
        if silence < STALL_SILENCE_MS {
            return Ok(StallAct::None);
        }
        // раунд = статус: смена статуса обнуляет счётчик пингов
        let (prev_status, last_ping, mut count) = self
            .state
            .stall_ping(&board.board, task.number())
            .map(|(st, ms, c)| (Some(st), ms, c))
            .unwrap_or((None, 0, 0));
        if prev_status != Some(task.status()) {
            count = 0;
        }
        if count >= STALL_MAX_PINGS && now.saturating_sub(last_ping) >= STALL_PING_EVERY_MS {
            // два пинга исчерпаны, тишина продолжается — мяч владельцу
            tracing::warn!(
                event = "stall.escalated",
                number = task.number(),
                silence_min = silence / 60_000,
                "Сталл: сессия молчит после двух автопингов"
            );
            self.board
                .set_status(&board.board, task.number(), Status::NeedsAttention)
                .await?;
            self.board
                .post_comment(
                    task.number(),
                    &format!(
                        "⚠︎ Сталл-детект: stage-сессия `{}` молчит {} мин после двух \
                         автопингов — агент не завершает статусную транзакцию. Строка в \
                         Needs attention, разбор владельца.",
                        naming::slot_key(slot),
                        silence / 60_000
                    ),
                )
                .await?;
            self.state
                .mark_stall_ping(&board.board, task.number(), task.status(), now);
            return Ok(StallAct::Flagged);
        }
        if count > 0 && now.saturating_sub(last_ping) < STALL_PING_EVERY_MS {
            return Ok(StallAct::None);
        }
        tracing::info!(
            event = "stall.ping",
            number = task.number(),
            silence_min = silence / 60_000,
            "Сталл: автопинг зависшей стадии"
        );
        self.board
            .post_comment(
                task.number(),
                &format!(
                    "⏰ Автопинг сталл-детекта: сессия стадии `{}` молчит {} мин при статусе \
                     {:?} — ход, похоже, завершён без перевода статуса. Заверши транзакцию: \
                     переведи статус по канону пайпа (или продолжай работу и доведи её до \
                     статусного перевода).",
                    naming::slot_key(slot),
                    silence / 60_000,
                    task.status()
                ),
            )
            .await?;
        self.state
            .mark_stall_ping(&board.board, task.number(), task.status(), now);
        Ok(StallAct::Pinged)
    }

    fn log_action_failed(&self, task: &Task, e: UsecaseError) -> UsecaseError {
        tracing::error!(event = "action.failed", number = task.number(), error = %e, "Action failed");
        e
    }
}
