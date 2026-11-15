//! uwuwu-domain: агрегаты и инварианты контура uwuwu (hub#1133).
//! Самый внутренний слой: ноль зависимостей — ни инфры, ни юзкейсов.
//! Значения времени/идентификаторов — value-типы здесь; источники
//! (часы, сеть, ФС) — порты юзкейс-слоя.

pub mod agent;
pub mod budget;
pub mod cron;
pub mod pipeline;
pub mod task;
pub mod task_type;

pub use agent::{Agent, ModelRef};
pub use budget::{BudgetVerdict, SpawnBudget};
pub use cron::{civil_from_days, jst_fields, jst_stamp, CronMaster, Schedule};
pub use pipeline::{
    awaits, decide, gate, Action, Awaits, Gate, LoopOutcome, PeakWindow, ReviewRounds,
    RoundOutcome, SpawnLoop, SpawnPolicy, UtcMinute,
};
pub use task::{BranchName, PrUrl, Priority, SessionRef, Slot, Status, Task, TaskFields};
pub use task_type::TaskType;
