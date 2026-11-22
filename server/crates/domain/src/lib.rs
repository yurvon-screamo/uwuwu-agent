//! uwuwu-domain: агрегаты и инварианты контура uwuwu (hub#1133).
//! Самый внутренний слой: ноль зависимостей — ни инфры, ни юзкейсов,
//! ни имён носителей (маппинг имён — usecase::naming). Значения времени/
//! идентификаторов — value-типы здесь; источники (часы, сеть, ФС) —
//! порты юзкейс-слоя.

mod agent;
mod blob;
mod bot;
mod budget;
mod cron;
mod guard;
mod judge;
mod pipeline;
mod project;
mod task;
mod task_type;

pub use agent::{Agent, AgentSlot, ModelRef, SpawnModel};
pub use blob::BlobKey;
pub use bot::{BotAction, BotModel, BotSpec};
pub use budget::{BudgetVerdict, SpawnBudget};
pub use cron::{
    civil_from_days, jst_fields, CronField, CronMaster, CronSchedule, CronValues, IntervalMinutes,
    JstDate, JstTime, Schedule,
};
pub use guard::{
    sighting_is_live, sighting_known, spawn_guard, SessionSighting, SpawnGuard,
    SLOT_SESSION_LIVE_MS,
};
pub use project::{Goal, Persona, Project, ProjectName};
pub use task::Priority;
pub use task::{BranchName, PrUrl, SessionRef, Slot, Status, Task, TaskFields, Vcs};
pub use task_type::TaskType;

// внутренние виды расписания — параметр Schedule::new (парсер юзкейс-слоя)
pub use cron::ScheduleKind;
pub use judge::{judge_stage, judge_step, verdict_target, JudgeStatus, JudgeStep, JudgeVerdict};
pub use pipeline::{
    awaits, can_preempt, decide, gate, review_round_step, Action, Awaits, Gate, LoopOutcome,
    PeakWindow, ReviewRoundStep, SpawnLoop, SpawnPolicy, UtcMinute, REVIEW_ROUND_CAP,
};
