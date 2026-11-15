//! uwuwu-usecase: оркестрация конвейера + инфра-порты + naming (маппинг
//! имён носителей). Структура крейта инкапсулирована: наружу — только
//! pub use из lib. Папки: pipeline/ (тик, действия, триаж, дример),
//! tasks/ (запись, вьюхи), crons/, search/.

mod crons;
mod error;
mod fm_lite;
mod naming;
mod pipeline;
mod ports;
mod search;
mod tasks;

pub use crons::{crons_dir, parse_schedule_text, CronUseCase, FireOutcome};
pub use error::{UsecaseError, UsecaseResult};
pub use naming::{
    agent_name, parse_agent, parse_priority, parse_status, parse_task_type, priority_name,
    schedule_label, slot_key, status_name, task_type_name, TASK_TYPES,
};
pub use pipeline::{
    BoardPlan, DreamerUseCase, Mode, TickOutcome, TickPlan, TickUseCase, TriageBoard, TriageUseCase,
};
pub use ports::{
    AgentSessionGateway, Clock, CommentRef, FmMutator, IssueDigest, IssueFieldStore,
    IssueIndexSource, IssueView, Liveness, PrGateway, PrState, SessionInfo, SessionSpec,
    TaskBoardRepo, TickStateStore, WikiRepo,
};
pub use search::{sync_allowed, SearchHit, SearchUseCase, SemanticBackend};
pub use tasks::body_hash;
pub use tasks::views::{DocsView, PersonView, TaskView, ViewError};
pub use tasks::write::{
    BodyWriteReceipt, NewTask, SectionEditReport, TaskPatch, TaskWriteUseCase, TaskWriter,
};
