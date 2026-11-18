//! uwuwu-usecase: оркестрация конвейера + инфра-порты + naming (маппинг
//! имён носителей). Структура крейта инкапсулирована: наружу — только
//! pub use из lib. Папки: pipeline/ (тик, действия, триаж, дример),
//! tasks/ (запись, вьюхи), crons/, search/.

pub mod bots;
mod crons;
mod error;
mod fm_lite;
mod naming;
mod pipeline;
mod ports;
mod router;
#[cfg(test)]
mod router_tests;
mod search;
mod tasks;

pub use bots::{
    build_prompt, chunk_reply, classify_message, parse_bot_doc, plan_action, BotStateStore,
    TelegramGateway, TgMessage, TgUpdate,
};
pub use crons::{crons_dir, parse_schedule_text, CronUseCase, FireOutcome};
pub use error::{UsecaseError, UsecaseResult};
pub use naming::{
    agent_name, parse_agent, parse_priority, parse_status, parse_task_type, priority_name,
    schedule_label, slot_key, status_name, task_type_name, F_AUTOMERGE, F_PRIORITY, TASK_TYPES,
};
pub use pipeline::{
    incident::IncidentLoudness, incident::STALL_INCIDENT_COOLDOWN_MS,
    incident::STALL_INCIDENT_THRESHOLD, incident::STALL_INCIDENT_WINDOW_MS, AnalystBoard,
    AnalystReviewUseCase, AnalystUseCase, BoardPlan, DreamerUseCase, Mode, TickOutcome, TickPlan,
    TickUseCase,
};
pub use ports::{
    AgentSessionGateway, AssistantReply, Clock, CommentRef, FmMutator, IssueDigest,
    IssueFieldStore, IssueIndexSource, IssueView, Liveness, PrGateway, PrState, SessionInfo,
    SessionSpec, TaskBoardRepo, TickStateStore, WikiRepo,
};
pub use router::SessionRouter;
pub use search::{sync_allowed, SearchHit, SearchUseCase, SemanticBackend};
pub use tasks::body_hash;
pub use tasks::views::{DocsView, PersonView, TaskView, ViewError};
pub use tasks::write::{
    BodyWriteReceipt, NewTask, SectionAction, SectionEditReport, TaskPatch, TaskWriteUseCase,
    TaskWriter,
};
