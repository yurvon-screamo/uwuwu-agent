//! Общие стабы тест-фикстур поверхности MCP (tools_tests, tools_pins_tests):
//! вики/сессии/state в армах диспетчера не участвуют — no-op-реализации
//! поверх MemoryHub-носителя задач. Раньше дублировались байт-в-байт в
//! обоих тест-файлах (identical-code smell, раунд #1256).

use async_trait::async_trait;
use std::sync::Arc;
use uwuwu_domain::{Slot, Status};
use uwuwu_usecase::{
    AgentSessionGateway, AppendDocOutcome, DreamerMark, Liveness, SessionInfo, SessionSpec,
    TaskView, TaskWriteUseCase, TickStateStore, UsecaseResult, WikiRepo,
};

use crate::clock::SystemClock;
use crate::mockhub::MemoryHub;

use super::tools::ToolHub;

/// Вики в пробах диспетчера не читается: доков нет.
pub(crate) struct StubWiki;

/// Blob-канал в пробах диспетчера: memory-стор (#1292) — полный путь
/// place→presign без S3, как прод-контур, но без сети.
pub(crate) fn blob_usecase() -> Arc<uwuwu_usecase::BlobUseCase> {
    Arc::new(uwuwu_usecase::BlobUseCase::ready(Arc::new(
        crate::s3::MemoryBlobStore::default(),
    )))
}

/// Blob-канал «не сконфигурирован»: громкий отказ с перечнем (ADR-010).
pub(crate) fn blob_usecase_missing(names: &[&str]) -> Arc<uwuwu_usecase::BlobUseCase> {
    Arc::new(uwuwu_usecase::BlobUseCase::missing(
        names.iter().map(|name| (*name).to_string()).collect(),
    ))
}

/// Фикстура ToolHub на MemoryHub-носителе + стабах сессий/state. Вики и
/// blob-бэкенд — параметры: pins гоняют docs_* на memory-вики, диспетчерные
/// пробы — на StubWiki; blob — memory для happy-path, missing для отказов
/// (четыре точки сборки — одна фабрика, ревью #1292).
pub(crate) fn tool_hub(
    wiki: Arc<dyn WikiRepo>,
    blobs: Arc<uwuwu_usecase::BlobUseCase>,
    crons_state: Arc<dyn uwuwu_usecase::CronStateStore>,
) -> (Arc<ToolHub>, Arc<MemoryHub>) {
    let memory = Arc::new(MemoryHub::from_env());
    let board: Arc<dyn uwuwu_usecase::TaskBoardRepo> = memory.clone();
    tool_hub_on_board(wiki, blobs, crons_state, board, memory.clone())
}

/// Та же сборка с подменённой доской (пин квитанции Unverified #1437,
/// тест-ревью C2): tasks/writer смотрят на поданный носитель, остальные
/// армы — на aux. Исход set_status фейка доходит до поверхности MCP
/// сквозь настоящий TaskWriteUseCase; возвращается aux — носитель
/// остальных армов (при дефолтной сборке он же и доска, как прежде).
pub(crate) fn tool_hub_on_board(
    wiki: Arc<dyn WikiRepo>,
    blobs: Arc<uwuwu_usecase::BlobUseCase>,
    crons_state: Arc<dyn uwuwu_usecase::CronStateStore>,
    board: Arc<dyn uwuwu_usecase::TaskBoardRepo>,
    aux: Arc<MemoryHub>,
) -> (Arc<ToolHub>, Arc<MemoryHub>) {
    let fields: Arc<dyn uwuwu_usecase::IssueFieldStore> = aux.clone();
    let clean_repo: Arc<dyn uwuwu_usecase::CleanBoardRepo> = aux.clone();
    let tools = Arc::new(ToolHub {
        tasks: Arc::new(TaskView::new(
            board.clone(),
            vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())],
        )),
        goals: Arc::new(uwuwu_usecase::GoalsView::new(
            aux.clone(),
            vec!["uwuwu".to_string()],
            "https://github.com/acme/hub/milestones".into(),
        )),
        docs: Arc::new(uwuwu_usecase::DocsView::new(wiki.clone())),
        person: Arc::new(uwuwu_usecase::PersonView::new(wiki.clone())),
        writer: Arc::new(TaskWriteUseCase {
            board: board.clone(),
            fields,
            boards: vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())],
            clock: Arc::new(SystemClock),
            state: Arc::new(StubState),
        }),
        clean: Arc::new(uwuwu_usecase::TaskCleanUseCase {
            clean: clean_repo,
            journals: Arc::new(StubJournals::default()),
            boards: vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())],
            clock: Arc::new(SystemClock),
        }),
        status: Arc::new(uwuwu_usecase::BoardStatusRenameUseCase {
            repo: aux.clone() as Arc<dyn uwuwu_usecase::BoardStatusRepo>,
            journal: Arc::new(StubPreImages),
            boards: vec![("uwuwu".to_string(), "Задачи uwuwu".to_string())],
            clock: Arc::new(SystemClock),
        }),
        wiki: wiki.clone(),
        sessions: Arc::new(uwuwu_usecase::SessionRouter::uniform(Arc::new(
            StubSessions,
        ))),
        search: Arc::new(uwuwu_usecase::SearchUseCase { semantic: None }),
        blobs,
        crons: Arc::new(uwuwu_usecase::CronUseCase::new(
            wiki,
            Arc::new(uwuwu_usecase::SessionRouter::uniform(Arc::new(
                StubSessions,
            ))),
            Arc::new(SystemClock),
            Arc::new(uwuwu_usecase::SpawnModels::canonical()),
            Default::default(),
            7,
            crons_state,
            None,
        )),
    });
    (tools, aux)
}

