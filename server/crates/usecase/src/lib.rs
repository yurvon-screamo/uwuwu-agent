//! uwuwu-usecase: оркестрация конвейера + инфра-порты + naming (маппинг
//! имён носителей). Структура крейта инкапсулирована: наружу — только
//! pub use из lib. Папки: pipeline/ (тик, действия, триаж, дример),
//! tasks/ (запись, вьюхи), crons/, search/.

pub mod blobs;
pub mod bots;
mod crons;
mod error;
pub mod fm_guard;
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
mod search_tests;
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
pub use crons::{
    cron_name, jst_stamp, parse_schedule_text, CronUseCase, FireOutcome, CRON_DIR,
    FIRE_SIGNAL_COOLDOWN_MS, RUN_SILENCE_MS,
};
pub use error::{UsecaseError, UsecaseResult, ViewError};
pub use goals::{gist_of, Goal, GoalsRepo, GoalsView};
pub use naming::{
    agent_name, parse_agent, parse_priority, parse_status, parse_task_type, priority_name,
    schedule_label, slot_key, status_name, status_names, task_type_name, task_type_native_name,
    F_AUTOMERGE, F_DIRECT, F_PRIORITY, TASK_TYPES,
};
pub use pipeline::env_gc;
pub use pipeline::sweeper;
pub use pipeline::{
    executor::ExecutorLoudness, executor::EXECUTOR_UNAVAILABLE_COOLDOWN_MS,
    executor::EXECUTOR_UNAVAILABLE_THRESHOLD, executor::EXECUTOR_UNAVAILABLE_WINDOW_MS,
    incident::IncidentLoudness, incident::INCIDENT_COOLDOWN_MS, incident::INCIDENT_THRESHOLD,
    incident::INCIDENT_WINDOW_MS, AnalystBoard, AnalystReviewUseCase, AnalystUseCase, BoardPlan,
    DreamerUseCase, Mode, TickOutcome, TickPlan, TickUseCase, ANALYST_REVIEW_WAVE_MARKER,
    ANALYST_WAVE_MARKER, STAGE_PROMPTS,
};
pub use ports::{
    AgentSessionGateway, AppendDocOutcome, AssistantReply, Clock, CommentRef, CronRow,
    CronRunTrack, CronStateStore, DecisionJudge, DreamerMark, FmMutator, IssueDigest,
    IssueFieldStore, IssueIndexSource, IssueView, JudgeCfg, JudgeContext, JudgeSessionTail,
    Liveness, PrGateway, PrProbe, PrState, SessionInfo, SessionSpec, TaskBoardRepo, TickStateStore,
    WikiRepo,
};
pub use router::SessionRouter;
pub use search::{sync_allowed, SearchHit, SearchUseCase, SemanticBackend};
pub use spawn_models::SpawnModels;
pub use tasks::board_status::{
    BoardStatusRenameUseCase, BoardStatusRepo, RenameReport, StatusFieldImage, StatusOption,
    StatusPreImage, StatusPreImageStore, VERIFY_ATTEMPTS,
};
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
