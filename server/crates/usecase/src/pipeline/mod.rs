//! Конвейер: тик (tick), действия (actions), аналитика (#1135), дример.

pub mod actions;
pub mod analyst;
pub mod analyst_review;
pub mod dreamer;
pub mod executor;
pub mod incident;
pub mod sweeper;
#[cfg(test)]
mod sweeper_tests;
pub mod tick;
#[cfg(test)]
pub(crate) mod tick_tests;

pub use actions::STAGE_PROMPTS;
pub use analyst::{AnalystBoard, AnalystUseCase, ANALYST_WAVE_MARKER};
pub use analyst_review::{AnalystReviewUseCase, ANALYST_REVIEW_WAVE_MARKER};
pub use dreamer::DreamerUseCase;
pub use tick::{BoardPlan, Mode, TickOutcome, TickPlan, TickUseCase};
