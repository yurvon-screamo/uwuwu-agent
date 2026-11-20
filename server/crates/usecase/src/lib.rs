//! uwuwu-usecase: оркестрация конвейера + инфра-порты + naming (маппинг
//! имён носителей). Структура крейта инкапсулирована: наружу — только
//! pub use из lib. Папки: pipeline/ (тик, действия, триаж, дример),
//! tasks/ (запись, вьюхи), crons/, search/.

pub mod blobs;
pub mod bots;
mod crons;
mod error;
mod fm_lite;
pub mod goals;
mod naming;
mod pipeline;
mod ports;
mod router;
#[cfg(test)]
mod router_tests;
mod search;
#[cfg(test)]
mod spawn_model_tests;
pub mod spawn_models;
mod tasks;

pub use blobs::{
    BlobPlace, BlobPutReceipt, BlobStore, BlobUseCase, BLOB_MAX_BYTES, BLOB_PRESIGN_TTL_SECS,
};
pub use bots::{
    build_prompt, chunk_reply, classify_message, parse_bot_doc, plan_action, BotStateStore,
    TelegramGateway, TgMessage, TgUpdate,
};
pub use crons::{parse_schedule_text, CronUseCase, FireOutcome, CRON_DIR};
pub use error::{UsecaseError, UsecaseResult, ViewError};
pub use goals::{gist_of, Goal, GoalsRepo, GoalsView};
pub use naming::{
    agent_name, parse_agent, parse_priority, parse_status, parse_task_type, priority_name,
    schedule_label, slot_key, status_name, status_names, task_type_name, F_AUTOMERGE, F_DIRECT,
    F_PRIORITY, TASK_TYPES,
};
pub use pipeline::sweeper;
pub use pipeline::{
    executor::ExecutorLoudness, executor::EXECUTOR_UNAVAILABLE_COOLDOWN_MS,
    executor::EXECUTOR_UNAVAILABLE_THRESHOLD, executor::EXECUTOR_UNAVAILABLE_WINDOW_MS,
    incident::IncidentLoudness, incident::INCIDENT_COOLDOWN_MS, incident::INCIDENT_THRESHOLD,
    incident::INCIDENT_WINDOW_MS, AnalystBoard, AnalystReviewUseCase, AnalystUseCase, BoardPlan,
    DreamerUseCase, Mode, TickOutcome, TickPlan, TickUseCase,
};
pub use ports::{
    AgentSessionGateway, AppendDocOutcome, AssistantReply, Clock, CommentRef, DreamerMark,
    FmMutator, IssueDigest, IssueFieldStore, IssueIndexSource, IssueView, Liveness, PrGateway,
    PrState, SessionInfo, SessionSpec, TaskBoardRepo, TickStateStore, WikiRepo,
};
pub use router::SessionRouter;
pub use search::{sync_allowed, SearchHit, SearchUseCase, SemanticBackend};
pub use spawn_models::SpawnModels;
pub use tasks::body_hash;
pub use tasks::clean::{
    CleanBoardRepo, CleanExecReport, CleanJournal, CleanJournalStore, CleanRow, CleanRowReport,
    CleanVerdict, IssueProbe, RowBinding, TaskCleanUseCase, UndoOutcome, UndoReport,
    CLEAN_BATCH_CAP,
};
pub use tasks::views::{DocsView, PersonView, TaskView};
pub use tasks::write::{
    BodyWriteReceipt, NewTask, SectionAction, SectionEditReport, TaskPatch, TaskWriteUseCase,
    TaskWriter,
};
