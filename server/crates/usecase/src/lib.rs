//! uwuwu-usecase: оркестрация доменом, порты инфры (трейты).
//! Слой не знает ни MCP, ни gh, ни git, ни HTTP — только доменные типы
//! и требования к инфре (порты). Реализации приезжают из uwuwu-server.

pub mod actions;
pub mod error;
pub mod ports;
pub mod tick;
#[cfg(test)]
mod tick_tests;

pub use error::{UsecaseError, UsecaseResult};
pub use ports::{
    AgentSessionGateway, Clock, CommentRef, Liveness, PrGateway, PrState, SessionSpec,
    TaskBoardRepo, TickStateStore,
};
pub use tick::{BoardPlan, Mode, TickOutcome, TickPlan, TickUseCase};