#[async_trait]
impl WikiRepo for StubWiki {
    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(vec![])
    }
    async fn read_doc(&self, _slug: &str) -> UsecaseResult<Option<String>> {
        Ok(None)
    }
    async fn list_docs(&self, _prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(vec![])
    }
    async fn write_doc(&self, _: &str, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn append_doc(&self, _: &str, _: &str, _: &str) -> UsecaseResult<AppendDocOutcome> {
        Ok(AppendDocOutcome {
            created: false,
            lines: 0,
        })
    }
}

/// Undo-журналы в армах диспетчера: память на время пробы, персистентность
/// не проверяется (юзкейс-пины clean.rs покрывают контракт журнала).
pub(crate) struct StubJournals {
    entries: std::sync::Mutex<std::collections::BTreeMap<String, uwuwu_usecase::CleanJournal>>,
}

/// Pre-image журнал rename (#1447) в армах диспетчера: память на время
/// пробы (персистентность покрывает пин rename_journal).
pub(crate) struct StubPreImages;

impl uwuwu_usecase::StatusPreImageStore for StubPreImages {
    fn put(&self, _image: &uwuwu_usecase::StatusPreImage) -> UsecaseResult<()> {
        Ok(())
    }
    fn path_hint(&self) -> String {
        "/memory/rename-pre-image.json".into()
    }
}

