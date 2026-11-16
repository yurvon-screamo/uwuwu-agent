//! Контракт спавна «модель — из декларации агента» (#1154): ни один
//! спавн конвейера (триаж/дример/кроны) не кладёт модель в SessionSpec —
//! иначе гейтвей opencode применяет body.model поверх дефа агента.
//! Проводка сервера модель не задаёт вовсе; тесты — регресс-гард против
//! возврата подстановки. Тик-спавны покрыты в pipeline/tick_tests.rs.

use crate::crons::CronUseCase;
use crate::error::UsecaseResult;
use crate::pipeline::dreamer::DreamerUseCase;
use crate::pipeline::triage::{TriageBoard, TriageUseCase};
use crate::ports::{
    AgentSessionGateway, AssistantReply, Clock, CommentRef, IssueView, Liveness, SessionInfo,
    SessionSpec, TaskBoardRepo, TickStateStore, WikiRepo,
};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use uwuwu_domain::{Slot, Status, Task};

/// Захватывающий шлюз: спеки create_session + выдача list_sessions.
struct CaptureSessions {
    specs: Mutex<Vec<SessionSpec>>,
    listed: Vec<SessionInfo>,
}

impl CaptureSessions {
    fn new(listed: Vec<SessionInfo>) -> Arc<Self> {
        Arc::new(Self {
            specs: Mutex::default(),
            listed,
        })
    }

    fn spec_of(&self) -> Option<SessionSpec> {
        self.specs.lock().unwrap().first().cloned()
    }
}

#[async_trait]
impl AgentSessionGateway for CaptureSessions {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String> {
        self.specs.lock().unwrap().push(spec.clone());
        Ok("ses_new".into())
    }
    async fn prompt(&self, _session: &str, _text: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn run_command(&self, _s: &str, _n: &str, _a: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, _s: &str) -> Liveness {
        Liveness::Alive
    }
    async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
        Ok(Some(1_000))
    }
    async fn last_assistant_reply(&self, _s: &str) -> UsecaseResult<Option<AssistantReply>> {
        Ok(None)
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        Ok(self.listed.clone())
    }
    async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
}

/// Текущее время: 40 минут; дример-idle порог — 30 минут.
struct TestClock;
impl Clock for TestClock {
    fn utc_minute(&self) -> uwuwu_domain::UtcMinute {
        uwuwu_domain::UtcMinute {
            minute_of_day: 100,
            weekday: 3,
        }
    }
    fn now_ms(&self) -> u64 {
        40 * 60_000
    }
}

/// Минимальный стейт: контракту спавна нужны только дефолты (None/false/0).
#[derive(Default)]
struct NoopState;

impl TickStateStore for NoopState {
    fn last_comment_id(&self, _b: &str, _n: u64) -> Option<String> {
        None
    }
    fn last_seen_status(&self, _b: &str, _n: u64) -> Option<Status> {
        None
    }
    fn set_last_seen_status(&self, _b: &str, _n: u64, _s: Status) {}
    fn set_last_comment_id(&self, _b: &str, _n: u64, _id: &str) {}
    fn dreamer_done(&self, _s: &str) -> bool {
        false
    }
    fn mark_dreamer_done(&self, _s: &str) {}
    fn dreamer_day_spawns(&self, _d: &str) -> u32 {
        0
    }
    fn mark_dreamer_spawn(&self, _d: &str) {}
    fn last_spawn_ms(&self, _b: &str, _n: u64, _s: Slot) -> Option<u64> {
        None
    }
    fn mark_spawn(&self, _b: &str, _n: u64, _s: Slot, _at: u64) {}
    fn stall_ping(&self, _b: &str, _n: u64) -> Option<(Status, u64, u32)> {
        None
    }
    fn mark_stall_ping(&self, _b: &str, _n: u64, _s: Status, _at: u64) {}
    fn clear_spawn_ms(&self, _b: &str, _n: u64, _s: Slot) {}
    fn last_triage_ms(&self, _b: &str, _n: u64) -> Option<u64> {
        None
    }
    fn mark_triage_spawn(&self, _b: &str, _n: u64, _at: u64) {}
    fn spawn_loop(&self, _b: &str, _n: u64, _s: Slot) -> Option<(Status, u32)> {
        None
    }
    fn set_spawn_loop(&self, _b: &str, _n: u64, _s: Slot, _st: Status, _r: u32) {}
    fn reset_stale_spawn_loops(&self, _b: &str, _n: u64, _c: Status) {}
    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}

/// Доска с одной строкой (триаж: Draft).
struct DraftBoard;

