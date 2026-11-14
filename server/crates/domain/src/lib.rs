//! uwuwu-domain: агрегаты и инварианты контура uwuwu (hub#1133).
//! Самый внутренний слой: ноль зависимостей — ни инфры, ни юзкейсов.
//! Значения времени/идентификаторов — value-типы здесь; источники
//! (часы, сеть, ФС) — порты юзкейс-слоя.

pub mod pipeline;
pub mod task;

pub use pipeline::{
    awaits, decide, gate, Action, Awaits, Gate, PeakWindow, ReviewRounds, RoundOutcome,
    SpawnPolicy, UtcMinute,
};
pub use task::{Priority, Slot, Status, Task};
