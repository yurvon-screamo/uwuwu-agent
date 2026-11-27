//! Конвейер: тик (tick), действия (actions), аналитика (#1135), дример.

pub mod actions;
pub mod analyst;
pub mod analyst_review;
mod compact_pass;
#[cfg(test)]
mod cup_pins_tests;
#[cfg(test)]
pub(crate) mod degraded_list_support;
#[cfg(test)]
mod degraded_list_tests;
pub mod dreamer;
pub mod env_gc;
#[cfg(test)]
mod env_gc_tests;
pub mod executor;
pub mod incident;
#[cfg(test)]
mod prompt_carrier_tests;
#[cfg(test)]
mod spawn_carrier_tests;
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