#[async_trait]
impl TaskBoardRepo for DraftBoard {
    async fn read_board(&self, _b: &str) -> UsecaseResult<Vec<Task>> {
        Ok(vec![Task::new(7, "черновик".into(), Status::Draft)])
    }
    async fn set_status(&self, _b: &str, _n: u64, _s: Status) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_session(&self, _b: &str, _n: u64, _s: Slot, _sess: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn clear_session(&self, _b: &str, _n: u64, _s: Slot) -> UsecaseResult<()> {
        Ok(())
    }
    async fn post_comment(&self, _n: u64, _body: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn close_issue(&self, _n: u64) -> UsecaseResult<()> {
        Ok(())
    }
    async fn latest_comments(&self, _n: u64) -> UsecaseResult<Vec<CommentRef>> {
        Ok(vec![])
    }
    async fn read_issue(&self, _b: &str, n: u64) -> UsecaseResult<IssueView> {
        Ok(IssueView {
            task: Task::new(n, String::new(), Status::Empty),
            body: String::new(),
            comments: vec![],
        })
    }
    async fn issue_fields(&self, _n: u64) -> UsecaseResult<uwuwu_domain::TaskFields> {
        Ok(Default::default())
    }
    async fn create_issue(&self, _b: &str, _p: &str, _t: &str, _body: &str) -> UsecaseResult<u64> {
        Ok(100)
    }
    async fn rename_issue(&self, _n: u64, _t: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn patch_cells(&self, _n: u64, _m: crate::ports::FmMutator) -> UsecaseResult<()> {
        Ok(())
    }
    async fn close_issue_ext(&self, _n: u64, _np: bool) -> UsecaseResult<()> {
        Ok(())
    }
    async fn replace_body(&self, _n: u64, _b: &str) -> UsecaseResult<()> {
        Ok(())
    }
}

/// Вики: один крон-мастер с расписанием 5m; журнал пуст → ран due.
struct CronWiki;

#[async_trait]
impl WikiRepo for CronWiki {
    async fn read_doc(&self, slug: &str) -> UsecaseResult<Option<String>> {
        Ok(match slug {
            "Projects/uwuwu/Кроны/Тест" => {
                Some("---\ntitle: Тест\nschedule: 5m\n---\n\nТело".into())
            }
            _ => None,
        })
    }
    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(if prefix.contains("Кроны") {
            vec!["Projects/uwuwu/Кроны/Тест".into()]
        } else {
            vec![]
        })
    }
    async fn doc_shas(&self) -> UsecaseResult<Vec<(String, String)>> {
        Ok(vec![])
    }
    async fn write_doc(&self, _s: &str, _c: &str, _m: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn append_doc(&self, _s: &str, _l: &str, _m: &str) -> UsecaseResult<()> {
        Ok(())
    }
}

fn assert_no_model(spec: Option<SessionSpec>, what: &str) {
    let spec = spec.expect(what);
    assert!(
        spec.model.is_none(),
        "{what}: конфиг-модель не должна попадать в спавн: {:?}",
        spec.model
    );
}

#[tokio::test]
async fn triage_spawn_passes_no_model() {
    // Arrange
    let sessions = CaptureSessions::new(vec![]);
    let u = TriageUseCase {
        board: Arc::new(DraftBoard),
        boards: vec![TriageBoard {
            project: "uwuwu".into(),
            board: "Задачи uwuwu".into(),
            checkout: "/tmp".into(),
        }],
        sessions: sessions.clone(),
        state: Arc::new(NoopState),
        clock: Arc::new(TestClock),
    };

    // Act
    let spawned = u.tick().await.unwrap();

    // Assert
    assert_eq!(spawned, 1, "триаж-спавн состоялся");
    assert_no_model(sessions.spec_of(), "триаж-спавн");
}

#[tokio::test]
async fn dreamer_spawn_passes_no_model() {
    // Arrange: уснувшая dev-сессия с финалом и реальной работой
    let sessions = CaptureSessions::new(vec![SessionInfo {
        id: "ses_dev1".into(),
        agent: "developer".into(),
        updated_ms: 0,
        outcome: "stop".into(),
        total_tokens: 42_000,
    }]);
    let u = DreamerUseCase {
        sessions: sessions.clone(),
        state: Arc::new(NoopState),
        clock: Arc::new(TestClock),
        checkout: "/tmp".into(),
    };

    // Act
    let spawned = u.tick().await.unwrap();

    // Assert
    assert_eq!(spawned, 1, "дример-спавн состоялся");
    assert_no_model(sessions.spec_of(), "дример-спавн");
}

#[tokio::test]
async fn cron_run_passes_no_model() {
    // Arrange
    let sessions = CaptureSessions::new(vec![]);
    let u = CronUseCase {
        wiki: Arc::new(CronWiki),
        sessions: sessions.clone(),
        clock: Arc::new(TestClock),
        checkout: "/tmp".into(),
    };

    // Act
    let fired = u.tick("uwuwu").await.unwrap();

    // Assert
    assert_eq!(fired.len(), 1, "крон-файр состоялся");
    assert_no_model(sessions.spec_of(), "крон-спавн");
}
