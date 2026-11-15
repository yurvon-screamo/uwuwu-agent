//! Конвейер: тик (tick), действия (actions), триаж, дример.

pub mod actions;
pub mod dreamer;
pub mod tick;
#[cfg(test)]
mod tick_tests;
pub mod triage;

pub use dreamer::DreamerUseCase;
pub use tick::{BoardPlan, Mode, TickOutcome, TickPlan, TickUseCase};
pub use triage::{TriageBoard, TriageUseCase};
