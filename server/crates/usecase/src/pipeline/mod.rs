//! Конвейер: тик (tick), действия (actions), аналитика (#1135), дример.

pub mod actions;
pub mod analyst;
pub mod analyst_review;
pub mod dreamer;
pub mod tick;
#[cfg(test)]
pub(crate) mod tick_tests;

pub use analyst::{AnalystBoard, AnalystUseCase};
pub use analyst_review::AnalystReviewUseCase;
pub use dreamer::DreamerUseCase;
pub use tick::{BoardPlan, Mode, TickOutcome, TickPlan, TickUseCase};
