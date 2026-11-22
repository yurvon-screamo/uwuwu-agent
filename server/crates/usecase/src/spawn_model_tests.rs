//! Контракт моделей спавна (#1154, решение владельца 17.11): модель —
//! карта сервера (`SpawnModels`), каждый спавн несёт её в теле сессии;
//! деф-файлы агентов моделей не несут. Тик-спавны покрыты в
//! pipeline/tick_tests.rs.

use crate::crons::CronUseCase;
use crate::error::UsecaseResult;
use crate::pipeline::analyst::{AnalystBoard, AnalystUseCase};
use crate::pipeline::analyst_review::AnalystReviewUseCase;
use crate::pipeline::dreamer::DreamerUseCase;
use crate::ports::AppendDocOutcome;
use crate::ports::{
    AgentSessionGateway, AssistantReply, Clock, CommentRef, DreamerMark, IssueView, Liveness,
    SessionInfo, SessionSpec, TaskBoardRepo, TickStateStore, WikiRepo,
};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use uwuwu_domain::{Slot, SpawnModel, Status, Task};

/// Канон-карта владельца (единственный механизм моделей — #1154).
fn models() -> Arc<SpawnModels> {
    Arc::new(SpawnModels::canonical())
}

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
    fn dreamer_mark(&self, _s: &str) -> Option<DreamerMark> {
        None
    }
    fn mark_dreamer(&self, _s: &str, _m: DreamerMark) {}
    fn dreamer_session(&self, _s: &str) -> Option<String> {
        None
    }
    fn link_dreamer_session(&self, _s: &str, _d: &str) {}
    fn prune_dreamer(&self, _live: &std::collections::HashSet<String>) {}
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
    fn clear_stall(&self, _b: &str, _n: u64) {}
    fn lost_session(&self, _: &str, _: u64) -> Option<(Status, u32)> {
        None
    }
    fn mark_lost_session(&self, _: &str, _: u64, _: Status) {}
    fn clear_lost_session(&self, _: &str, _: u64) {}
    fn clear_spawn_ms(&self, _b: &str, _n: u64, _s: Slot) {}
    /// Аналитика: кулдауны волн/ревью и цикл-гварды (#1135).
    fn last_analys_ms(&self, _b: &str, _n: u64) -> Option<u64> {
        None
    }
    fn mark_analys_spawn(&self, _b: &str, _n: u64, _at: u64) {}
    fn last_analys_review_ms(&self, _b: &str, _n: u64) -> Option<u64> {
        None
    }
    fn mark_analys_review_spawn(&self, _b: &str, _n: u64, _at: u64) {}
    fn analys_loop(&self, _b: &str, _n: u64) -> Option<(Status, u32)> {
        None
    }
    fn set_analys_loop(&self, _b: &str, _n: u64, _s: Status, _r: u32) {}
    fn clear_analys_loop(&self, _b: &str, _n: u64) {}
    fn analys_review_loop(&self, _b: &str, _n: u64) -> Option<(Status, u32)> {
        None
    }
    fn set_analys_review_loop(&self, _b: &str, _n: u64, _s: Status, _r: u32) {}
    fn clear_analys_review_loop(&self, _b: &str, _n: u64) {}
    fn prune_analys_guards(&self, _live: &std::collections::HashSet<String>) {}
    fn spawn_loop(&self, _b: &str, _n: u64, _s: Slot) -> Option<(Status, u32)> {
        None
    }
    fn set_spawn_loop(&self, _b: &str, _n: u64, _s: Slot, _st: Status, _r: u32) {}
    fn clear_spawn_loop(&self, _b: &str, _n: u64, _s: Slot) {}
    fn reset_stale_spawn_loops(&self, _b: &str, _n: u64, _c: Status) {}
    fn review_rounds(&self, _b: &str, _n: u64) -> u32 {
        0
    }
    fn set_review_rounds(&self, _b: &str, _n: u64, _r: u32) {}
    fn own_comment(&self, _id: &str) -> bool {
        false
    }
    fn mark_own_comment(&self, _id: &str, _posted_ms: u64) {}
    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}

/// Доска с заданными строками (фантом спавн-контрактов).
struct RowsBoard {
    rows: Vec<Task>,
}

