//! Конвейер: тик (tick), действия (actions), аналитика (#1135), дример.

pub mod actions;
pub mod analyst;
pub mod analyst_review;
pub mod dreamer;
pub mod executor;
pub mod incident;
pub mod tick;
#[cfg(test)]
pub(crate) mod tick_tests;

pub use analyst::{AnalystBoard, AnalystUseCase};
pub use analyst_review::AnalystReviewUseCase;
pub use dreamer::DreamerUseCase;
pub use executor::{
    ExecutorIncident, ExecutorLoudness, EXECUTOR_UNAVAILABLE_COOLDOWN_MS,
    EXECUTOR_UNAVAILABLE_THRESHOLD, EXECUTOR_UNAVAILABLE_WINDOW_MS,
};
pub use tick::{BoardPlan, Mode, TickOutcome, TickPlan, TickUseCase};