impl Default for StubJournals {
    fn default() -> Self {
        Self {
            entries: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }
}

impl uwuwu_usecase::CleanJournalStore for StubJournals {
    fn put(&self, journal: &uwuwu_usecase::CleanJournal) -> UsecaseResult<()> {
        self.entries
            .lock()
            .expect("stub journals")
            .insert(journal.id.clone(), journal.clone());
        Ok(())
    }
    fn get(
        &self,
        project: &str,
        id: Option<&str>,
    ) -> UsecaseResult<Option<uwuwu_usecase::CleanJournal>> {
        let guard = self.entries.lock().expect("stub journals");
        match id {
            Some(id) => Ok(guard.get(id).filter(|j| j.project == project).cloned()),
            None => Ok(guard
                .values()
                .filter(|j| j.project == project)
                .max_by(|a, b| {
                    a.created_ms
                        .cmp(&b.created_ms)
                        .then_with(|| a.id.cmp(&b.id))
                })
                .cloned()),
        }
    }
    fn ids(&self, project: &str) -> UsecaseResult<Vec<String>> {
        let mut by_age: Vec<(u64, String)> = self
            .entries
            .lock()
            .expect("stub journals")
            .values()
            .filter(|j| j.project == project)
            .map(|j| (j.created_ms, j.id.clone()))
            .collect();
        by_age.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        Ok(by_age.into_iter().map(|(_, id)| id).collect())
    }
}

/// Сессии в пробах не вызываются: гварды армов стреляют до gateway.
pub(crate) struct StubSessions;

/// Учёт кронов в пробах: seeded-ряды для рендер-проб cron_list
/// (пустое состояние — дефолт: все кроны «never»).
#[derive(Default)]
pub(crate) struct StubCronState {
    rows: std::sync::Mutex<std::collections::HashMap<String, uwuwu_usecase::CronRow>>,
}

impl StubCronState {
    /// Сидирует ряд учёта крона (имя крона — средний сегмент слага).
    pub(crate) fn with_row(self, name: &str, row: uwuwu_usecase::CronRow) -> Self {
        self.rows
            .lock()
            .expect("stub state lock")
            .insert(name.to_string(), row);
        self
    }
}

impl uwuwu_usecase::CronStateStore for StubCronState {
    fn cron_row(&self, name: &str) -> uwuwu_usecase::CronRow {
        self.rows
            .lock()
            .expect("stub state lock")
            .get(name)
            .cloned()
            .unwrap_or_default()
    }
    fn set_cron_row(&self, name: &str, row: uwuwu_usecase::CronRow) {
        self.rows
            .lock()
            .expect("stub state lock")
            .insert(name.to_string(), row);
    }
    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}

#[async_trait]
impl AgentSessionGateway for StubSessions {
    async fn create_session(&self, _: &SessionSpec) -> UsecaseResult<String> {
        Ok("ses_stub".into())
    }
    async fn prompt(&self, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn run_command(&self, _: &str, _: &str, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, _: &str) -> Liveness {
        Liveness::Unknown
    }
    async fn delete_session(&self, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn last_activity_ms(&self, _: &str) -> UsecaseResult<Option<u64>> {
        Ok(None)
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        Ok(vec![])
    }
    async fn export_session(&self, _: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
    async fn last_assistant_reply(
        &self,
        _: &str,
    ) -> UsecaseResult<Option<uwuwu_usecase::AssistantReply>> {
        Ok(None)
    }
}

/// No-op TickStateStore: писателю нужен факт вызова mark_own_comment,
/// состояние в пробах не проверяется.
pub(crate) struct StubState;

impl TickStateStore for StubState {
    fn last_comment_id(&self, _: &str, _: u64) -> Option<String> {
        None
    }
    fn last_seen_status(&self, _: &str, _: u64) -> Option<Status> {
        None
    }
    fn set_last_seen_status(&self, _: &str, _: u64, _: Status) {}
    fn set_last_comment_id(&self, _: &str, _: u64, _: &str) {}
    fn dreamer_mark(&self, _: &str) -> Option<DreamerMark> {
        None
    }
    fn mark_dreamer(&self, _: &str, _: DreamerMark) {}
    fn dreamer_session(&self, _: &str) -> Option<String> {
        None
    }
    fn link_dreamer_session(&self, _: &str, _: &str) {}
    fn prune_dreamer(&self, _: &std::collections::HashSet<String>) {}
    fn dreamer_day_spawns(&self, _: &str) -> u32 {
        0
    }
    fn mark_dreamer_spawn(&self, _: &str) {}
    fn last_spawn_ms(&self, _: &str, _: u64, _: Slot) -> Option<u64> {
        None
    }
    fn mark_spawn(&self, _: &str, _: u64, _: Slot, _: u64) {}
    fn clear_spawn_ms(&self, _: &str, _: u64, _: Slot) {}
    fn last_analys_ms(&self, _: &str, _: u64) -> Option<u64> {
        None
    }
    fn mark_analys_spawn(&self, _: &str, _: u64, _: u64) {}
    fn last_analys_review_ms(&self, _: &str, _: u64) -> Option<u64> {
        None
    }
    fn mark_analys_review_spawn(&self, _: &str, _: u64, _: u64) {}
    fn analys_loop(&self, _: &str, _: u64) -> Option<(Status, u32)> {
        None
    }
    fn set_analys_loop(&self, _: &str, _: u64, _: Status, _: u32) {}
    fn clear_analys_loop(&self, _: &str, _: u64) {}
    fn analys_review_loop(&self, _: &str, _: u64) -> Option<(Status, u32)> {
        None
    }
    fn set_analys_review_loop(&self, _: &str, _: u64, _: Status, _: u32) {}
    fn clear_analys_review_loop(&self, _: &str, _: u64) {}
    fn prune_analys_guards(&self, _: &std::collections::HashSet<String>) {}
    fn stall_ping(&self, _: &str, _: u64) -> Option<(Status, u64, u32)> {
        None
    }
    fn mark_stall_ping(&self, _: &str, _: u64, _: Status, _: u64) {}
    fn clear_stall(&self, _: &str, _: u64) {}
    fn lost_session(&self, _: &str, _: u64) -> Option<(Status, u32)> {
        None
    }
    fn mark_lost_session(&self, _: &str, _: u64, _: Status) {}
    fn clear_lost_session(&self, _: &str, _: u64) {}
    fn spawn_loop(&self, _: &str, _: u64, _: Slot) -> Option<(Status, u32)> {
        None
    }
    fn set_spawn_loop(&self, _: &str, _: u64, _: Slot, _: Status, _: u32) {}
    fn clear_spawn_loop(&self, _: &str, _: u64, _: Slot) {}
    fn reset_stale_spawn_loops(&self, _: &str, _: u64, _: Status) {}
    fn review_rounds(&self, _: &str, _: u64) -> u32 {
        0
    }
    fn set_review_rounds(&self, _: &str, _: u64, _: u32) {}
    fn own_comment(&self, _: &str) -> bool {
        false
    }
    fn mark_own_comment(&self, _: &str, _: u64) {}
    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}