#[async_trait]
impl TaskBoardRepo for RowsBoard {
    async fn read_board(&self, _b: &str) -> UsecaseResult<Vec<Task>> {
        Ok(self.rows.clone())
    }
    async fn set_status(
        &self,
        _b: &str,
        _n: u64,
        _s: Status,
    ) -> UsecaseResult<crate::ports::StatusDelivery> {
        Ok(crate::ports::StatusDelivery::Confirmed {
            actual: crate::naming::status_name(_s).to_string(),
        })
    }
    async fn row_status(&self, _b: &str, _n: u64) -> UsecaseResult<Option<Status>> {
        Ok(None)
    }
    async fn row_exists(&self, _b: &str, n: u64) -> UsecaseResult<bool> {
        Ok(self.rows.iter().any(|t| t.number() == n))
    }
    async fn set_session(&self, _b: &str, _n: u64, _s: Slot, _sess: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_board_cells(
        &self,
        _b: &str,
        _n: u64,
        _priority: Option<&str>,
        _automerge: Option<bool>,
        _direct: Option<bool>,
    ) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_branch(
        &self,
        _b: &str,
        _n: u64,
        _branch: &uwuwu_domain::BranchName,
    ) -> UsecaseResult<()> {
        Ok(())
    }
    async fn clear_session(&self, _b: &str, _n: u64, _s: Slot) -> UsecaseResult<()> {
        Ok(())
    }
    async fn post_comment(&self, _n: u64, _body: &str) -> UsecaseResult<String> {
        Ok("posted-1".into())
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
    async fn create_issue(
        &self,
        _b: &str,
        _p: &str,
        _t: &str,
        _body: &str,
        _kind: Option<&str>,
    ) -> UsecaseResult<u64> {
        Ok(100)
    }
    async fn set_issue_type(&self, _n: u64, _kind: &str) -> UsecaseResult<()> {
        Ok(())
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
    async fn archive_row(&self, _b: &str, _n: u64) -> UsecaseResult<()> {
        Ok(())
    }
    async fn issues_with_title(&self, _title: &str) -> UsecaseResult<Vec<u64>> {
        Ok(Vec::new())
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
            "cron/Тест/README" => {
                Some("---\ntitle: Тест\nschedule: 5m\nproject: uwuwu\n---\n\nТело".into())
            }
            _ => None,
        })
    }
    async fn list_docs(&self, prefix: &str) -> UsecaseResult<Vec<String>> {
        Ok(if prefix.contains("cron") {
            vec!["cron/Тест/README".into()]
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
    async fn append_doc(&self, _s: &str, _l: &str, _m: &str) -> UsecaseResult<AppendDocOutcome> {
        Ok(AppendDocOutcome {
            created: false,
            lines: 0,
        })
    }
}

fn assert_model(spec: Option<SessionSpec>, what: &str, raw: &str) {
    let spec = spec.expect(what);
    let want = SpawnModel::parse(raw).unwrap();
    assert_eq!(
        spec.model, want.model,
        "{what}: модель спавна = карта сервера"
    );
    assert_eq!(spec.variant, want.variant, "{what}: вариант из карты");
}

#[tokio::test]
async fn analyst_review_spawn_carries_model_from_server_map() {
    // Arrange: строка Review Analys — спавн парного ревьювера волны
    let sessions = CaptureSessions::new(vec![]);
    let u = AnalystReviewUseCase {
        board: Arc::new(RowsBoard {
            rows: vec![Task::new(9, "проработанная".into(), Status::ReviewAnalys)],
        }),
        boards: vec![AnalystBoard {
            project: "uwuwu".into(),
            board: "Задачи uwuwu".into(),
            checkout: "/tmp".into(),
        }],
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        state: Arc::new(NoopState),
        clock: Arc::new(TestClock),
        models: models(),
        cup: 7,
    };

    // Act
    let spawned = u.tick().await.unwrap();

    // Assert
    assert!(spawned >= 1, "ревью-спавн состоялся");
    assert_model(
        sessions.spec_of(),
        "ревью-спавн аналитики",
        "zai-coding-plan/glm-5.3#high",
    );
}

#[tokio::test]
async fn analyst_spawn_carries_model_from_server_map() {
    // Arrange: волна — две строки Analys
    let sessions = CaptureSessions::new(vec![]);
    let u = AnalystUseCase {
        board: Arc::new(RowsBoard {
            rows: vec![
                Task::new(7, "первая".into(), Status::Analys),
                Task::new(8, "вторая".into(), Status::Analys),
            ],
        }),
        boards: vec![AnalystBoard {
            project: "uwuwu".into(),
            board: "Задачи uwuwu".into(),
            checkout: "/tmp".into(),
        }],
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        state: Arc::new(NoopState),
        clock: Arc::new(TestClock),
        models: models(),
        cup: 7,
    };

    // Act
    let spawned = u.tick().await.unwrap();

    // Assert
    assert!(spawned >= 1, "аналитик-спавн состоялся");
    assert_model(
        sessions.spec_of(),
        "аналитик-спавн",
        "zai-coding-plan/glm-5.3-flash#high",
    );
}

#[tokio::test]
async fn dreamer_spawn_carries_model_from_server_map() {
    // Arrange: уснувшая dev-сессия с финалом и реальной работой
    let sessions = CaptureSessions::new(vec![SessionInfo {
        id: "ses_dev1".into(),
        agent: "developer".into(),
        directory: String::new(),
        updated_ms: 0,
        outcome: "stop".into(),
        total_tokens: 42_000,
        busy: false,
    }]);
    let u = DreamerUseCase {
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        state: Arc::new(NoopState),
        clock: Arc::new(TestClock),
        models: models(),
        checkout: "/tmp".into(),
        cup: 7,
    };

    // Act
    let spawned = u.tick().await.unwrap();

    // Assert
    assert_eq!(spawned, 1, "дример-спавн состоялся");
    assert_model(
        sessions.spec_of(),
        "дример-спавн",
        "zai-coding-plan/glm-5.3-flash#high",
    );
}

#[tokio::test]
async fn cron_run_carries_model_from_server_map() {
    // Arrange
    let sessions = CaptureSessions::new(vec![]);
    let u = CronUseCase {
        wiki: Arc::new(CronWiki),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        clock: Arc::new(TestClock),
        models: models(),
        projects: std::collections::HashMap::from([("uwuwu".to_string(), "/tmp".to_string())]),
        cup: 7,
    };

    // Act
    let fired = u.tick().await.unwrap();

    // Assert
    assert_eq!(fired.len(), 1, "крон-файр состоялся");
    // мастер без fm agent — Default-слот → default-модель карты
    assert_model(
        sessions.spec_of(),
        "крон-спавн",
        "zai-coding-plan/glm-5.3-flash#high",
    );
}
