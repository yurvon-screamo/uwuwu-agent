//! uwuwu-usecase: оркестрация доменом, порты инфры (трейты).
//! Слой не знает ни MCP, ни gh, ни git, ни HTTP — только доменные типы
//! и требования к инфре (порты). Реализации приезжают из uwuwu-server.

pub mod actions;
pub mod crons;
pub mod dreamer;
pub mod error;
pub mod fm_lite;
pub mod ports;
pub mod search;
pub mod taskwrite;
pub mod tick;
#[cfg(test)]
mod tick_tests;
pub mod triage;
pub mod views;

pub use error::{UsecaseError, UsecaseResult};
pub use ports::SessionInfo;
pub use ports::{
    AgentSessionGateway, Clock, CommentRef, FmMutator, IssueView, Liveness, PrGateway, PrState,
    SessionSpec, TaskBoardRepo, TickStateStore, WikiRepo,
};
pub use taskwrite::{NewTask, TaskPatch, TaskWriteUseCase, TaskWriter};
pub use tick::{BoardPlan, Mode, TickOutcome, TickPlan, TickUseCase};
pub use views::{DocsView, PersonView, TaskView, ViewError};
