//! Юнит-тесты тика на in-memory фейках портов (тест-стратегия L2:
//! юзкейсы — с фейками, без сети/досок).

use crate::error::UsecaseResult;
use crate::pipeline::tick::{BoardPlan, Mode, TickPlan, TickUseCase};
use crate::ports::{
    AgentSessionGateway, Clock, CommentRef, Liveness, PrGateway, PrState, SessionSpec,
    TaskBoardRepo, TickStateStore,
};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use uwuwu_domain::{
    BranchName, PeakWindow, PrUrl, Slot, SpawnPolicy, Status, Task, TaskFields, UtcMinute,
};

struct FakeBoard {
    rows: Vec<Task>,
    log: Mutex<Vec<String>>,
    /// Журнал комментов задач (будильник-сценарии: latest_comments).
    comments: Vec<CommentRef>,
}

#[async_trait]
impl TaskBoardRepo for FakeBoard {
    async fn read_board(&self, _board: &str) -> UsecaseResult<Vec<Task>> {
        Ok(self.rows.clone())
    }
    async fn set_status(&self, _b: &str, n: u64, s: Status) -> UsecaseResult<()> {
        self.log.lock().unwrap().push(format!("status#{n}={s:?}"));
        Ok(())
    }
    async fn set_session(&self, _b: &str, n: u64, _slot: Slot, s: &str) -> UsecaseResult<()> {
        self.log.lock().unwrap().push(format!("session#{n}={s}"));
        Ok(())
    }
    async fn clear_session(&self, _b: &str, n: u64, slot: Slot) -> UsecaseResult<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("clear#{n}={:?}", slot));
        Ok(())
    }
    async fn issue_fields(&self, n: u64) -> UsecaseResult<uwuwu_domain::TaskFields> {
        // enrichment-фейк: поля несут строки (как настоящий hub из REST)
        Ok(self
            .rows
            .iter()
            .find(|t| t.number() == n)
            .map(|t| {
                let mut f = TaskFields::default();
                for slot in [
                    Slot::Dev,
                    Slot::Design,
                    Slot::PlanReview,
                    Slot::ResultReview,
                    Slot::Test,
                ] {
                    if let Some(s) = t.session_of(slot) {
                        f.set_session(slot, uwuwu_domain::SessionRef::parse(s).unwrap());
                    }
                }
                let mut f = f.clone();
                // VCS тоже переносим (как настоящий hub: Branch/PR-поля issue)
                if let Some(b) = t.branch().cloned() {
                    f.set_branch(b);
                }
                if let Some(p) = t.pr().cloned() {
                    let _ = f.open_pr(p);
                }
                f
            })
            .unwrap_or_default())
    }
    async fn post_comment(&self, n: u64, body: &str) -> UsecaseResult<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("comment#{n}={}", body.len()));
        Ok(())
    }
    async fn close_issue(&self, n: u64) -> UsecaseResult<()> {
        self.log.lock().unwrap().push(format!("close#{n}"));
        Ok(())
    }
    async fn latest_comments(&self, _n: u64) -> UsecaseResult<Vec<CommentRef>> {
        Ok(self.comments.clone())
    }
    async fn read_issue(&self, _b: &str, n: u64) -> UsecaseResult<crate::ports::IssueView> {
        let task = self
            .rows
            .iter()
            .find(|t| t.number() == n)
            .cloned()
            .unwrap_or_else(|| Task::new(n, String::new(), Status::Empty));
        Ok(crate::ports::IssueView {
            task,
            body: String::new(),
            comments: vec![],
        })
    }
    async fn create_issue(
        &self,
        _b: &str,
        _p: &str,
        title: &str,
        _body: &str,
    ) -> UsecaseResult<u64> {
        let n = self.rows.len() as u64 + 100;
        self.log.lock().unwrap().push(format!("create:{title}"));
        Ok(n)
    }
    async fn rename_issue(&self, _n: u64, _t: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn patch_cells(&self, n: u64, _m: crate::ports::FmMutator) -> UsecaseResult<()> {
        self.log.lock().unwrap().push(format!("cells#{n}"));
        Ok(())
    }
    async fn close_issue_ext(&self, n: u64, _np: bool) -> UsecaseResult<()> {
        self.log.lock().unwrap().push(format!("close_ext#{n}"));
        Ok(())
    }
    async fn replace_body(&self, n: u64, _b: &str) -> UsecaseResult<()> {
        self.log.lock().unwrap().push(format!("body#{n}"));
        Ok(())
    }
}

#[derive(Default)]
struct FakeSessions {
    prompts: Mutex<Vec<String>>,
    spawned: Mutex<Vec<String>>,
    /// ms последней активности любой сессии (сталл-детект-тесты).
    activity_ms: u64,
    /// Id мёртвых сессий (liveness-сценарии кик-разморозки/респавна).
    dead_sessions: Mutex<Vec<String>>,
    /// Недоступность list_sessions (fallback-сценарий CUP-подсчёта).
    list_fails: bool,
    /// Последнее сообщение сессии — финальный ответ агента (idle).
    idle_reply: bool,
    /// спеки созданных сессий (агент/директория — для ассертов)
    specs: Mutex<Vec<SessionSpec>>,
    /// живые сессии исполнителя: (agent-имя, outcome; пустой outcome = живая)
    live: Mutex<Vec<(String, String)>>,
}

impl FakeSessions {
    fn new() -> Self {
        Self {
            prompts: Mutex::default(),
            spawned: Mutex::default(),
            specs: Mutex::default(),
            live: Mutex::default(),
            activity_ms: 1000,
        }
    }
    fn with_live(agent: &str) -> Self {
        Self {
            live: Mutex::new(vec![(agent.into(), String::new())]),
            ..Self::new()
        }
    }


}

#[async_trait]
impl AgentSessionGateway for FakeSessions {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String> {
        self.specs.lock().unwrap().push(spec.clone());
        let id = format!("ses_{}", self.spawned.lock().unwrap().len() + 1);
        self.spawned.lock().unwrap().push(id.clone());
        Ok(id)
    }
    async fn prompt(&self, session: &str, text: &str) -> UsecaseResult<()> {
        self.prompts
            .lock()
            .unwrap()
            .push(format!("{session}|{text}"));
            .push(format!("{session}: {text}"));

        Ok(())
    }
    async fn run_command(&self, _s: &str, _n: &str, _a: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, s: &str) -> Liveness {
        if self.dead_sessions.lock().unwrap().iter().any(|d| d == s) {
            Liveness::Dead
        } else {
            Liveness::Alive
        }
    }
    async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
        Ok(Some(self.activity_ms))
    }
    async fn last_assistant_reply(
        &self,
        _s: &str,
    ) -> UsecaseResult<Option<crate::ports::AssistantReply>> {
        Ok(self.idle_reply.then(|| crate::ports::AssistantReply {
            created_ms: self.activity_ms,
            text: String::new(),
            finished: true,
            is_latest: true,
        }))
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<crate::ports::SessionInfo>> {
        if self.list_fails {
            return Err(crate::error::UsecaseError::Session(
                "list недоступен (мок)".into(),
            ));
        }
        // spawned = живые сессии (outcome пуст — финала нет)
        Ok(self
            .spawned
            .lock()
            .unwrap()
            .iter()
            .map(|id| crate::ports::SessionInfo {
                id: id.clone(),
                agent: String::new(),
                updated_ms: 0,
                outcome: String::new(),
        Ok(self
            .live
            .lock()
            .unwrap()
            .iter()
            .map(|(agent, outcome)| crate::ports::SessionInfo {
                id: format!("ses_live_{agent}"),
                agent: agent.clone(),
                updated_ms: 0,
                outcome: outcome.clone(),

                total_tokens: 0,
            })
            .collect())
    }
    async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
}

struct FakePrs {
    state: PrState,
    merges: Mutex<Vec<String>>,
}

#[async_trait]
impl PrGateway for FakePrs {
    async fn probe(&self, _r: &str, _u: Option<&str>, _b: Option<&str>) -> UsecaseResult<PrState> {
        Ok(self.state)
    }
    async fn merge(&self, _r: &str, pr: &str) -> UsecaseResult<()> {
        self.merges.lock().unwrap().push(pr.into());
        Ok(())
    }
}

struct FixedClock;
impl Clock for FixedClock {
    fn utc_minute(&self) -> UtcMinute {
        UtcMinute {
            minute_of_day: 100,
            weekday: 3,
        }
    }
    fn now_ms(&self) -> u64 {
        2000
    }
}

#[derive(Default)]
struct MemStateInner {
    comments: Vec<String>,
    statuses: Vec<(u64, Status)>,
    /// (number, slot) → (status, repeats)
    loops: Vec<(u64, Slot, Status, u32)>,
    /// (number, status, rounds) — цикл-гвард аналитики (#1135)
    analys_loops: Vec<(u64, Status, u32)>,
    /// (number, status, repeats) — цикл-гвард ревью-контура
    analys_review_loops: Vec<(u64, Status, u32)>,
    /// (number, ts) — кулдауны волн аналитика/ревьювера
    analys_ms: Vec<(u64, u64)>,
    analys_review_ms: Vec<(u64, u64)>,
    resets: Vec<(u64, Status)>,
    /// сталл-детект: (number, статус раунда, ms пинга)
    stalls: Vec<(u64, Status, u64)>,
}
struct MemState(Mutex<MemStateInner>);
impl TickStateStore for MemState {
    fn last_comment_id(&self, _b: &str, _n: u64) -> Option<String> {
        self.0.lock().unwrap().comments.last().cloned()
    }
    fn set_last_comment_id(&self, _b: &str, _n: u64, id: &str) {
        self.0.lock().unwrap().comments.push(id.into());
    }
    fn last_seen_status(&self, _b: &str, n: u64) -> Option<Status> {
        self.0
            .lock()
            .unwrap()
            .statuses
            .iter()
            .rev()
            .find(|(num, _)| *num == n)
            .map(|(_, st)| *st)
    }
    fn set_last_seen_status(&self, _b: &str, n: u64, status: Status) {
        self.0.lock().unwrap().statuses.push((n, status));
    }
    fn dreamer_done(&self, _s: &str) -> bool {
        false
    }
    fn mark_dreamer_done(&self, _s: &str) {}
    fn dreamer_day_spawns(&self, _day: &str) -> u32 {
        0
    }
    fn mark_dreamer_spawn(&self, _day: &str) {}
    fn last_spawn_ms(&self, _b: &str, _n: u64, _s: Slot) -> Option<u64> {
        None
    }
    fn mark_spawn(&self, _b: &str, _n: u64, _s: Slot, _at: u64) {}
    fn clear_spawn_ms(&self, _b: &str, _n: u64, _s: Slot) {}
    fn last_analys_ms(&self, _b: &str, n: u64) -> Option<u64> {
        self.0
            .lock()
            .unwrap()
            .analys_ms
            .iter()
            .rev()
            .find(|(num, _)| *num == n)
            .map(|(_, ts)| *ts)
    }
    fn mark_analys_spawn(&self, _b: &str, n: u64, at: u64) {
        self.0.lock().unwrap().analys_ms.push((n, at));
    }
    fn last_analys_review_ms(&self, _b: &str, n: u64) -> Option<u64> {
        self.0
            .lock()
            .unwrap()
            .analys_review_ms
            .iter()
            .rev()
            .find(|(num, _)| *num == n)
            .map(|(_, ts)| *ts)
    }
    fn mark_analys_review_spawn(&self, _b: &str, n: u64, at: u64) {
        self.0.lock().unwrap().analys_review_ms.push((n, at));
    }
    fn analys_loop(&self, _b: &str, n: u64) -> Option<(Status, u32)> {
        self.0
            .lock()
            .unwrap()
            .analys_loops
            .iter()
            .rev()
            .find(|(num, _, _)| *num == n)
            .map(|(_, st, r)| (*st, *r))
    }
    fn set_analys_loop(&self, _b: &str, n: u64, status: Status, rounds: u32) {
        let mut g = self.0.lock().unwrap();
        if let Some(e) = g.analys_loops.iter_mut().find(|(num, _, _)| *num == n) {
            *e = (n, status, rounds);
        } else {
            g.analys_loops.push((n, status, rounds));
        }
    }
    fn clear_analys_loop(&self, _b: &str, n: u64) {
        self.0
            .lock()
            .unwrap()
            .analys_loops
            .retain(|(num, _, _)| *num != n);
    }
    fn stall_ping(&self, _b: &str, n: u64) -> Option<(Status, u64, u32)> {
        let g = self.0.lock().unwrap();
        let last = g.stalls.iter().rev().find(|(num, ..)| *num == n)?;
        let count = g
            .stalls
            .iter()
            .filter(|(num, st, _)| *num == n && *st == last.1)
            .count() as u32;
        Some((last.1, last.2, count))
    }
    fn mark_stall_ping(&self, _b: &str, n: u64, status: Status, at_ms: u64) {
        self.0.lock().unwrap().stalls.push((n, status, at_ms));
    }
    fn analys_review_loop(&self, _b: &str, n: u64) -> Option<(Status, u32)> {
        self.0
            .lock()
            .unwrap()
            .analys_review_loops
            .iter()
            .rev()
            .find(|(num, _, _)| *num == n)
            .map(|(_, st, r)| (*st, *r))
    }
    fn set_analys_review_loop(&self, _b: &str, n: u64, status: Status, repeats: u32) {
        let mut g = self.0.lock().unwrap();
        if let Some(e) = g
            .analys_review_loops
            .iter_mut()
            .find(|(num, _, _)| *num == n)
        {
            *e = (n, status, repeats);
        } else {
            g.analys_review_loops.push((n, status, repeats));
        }
    }
    fn clear_analys_review_loop(&self, _b: &str, n: u64) {
        self.0
            .lock()
            .unwrap()
            .analys_review_loops
            .retain(|(num, _, _)| *num != n);
    }
    fn prune_analys_guards(&self, live: &std::collections::HashSet<String>) {
        let mut g = self.0.lock().unwrap();
        g.analys_loops
            .retain(|(num, _, _)| live.contains(&format!("B:{num}")));
        g.analys_review_loops
            .retain(|(num, _, _)| live.contains(&format!("B:{num}")));
    }
    fn spawn_loop(&self, _b: &str, n: u64, s: Slot) -> Option<(Status, u32)> {
        self.0
            .lock()
            .unwrap()
            .loops
            .iter()
            .rev()
            .find(|(num, slot, _, _)| *num == n && *slot == s)
            .map(|(_, _, st, r)| (*st, *r))
    }
    fn set_spawn_loop(&self, _b: &str, n: u64, s: Slot, status: Status, r: u32) {
        let mut g = self.0.lock().unwrap();
        if let Some(e) = g
            .loops
            .iter_mut()
            .find(|(num, slot, _, _)| *num == n && *slot == s)
        {
            *e = (n, s, status, r);
        } else {
            g.loops.push((n, s, status, r));
        }
    }
    fn reset_stale_spawn_loops(&self, _b: &str, n: u64, current: Status) {
        let mut g = self.0.lock().unwrap();
        g.resets.push((n, current));
        g.loops
            .retain(|(num, _, st, _)| !(*num == n && *st != current));
    }
    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}

fn board_plan() -> BoardPlan {
    BoardPlan {
        project: "p".into(),
        board: "B".into(),
        repo: "a/c".into(),
        checkout: "/tmp/checkout".into(),
        env_root: "/tmp/env".into(),
    }
}

fn plan(mode: Mode, cup: usize) -> TickPlan {
    TickPlan {
        mode,
        policy: SpawnPolicy {
            cup,
            peak: PeakWindow {
                enabled: false,
                start_min: 0,
                end_min: 0,
            },
        },
        boards: vec![board_plan()],
    }
}

fn usecase(
    rows: Vec<Task>,
    pr: PrState,
) -> (TickUseCase, Arc<FakeBoard>, Arc<FakeSessions>, Arc<FakePrs>) {
    usecase_with_state(rows, pr, Arc::new(MemState(Mutex::default())))
}

fn usecase_with_state(
    rows: Vec<Task>,
    pr: PrState,
    state: Arc<MemState>,
) -> (TickUseCase, Arc<FakeBoard>, Arc<FakeSessions>, Arc<FakePrs>) {
    let board = Arc::new(FakeBoard {
        rows,
        log: Mutex::default(),
        comments: Vec::default(),
    });
    let sessions = Arc::new(FakeSessions::new());
    let prs = Arc::new(FakePrs {
        state: pr,
        merges: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: sessions.clone(),
        prs: prs.clone(),
        clock: Arc::new(FixedClock),
        state,
        spawn_model: None,
    };
    (u, board, sessions, prs)
}

/// План с включённым пик-окном 06:00–10:00 UTC (PeakClock — внутри него).
fn peak_plan(mode: Mode, cup: usize) -> TickPlan {
    TickPlan {
        mode,
        policy: SpawnPolicy {
            cup,
            peak: PeakWindow {
                enabled: true,
                start_min: 360,
                end_min: 600,
            },
        },
        boards: vec![board_plan()],
    }
}

/// Юзкейс в пик-окне с настраиваемыми комментами (будильник-сценарии).
fn peak_usecase(
    rows: Vec<Task>,
    comments: Vec<CommentRef>,
    state: Arc<MemState>,
) -> (TickUseCase, Arc<FakeBoard>, Arc<FakeSessions>, Arc<FakePrs>) {
    let board = Arc::new(FakeBoard {
        rows,
        log: Mutex::default(),
        comments,
    });
    let sessions = Arc::new(FakeSessions {
        prompts: Mutex::default(),
        spawned: Mutex::default(),
        activity_ms: 1000,
        dead_sessions: Mutex::default(),
        list_fails: false,
        idle_reply: false,
    });
    let prs = Arc::new(FakePrs {
        state: PrState::Missing,
        merges: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: sessions.clone(),
        prs: prs.clone(),
        clock: Arc::new(PeakClock),
        state,
        spawn_model: None,
    };
    (u, board, sessions, prs)
}

/// Юзкейс с готовым моком сессий (сценарии list_fails и пр.).
fn usecase_with_sessions(
    rows: Vec<Task>,
    pr: PrState,
    sessions: FakeSessions,
    state: Arc<MemState>,
) -> (TickUseCase, Arc<FakeBoard>, Arc<FakeSessions>, Arc<FakePrs>) {
    let board = Arc::new(FakeBoard {
        rows,
        log: Mutex::default(),
        comments: Vec::default(),
    });
    let sessions = Arc::new(sessions);
    let prs = Arc::new(FakePrs {
        state: pr,
        merges: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: sessions.clone(),
        prs: prs.clone(),
        clock: Arc::new(FixedClock),
        state,
        spawn_model: None,
    };
    (u, board, sessions, prs)
}

/// Часы внутри пик-окна (06:00–10:00 UTC, будни) — UC-05 тесты.
struct PeakClock;
impl Clock for PeakClock {
    fn utc_minute(&self) -> UtcMinute {
        UtcMinute {
            minute_of_day: 400,
            weekday: 3,
        }
    }
    fn now_ms(&self) -> u64 {
        2000
    }
}

fn task(n: u64, status: Status) -> Task {
    Task::new(n, format!("t{n}"), status)
}

#[tokio::test]
async fn observe_never_touches_ports() {
    let (u, board, sessions, _) = usecase(vec![task(1, Status::InProgress)], PrState::Missing);
    let out = u.run(&plan(Mode::Observe, 7)).await.unwrap();
    assert_eq!(out.spawns_allowed, 1);
    assert_eq!(out.spawns_done, 0);
    assert!(board.log.lock().unwrap().is_empty(), "мутаций доски нет");
    assert!(sessions.spawned.lock().unwrap().is_empty(), "спавнов нет");
}

#[tokio::test]
async fn live_spawns_and_claims_session() {
    let (u, board, sessions, _) = usecase(vec![task(1, Status::InProgress)], PrState::Missing);
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 1);
    assert_eq!(
        sessions.spawned.lock().unwrap().len(),
        2,
        "bootstrap + work"
    );
    assert!(
        !sessions.prompts.lock().unwrap().is_empty(),
        "спавн-промпт отправлен"
    );
    assert!(board
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("session#1")));
}

#[tokio::test]
async fn cup_holds_spawn_in_live() {
    let rows: Vec<Task> = (1..=8).map(|n| task(n, Status::InProgress)).collect();
    let (u, board, sessions, _) = usecase(rows, PrState::Missing);
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.held_cup, 8, "активных 8 > капа 7");
    assert_eq!(out.spawns_done, 0);
    assert!(sessions.spawned.lock().unwrap().is_empty());
    assert!(board.log.lock().unwrap().is_empty());
}

#[tokio::test]
async fn autodone_merged_finishes() {
    let mut t = task(3, Status::UserReview);
    t.set_autodone(true);
    let (u, board, _, _) = usecase(vec![t], PrState::Merged);
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.autodone_done, 1);
    let log = board.log.lock().unwrap();
    assert!(log.iter().any(|l| l.contains("status#3=Done")), "{log:?}");
    assert!(log.iter().any(|l| l.contains("close#3")));
}

#[tokio::test]
async fn autodone_red_ci_bounces_to_in_progress() {
    let mut t = task(4, Status::UserReview);
    t.set_autodone(true);
    let (u, board, _, _) = usecase(
        vec![t],
        PrState::Open {
            mergeable: true,
            ci_green: false,
        },
    );
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|l| l.contains("status#4=InProgress")),
        "{log:?}"
    );
    assert!(!log.iter().any(|l| l.contains("close#4")));
}

#[tokio::test]
async fn autodone_green_merges() {
    let mut t = task(5, Status::UserReview);
    t.set_autodone(true);
    let mut f = TaskFields::default();
    f.set_branch(BranchName::parse("work/task-5").unwrap());
    f.open_pr(PrUrl::parse("https://github.com/a/c/pull/9").unwrap())
        .unwrap();
    t.apply_fields(f);
    let (u, board, _, prs) = usecase(
        vec![t],
        PrState::Open {
            mergeable: true,
            ci_green: true,
        },
    );
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(prs.merges.lock().unwrap().len(), 1);
    assert!(board
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("status#5=Done")));
}

#[tokio::test]
async fn autodone_missing_pr_with_branch_bounces() {
    let mut t = task(6, Status::UserReview);
    t.set_autodone(true);
    let mut f = TaskFields::default();
    f.set_branch(BranchName::parse("work/task-6").unwrap());
    t.apply_fields(f);
    let (u, board, _, _) = usecase(vec![t], PrState::Missing);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|l| l.contains("status#6=InProgress")),
        "{log:?}"
    );
}

#[tokio::test]
async fn autodone_docs_only_done() {
    let mut t = task(7, Status::UserReview);
    t.set_autodone(true);
    let (u, board, _, _) = usecase(vec![t], PrState::Missing);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(board
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("status#7=Done")));
}

#[tokio::test]
async fn observe_does_not_charge_spawn_loops() {
    // Инцидент #1046 (15.11): observe-тики заряжали счётчик спавн-петли
    // (set_spawn_loop звался до Live-гейта) — live эскалировал строку
    // в Needs attention первым же спавном.
    let state = Arc::new(MemState(Mutex::default()));
    let (u, _, _, _) = usecase_with_state(
        vec![task(11, Status::InProgress)],
        PrState::Missing,
        state.clone(),
    );
    let out = u.run(&plan(Mode::Observe, 7)).await.unwrap();
    assert_eq!(out.spawns_allowed, 1, "решение логируется");
    assert!(
        state.0.lock().unwrap().loops.is_empty(),
        "observe не пишет счётчики петель"
    );
}

#[tokio::test]
async fn status_journey_resets_loop_counter() {
    // Счётчик (In Progress, 2) пережил путь IP→Design→Ревью плана→IP:
    // без сброса при наблюдении строки в другом статусе первый же
    // спавн после возврата эскалирует (инцидент #1046).
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .loops
        .push((12, Slot::Dev, Status::InProgress, 2));
    // тик, пока строка в Design: счётчик dev-слота устарел — сброс
    let (u, _, _, _) = usecase_with_state(
        vec![task(12, Status::Design)],
        PrState::Missing,
        state.clone(),
    );
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        !state
            .0
            .lock()
            .unwrap()
            .loops
            .iter()
            .any(|(n, slot, _, _)| *n == 12 && *slot == Slot::Dev),
        "счётчик dev-слота чужого статуса сброшен"
    );
    // возврат в In Progress: спавн чистый, без эскалации
    let (u2, board, _, _) = usecase_with_state(
        vec![task(12, Status::InProgress)],
        PrState::Missing,
        state.clone(),
    );
    let out = u2.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 1, "спавн прошёл");
    assert_eq!(out.attention_flagged, 0, "эскалации нет");
    assert!(board
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("session#12=")));
}

#[tokio::test]
async fn multi_round_reentry_never_escalates() {
    // Популярный кейс (инцидент #1046): строка ходит по кругу
    // In Progress → Ревью результата → (request_changes) → In Progress —
    // три круга доработки. Прогресс есть — луп-гвард не эскалирует.
    let state = Arc::new(MemState(Mutex::default()));
    let mut escalations = 0usize;
    for round in 0..3 {
        // круг: спавн dev → результат (ReviewResult) → вердикт на доработку
        let (u1, _, sessions, _) = usecase_with_state(
            vec![task(20, Status::InProgress)],
            PrState::Missing,
            state.clone(),
        );
        let out = u1.run(&plan(Mode::Live, 7)).await.unwrap();
        assert_eq!(out.spawns_done, 1, "круг {round}: dev-спавн");
        assert_eq!(out.attention_flagged, 0, "круг {round}: без эскалации");
        // ревью результата (свой слот)
        let mut t = task(20, Status::ReviewResult);
        let mut f = TaskFields::default();
        f.set_session(
            Slot::Dev,
            uwuwu_domain::SessionRef::parse(&format!("ses_round{round}")).unwrap(),
        );
        t.apply_fields(f);
        let (u2, _, _, _) = usecase_with_state(vec![t], PrState::Missing, state.clone());
        let out2 = u2.run(&plan(Mode::Live, 7)).await.unwrap();
        assert_eq!(out2.attention_flagged, 0, "круг {round}: ревью чисто");
        escalations += out.attention_flagged + out2.attention_flagged;
        let _ = sessions;
    }
    assert_eq!(escalations, 0, "три круга доработки — ни одной эскалации");
}

#[tokio::test]
async fn stage_transition_gives_fresh_eyes_to_review() {
    // Канон сессий: ревью-сессии всегда новые — переход в стадию ревью
    // снимает прошлую сессию слота, спавнится свежий ревьюер
    let state = Arc::new(MemState(Mutex::default()));
    let mut t = task(30, Status::ReviewResult);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev").unwrap(),
    );
    f.set_session(
        Slot::ResultReview,
        uwuwu_domain::SessionRef::parse("ses_old_reviewer").unwrap(),
    );
    t.apply_fields(f);
    // прошлый тик видел In Progress → сейчас ReviewResult = переход
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((30, Status::InProgress));
    let (u, board, sessions, _) = usecase_with_state(vec![t], PrState::Missing, state.clone());
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|l| l.starts_with("clear#30=")),
        "ревью-сессия снята при переходе: {log:?}"
    );
    assert_eq!(out.spawns_done, 1, "свежий ревьюер заспавнен");
    assert!(!sessions.spawned.lock().unwrap().is_empty());
}

#[tokio::test]
async fn same_round_keeps_session_no_clear() {
    // Тот же раунд (статус не менялся): сессия слота живёт, будится
    // по комментам — никакого fresh-eyes внутри раунда
    let state = Arc::new(MemState(Mutex::default()));
    let mut t = task(31, Status::ReviewResult);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::ResultReview,
        uwuwu_domain::SessionRef::parse("ses_reviewer").unwrap(),
    );
    t.apply_fields(f);
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((31, Status::ReviewResult));
    let (u, board, _, _) = usecase_with_state(vec![t], PrState::Missing, state);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter().any(|l| l.starts_with("clear#31=")),
        "внутри раунда сессию не трогаем: {log:?}"
    );
}

#[tokio::test]
async fn kick_unfreezes_dead_dev_session() {
    // #397: кик владельца в стадию с мёртвой дев-ячейкой обязан
    // разворачиваться: мёртвая сессия снимается, свежий исполнитель
    // спавнится (через гейт — не-high в пике останется замороженным)
    let state = Arc::new(MemState(Mutex::default()));
    let mut t = task(33, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_dev").unwrap(),
    );
    t.apply_fields(f);
    state.0.lock().unwrap().statuses.push((33, Status::Testing));
    let (u, board, sessions, _) = usecase_with_state(vec![t], PrState::Missing, state);
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_dev".into());
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|l| l.starts_with("clear#33=")),
        "мёртвая дев-сессия снята при кике: {log:?}"
    );
    assert_eq!(out.spawns_done, 1, "свежий исполнитель заспавнен");
}

#[tokio::test]
async fn dead_stage_session_does_not_hold_cup() {
    // #1200: кап держат живые LLM-работы, не статусы — строка с мёртвой
    // сессией не активна, и её слот достаётся следующему запуску
    let mut dead = task(40, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_40").unwrap(),
    );
    dead.apply_fields(f);
    let candidate = task(41, Status::InProgress);
    // тот же раунд (prev=In Progress) — dead40 идёт респавн-путём,
    // не кик-разморозкой
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((40, Status::InProgress));
    let (u, _, sessions, _) = usecase_with_state(vec![dead, candidate], PrState::Missing, state);
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_40".into());
    // cup=2: снапшот active=1 (кандидат), респавн инкрементит счётчик —
    // по статусному подсчёту active=2 → held бы не было вовсе; честный
    // счётчик вмещает ровно один запуск, кандидат ждёт следующего тика
    let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out.spawns_done, 1, "респавн мёртвой строки в слоте капа");
    assert_eq!(out.held_cup, 1, "кандидат удержан честным счётчиком");
}

#[tokio::test]
async fn dead_stage_session_holds_cup_when_list_unavailable() {
    // fallback: исполнитель недоступен — сессионные считаются активными
    // (статусный подсчёт), консервативно против спавн-шторма
    let mut dead = task(42, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_42").unwrap(),
    );
    dead.apply_fields(f);
    let candidate = task(43, Status::InProgress);
    let sessions = FakeSessions {
        prompts: Mutex::default(),
        spawned: Mutex::default(),
        activity_ms: 1000,
        dead_sessions: Mutex::default(),
        list_fails: true,
        idle_reply: false,
    };
    let (u, _, _, _) = usecase_with_sessions(
        vec![dead, candidate],
        PrState::Missing,
        sessions,
        Arc::new(MemState(Mutex::default())),
    );
    let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out.spawns_done, 0, "fallback держит консервативный кап");
    assert_eq!(out.held_cup, 1);
}

#[tokio::test]
async fn medium_wakeup_deferred_in_peak() {
    // UC-05/#1200: пик держит будильник не-high строки — prompt не уходит,
    // коммент копится (last_comment_id не пишется), доставка после окна
    let state = Arc::new(MemState(Mutex::default()));
    let mut t = task(34, Status::InProgress);
    t.set_priority(uwuwu_domain::Priority::Medium);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_live").unwrap(),
    );
    t.apply_fields(f);
    let (u, _, sessions, _) = peak_usecase(
        vec![t],
        vec![CommentRef {
            id: "c1".into(),
            created_ms: 3000,
            body: "свежий коммент".into(),
        }],
        state,
    );
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert!(
        sessions.prompts.lock().unwrap().is_empty(),
        "не-high в пике не будится"
    );
    assert_eq!(out.wakeups_deferred, 1);
    assert_eq!(out.wakeups, 0);
}

#[tokio::test]
async fn high_wakeup_passes_peak() {
    // канон UC-05: high продолжают работать — будильник high в окне доставляется
    let state = Arc::new(MemState(Mutex::default()));
    let mut t = task(35, Status::InProgress);
    t.set_priority(uwuwu_domain::Priority::High);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_high").unwrap(),
    );
    t.apply_fields(f);
    let (u, _, sessions, _) = peak_usecase(
        vec![t],
        vec![CommentRef {
            id: "c1".into(),
            created_ms: 3000,
            body: "свежий коммент".into(),
        }],
        state,
    );
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 1, "high будится в пике");
    assert!(!sessions.prompts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn medium_respawn_deferred_in_peak() {
    // #1161-класс: мёртвая stage-сессия не-high в окне — респавн отложен
    let mut t = task(36, Status::InProgress);
    t.set_priority(uwuwu_domain::Priority::Medium);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_36").unwrap(),
    );
    t.apply_fields(f);
    // тот же раунд (статус не менялся) — путь чистого respawn, без kick-ветки
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((36, Status::InProgress));
    let (u, _, sessions, _) = peak_usecase(vec![t], Vec::new(), state);
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_36".into());
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "не-high в пике не респавнится"
    );
    assert_eq!(out.wakeups_deferred, 1);
}

#[tokio::test]
async fn respawn_held_under_cup() {
    // респавн = фактический спавн: CUP-кап держит его как прямой спавн
    let mut t = task(37, Status::InProgress);
    t.set_priority(uwuwu_domain::Priority::High);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_37").unwrap(),
    );
    t.apply_fields(f);
    // тот же раунд — путь чистого respawn
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((37, Status::InProgress));
    let (u, _, sessions, _) = peak_usecase(vec![t], Vec::new(), state);
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_37".into());
    // cup=0: даже одна кандидату-строка не влезает (active 0 >= 0)
    let out = u.run(&peak_plan(Mode::Live, 0)).await.unwrap();
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "респавн держится CUP-капом"
    );
    assert_eq!(out.wakeups_deferred, 1);
}

#[tokio::test]
async fn high_respawn_passes_peak() {
    // high в пике респавнится (байпас), CUP свободен
    let mut t = task(38, Status::InProgress);
    t.set_priority(uwuwu_domain::Priority::High);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_38").unwrap(),
    );
    t.apply_fields(f);
    // тот же раунд — путь чистого respawn
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((38, Status::InProgress));
    let (u, _, sessions, _) = peak_usecase(vec![t], Vec::new(), state);
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_38".into());
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    // респавн — фактический спавн: счёт в spawns_done, сессия создана
    assert_eq!(out.spawns_done, 1, "high-респавн в пике проходит");
    assert_eq!(out.wakeups, 0);
    assert!(!sessions.spawned.lock().unwrap().is_empty());
}

#[tokio::test]
async fn respawn_escalation_does_not_count_wakeups() {
    // Low-находка ревью: луп-гвард Escalate (без спавна) не накручивает
    // счётчики запусков — строка уходит в Needs attention
    let mut t = task(38, Status::InProgress);
    t.set_priority(uwuwu_domain::Priority::High);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_38").unwrap(),
    );
    t.apply_fields(f);
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((38, Status::InProgress));
    // луп-гвард на эскалации: 5 повторов без прогресса статуса
    state
        .0
        .lock()
        .unwrap()
        .loops
        .push((38, Slot::Dev, Status::InProgress, 5));
    let (u, board, sessions, _) = peak_usecase(vec![t], Vec::new(), state);
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_38".into());
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 0, "эскалация — не будильник");
    assert_eq!(out.spawns_done, 0, "эскалация — не спавн");
    let log = board.log.lock().unwrap();
    assert!(
        log.iter()
            .any(|l| l.starts_with("status#38=NeedsAttention")),
        "строка ушла в Needs attention: {log:?}"
    );
}

#[tokio::test]
async fn respawns_within_tick_do_not_oversell_cup() {
    // Low-находка ревью: global_active — снапшот обхода; несколько мёртвых
    // stage-строк в одном тике не оверселяют кап — счётчик инкрементится
    // по факту спавна
    let mut rows = Vec::new();
    for n in [50u64, 51] {
        let mut t = task(n, Status::InProgress);
        let mut f = TaskFields::default();
        f.set_session(
            Slot::Dev,
            uwuwu_domain::SessionRef::parse(&format!("ses_dead_{n}")).unwrap(),
        );
        t.apply_fields(f);
        rows.push(t);
    }
    rows.push(task(52, Status::InProgress));
    // тот же раунд (prev=In Progress) — дед-строки идут респавн-путём,
    // не кик-разморозкой
    let state = Arc::new(MemState(Mutex::default()));
    {
        let mut g = state.0.lock().unwrap();
        g.statuses.push((50, Status::InProgress));
        g.statuses.push((51, Status::InProgress));
    }
    let (u, _, sessions, _) = usecase_with_state(rows, PrState::Missing, state);
    {
        let mut dead = sessions.dead_sessions.lock().unwrap();
        dead.push("ses_dead_50".into());
        dead.push("ses_dead_51".into());
    }
    // cup=2: снапшот active=1 (только кандидат), но респавны инкрементят
    // счётчик — второй респавн и кандидат упираются в кап
    let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out.spawns_done, 1, "один спавн в слот капа");
    assert_eq!(out.held_cup, 1, "кандидат удержан честным счётчиком");
    assert_eq!(out.wakeups_deferred, 1, "второй респавн отложен CUP");
}

#[tokio::test]
async fn undelivered_comment_is_not_marked_seen() {
    // #1206 (гонка доставки): коммент, созданный во время работы агента
    // (активность новее коммента), не доставлен — и seen помечать нельзя:
    // пометка навсегда прячет коммент от будильника (агент ждёт
    // ревьювера, ревьювер — агента). После завершения хода коммент
    // доставляется следующим тиком.
    let mut t = task(60, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_60").unwrap(),
    );
    t.apply_fields(f);
    // агент «работает»: активность новее коммента → fresh=false
    let state = Arc::new(MemState(Mutex::default()));
    let (u, _, sessions, _) = {
        let board = Arc::new(FakeBoard {
            rows: vec![t],
            log: Mutex::default(),
            comments: vec![CommentRef {
                id: "verdict".into(),
                created_ms: 3000,
                body: "вердикт".into(),
            }],
        });
        let sess = Arc::new(FakeSessions {
            prompts: Mutex::default(),
            spawned: Mutex::default(),
            activity_ms: 5000,
            dead_sessions: Mutex::default(),
            list_fails: false,
            idle_reply: false,
        });
        let prs = Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
        });
        let uc = TickUseCase {
            board: board.clone(),
            sessions: sess.clone(),
            prs: prs.clone(),
            clock: Arc::new(FixedClock),
            state: state.clone(),
            spawn_model: None,
        };
        (uc, board, sess, prs)
    };
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        sessions.prompts.lock().unwrap().is_empty(),
        "коммент старее активности — доставка откладывается"
    );
    assert!(
        state.0.lock().unwrap().comments.is_empty(),
        "недоставленный коммент не помечается seen — иначе потерян навсегда"
    );

    // агент завершил ход (активность застыла) — тот же seen-стейт:
    // следующий тик доставляет коммент
    let mut t2 = task(60, Status::InProgress);
    let mut f2 = TaskFields::default();
    f2.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_60").unwrap(),
    );
    t2.apply_fields(f2);
    let board = Arc::new(FakeBoard {
        rows: vec![t2],
        log: Mutex::default(),
        comments: vec![CommentRef {
            id: "verdict".into(),
            created_ms: 3000,
            body: "вердикт".into(),
        }],
    });
    let sess2 = Arc::new(FakeSessions {
        prompts: Mutex::default(),
        spawned: Mutex::default(),
        activity_ms: 1000,
        dead_sessions: Mutex::default(),
        list_fails: false,
        idle_reply: false,
    });
    let u2 = TickUseCase {
        board: board.clone(),
        sessions: sess2.clone(),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state,
        spawn_model: None,
    };
    let out = u2.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 1, "коммент доставлен после завершения хода");
    assert!(!sess2.prompts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn multi_comment_tail_delivered_together() {
    // #1206 Common-1: несколько комментов за один непрерывный ход агента
    // (вердикт ревьювера + ответ владельца) доставляются одним промптом —
    // доставка только последнего поглощала промежуточные навсегда
    let mut t = task(61, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_61").unwrap(),
    );
    t.apply_fields(f);
    // seen = агентский коммент "A": после него накопились два непрочитанных
    let state = Arc::new(MemState(Mutex::default()));
    state.0.lock().unwrap().comments.push("A".into());
    let board = Arc::new(FakeBoard {
        rows: vec![t],
        log: Mutex::default(),
        comments: vec![
            CommentRef {
                id: "A".into(),
                created_ms: 900,
                body: "агентский отчёт".into(),
            },
            CommentRef {
                id: "c1".into(),
                created_ms: 950,
                body: "вердикт ревьювера".into(),
            },
            CommentRef {
                id: "c2".into(),
                created_ms: 980,
                body: "ответ владельца".into(),
            },
        ],
    });
    let sess = Arc::new(FakeSessions {
        prompts: Mutex::default(),
        spawned: Mutex::default(),
        activity_ms: 900,
        dead_sessions: Mutex::default(),
        list_fails: false,
        idle_reply: false,
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: sess.clone(),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state: state.clone(),
        spawn_model: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 1);
    let prompts = sess.prompts.lock().unwrap();
    let sent = prompts.join("\n");
    assert!(
        sent.contains("вердикт ревьювера") && sent.contains("ответ владельца"),
        "хвост доставлен целиком одним промптом: {prompts:?}"
    );
    // seen продвинулся на последний коммент хвоста
    assert_eq!(
        state.0.lock().unwrap().comments.last().map(String::as_str),
        Some("c2")
    );
}

#[tokio::test]
async fn inflight_comment_delivered_after_agent_reply() {
    // High-1 #1206: коммент создан ВО ВРЕМЯ хода агента (активность новее
    // коммента — fresh=false навсегда, активность монотонна). После ответа
    // агента (последнее сообщение — финальный ассистентский) хвост
    // доставляется немедленно, без часового сталла
    let mut t = task(62, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_62").unwrap(),
    );
    t.apply_fields(f);
    let state = Arc::new(MemState(Mutex::default()));
    state.0.lock().unwrap().comments.push("A".into());
    let board = Arc::new(FakeBoard {
        rows: vec![t],
        log: Mutex::default(),
        comments: vec![CommentRef {
            id: "verdict".into(),
            created_ms: 3000,
            body: "вердикт ревьювера".into(),
        }],
    });
    // агент ответил ПОСЛЕ коммента: activity 5000 > created 3000 — но ход
    // завершён (idle_reply: последнее сообщение — финальный ответ)
    let sess = Arc::new(FakeSessions {
        activity_ms: 5000,
        idle_reply: true,
        ..Default::default()
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: sess.clone(),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state,
        spawn_model: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 1, "in-flight коммент доставлен после ответа");
    assert!(sess
        .prompts
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.contains("вердикт ревьювера")));
}

#[tokio::test]
async fn busy_session_not_pinged_early() {
    // ход идёт (последнее сообщение — не ответ агента): мягкий порог не
    // срабатывает, классический сталл ждёт свой час (#161 — длинные
    // операции не пингуем)
    struct BigClock;
    impl Clock for BigClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            100_000_000
        }
    }
    let mut t = task(63, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_63").unwrap(),
    );
    t.apply_fields(f);
    let board = Arc::new(FakeBoard {
        rows: vec![t],
        log: Mutex::default(),
        comments: Vec::default(),
    });
    let sess = Arc::new(FakeSessions {
        // молчание ~30 мин (tool-call висит), idle_reply = false
        activity_ms: 100_000_000 - 30 * 60_000,
        idle_reply: false,
        ..Default::default()
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: sess.clone(),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(MemState(Mutex::default())),
        spawn_model: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.stall_pings + out.soft_pings,
        0,
        "busy-сессия не пингуется (ход идёт)"
    );
    assert!(sess.prompts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn soft_ping_wakes_idle_session_without_transaction() {
    // Санкция владельца 16.11: сессия завершила активность, статус не
    // переведён → мягкий пинг ПРЯМЫМ промптом в сессию через 15 мин
    struct BigClock;
    impl Clock for BigClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            100_000_000
        }
    }
    let mut t = task(64, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_64").unwrap(),
    );
    t.apply_fields(f);
    let board = Arc::new(FakeBoard {
        rows: vec![t],
        log: Mutex::default(),
        comments: Vec::default(),
    });
    let sess = Arc::new(FakeSessions {
        // молчание 16 мин, ход завершён (idle)
        activity_ms: 100_000_000 - 16 * 60_000,
        idle_reply: true,
        ..Default::default()
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: sess.clone(),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(MemState(Mutex::default())),
        spawn_model: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.soft_pings, 1, "мягкий пинг ушёл в сессию");
    assert!(sess
        .prompts
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.contains("Мягкий пинг")));
}

#[tokio::test]
async fn spawn_prompt_demands_finish_transaction() {
    // Дыра промптинга (владелец 16.11): контракт хода обязан быть в промпте
    // спавна — «засыпает после ответа» + два легальных финала хода
    let board = BoardPlan {
        board: "B".into(),
        project: "p".into(),
        repo: "a/c".into(),
        checkout: "/tmp/checkout".into(),
        env_root: "/tmp/env".into(),
    };
    let task = Task::new(7, "t".into(), Status::Design);
    let prompt = crate::pipeline::actions::spawn_prompt(&board, &task, Slot::Design);
    assert!(prompt.contains("ЗАСЫПАЕТ"), "промпт объясняет сон сессии");
    assert!(
        prompt.contains("Ревью плана"),
        "финал хода назван для стадии"
    );
    assert!(
        prompt.contains("Needs attention"),
        "вопрос владельцу — легальный финал"
    );
}

#[tokio::test]
async fn dev_session_survives_all_transitions() {
    // Канон сессий: дев-сессия едина между этапами и откатами —
    // переходы стадий её никогда не снимают
    let state = Arc::new(MemState(Mutex::default()));
    for status in [
        Status::Design,
        Status::ReviewPlan,
        Status::InProgress,
        Status::ReviewResult,
    ] {
        let mut t = task(32, status);
        let mut f = TaskFields::default();
        f.set_session(
            Slot::Dev,
            uwuwu_domain::SessionRef::parse("ses_dev_forever").unwrap(),
        );
        t.apply_fields(f);
        let (u, board, _, _) = usecase_with_state(vec![t], PrState::Missing, state.clone());
        u.run(&plan(Mode::Live, 7)).await.unwrap();
        let mut log = board.log.lock().unwrap();
        assert!(
            !log.iter()
                .any(|l| l.starts_with("clear#32=") && l.contains("Dev")),
            "dev-ячейка неприкосновенна ({status:?}): {log:?}"
        );
        log.clear();
    }
}

#[tokio::test]
async fn failed_spawn_does_not_charge_loop_or_cooldown() {
    // 15.11, #1151 (high): временный отказ спавна (бюджет 10/ч) не должен
    // заряжать луп-счётчик (3 отказа = ложная эскалация) и кулдаун слота —
    // заряжаем только состоявшийся спавн
    struct FailingSessions;
    #[async_trait::async_trait]
    impl AgentSessionGateway for FailingSessions {
        async fn create_session(&self, _s: &SessionSpec) -> UsecaseResult<String> {
            Err(crate::error::UsecaseError::Session(
                "бюджет исчерпан (мок)".into(),
            ))
        }
        async fn prompt(&self, _s: &str, _t: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn run_command(&self, _s: &str, _n: &str, _a: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
            Ok(())
        }
        async fn liveness(&self, _s: &str) -> Liveness {
            Liveness::Alive
        }
        async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
            Ok(None)
        }
        async fn last_assistant_reply(
            &self,
            _s: &str,
        ) -> UsecaseResult<Option<crate::ports::AssistantReply>> {
            Ok(None)
        }
        async fn list_sessions(&self) -> UsecaseResult<Vec<crate::ports::SessionInfo>> {
            Ok(vec![])
        }
        async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
            Ok(String::new())
        }
    }
    let state = Arc::new(MemState(Mutex::default()));
    let (mut u, _, _, _) = usecase(vec![task(40, Status::InProgress)], PrState::Missing);
    u.sessions = Arc::new(FailingSessions);
    u.state = state.clone();
    for round in 0..4 {
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        assert_eq!(out.spawns_done, 0, "круг {round}: спавн не состоялся");
        assert_eq!(out.attention_flagged, 0, "круг {round}: без эскалации");
    }
    assert!(
        state.0.lock().unwrap().loops.is_empty(),
        "failed-спавны не заряжают луп-счётчик"
    );
}

// ===== Аналитика (#1135): волновой сайдкар + парный ревьювер =====

use crate::pipeline::analyst::{AnalystBoard, AnalystUseCase};
use crate::pipeline::analyst_review::AnalystReviewUseCase;
use uwuwu_domain::{Agent, AgentSlot};

fn analyst_board() -> AnalystBoard {
    AnalystBoard {
        project: "p".into(),
        board: "B".into(),
        checkout: "/tmp/checkout".into(),
    }
}

fn analyst_uc(
    rows: Vec<Task>,
    state: Arc<MemState>,
    sessions: Arc<dyn AgentSessionGateway>,
) -> (AnalystUseCase, Arc<FakeBoard>) {
    let board = Arc::new(FakeBoard {
        rows,
        log: Mutex::default(),
    });
    let u = AnalystUseCase {
        board: board.clone(),
        boards: vec![analyst_board()],
        sessions,
        state,
        clock: Arc::new(FixedClock),
    };
    (u, board)
}

fn review_uc(
    rows: Vec<Task>,
    state: Arc<MemState>,
    sessions: Arc<dyn AgentSessionGateway>,
) -> (AnalystReviewUseCase, Arc<FakeBoard>) {
    let board = Arc::new(FakeBoard {
        rows,
        log: Mutex::default(),
    });
    let u = AnalystReviewUseCase {
        board: board.clone(),
        boards: vec![analyst_board()],
        sessions,
        state,
        clock: Arc::new(FixedClock),
    };
    (u, board)
}

fn titled(n: u64, title: &str, status: Status) -> Task {
    Task::new(n, title.into(), status)
}

#[tokio::test]
async fn analyst_wave_one_session_for_all_rows() {
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    let (u, board) = analyst_uc(
        vec![
            titled(1, "первая", Status::Analys),
            titled(2, "вторая", Status::Analys),
            titled(3, "уже в пайпе", Status::Todo),
        ],
        state.clone(),
        sessions.clone(),
    );
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 1, "N строк Analys → одна сессия волны");
    assert_eq!(sessions.spawned.lock().unwrap().len(), 1);
    let specs = sessions.specs.lock().unwrap();
    assert!(matches!(specs[0].agent, AgentSlot::Role(Agent::Analyst)));
    assert_eq!(
        specs[0].directory, "/tmp/checkout",
        "мастер-чекаут, без env"
    );
    assert!(specs[0].model.is_none(), "модель несёт деф-файл");
    // Todo-строка мимо волны; обе Analys-строки в пачке, счётчик первой попытки
    let inner = state.0.lock().unwrap();
    assert_eq!(inner.analys_loops.len(), 2, "только строки волны");
    for (n, st, r) in &inner.analys_loops {
        assert_eq!(*st, Status::Analys);
        assert_eq!(*r, 1, "строка #{n}: первая попытка");
    }
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("status#")),
        "волна не двигает статусы сама"
    );
}

#[tokio::test]
async fn analyst_wave_orders_by_priority() {
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    let mut low = titled(1, "низкий", Status::Analys);
    low.set_priority(uwuwu_domain::Priority::Low);
    let mut high = titled(2, "высокий", Status::Analys);
    high.set_priority(uwuwu_domain::Priority::High);
    let (u, _) = analyst_uc(vec![low, high], state, sessions.clone());
    u.tick().await.unwrap();
    // порядок пачки: high раньше low в промпте
    let specs = sessions.specs.lock().unwrap();
    assert_eq!(specs.len(), 1);
    let prompts = sessions.prompts.lock().unwrap();
    let p = &prompts[0];
    assert!(
        p.find("#2").unwrap() < p.find("#1").unwrap(),
        "high раньше low: {p}"
    );
}

#[tokio::test]
async fn analyst_alive_skip_and_parity() {
    // живой аналитик — волна ждёт
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::with_live("analyst"));
    let (u, _) = analyst_uc(
        vec![titled(1, "a", Status::Analys)],
        state,
        sessions.clone(),
    );
    assert_eq!(u.tick().await.unwrap(), 0, "живой аналитик — спавна нет");
    assert!(sessions.spawned.lock().unwrap().is_empty());
    // живой ревьювер волну аналитика НЕ блокирует (парность ролей)
    let state2 = Arc::new(MemState(Mutex::default()));
    let sessions2 = Arc::new(FakeSessions::with_live("analyst-reviewer"));
    let (u2, _) = analyst_uc(vec![titled(1, "a", Status::Analys)], state2, sessions2);
    assert_eq!(u2.tick().await.unwrap(), 1);
}

#[tokio::test]
async fn analyst_skips_cron_and_empty_titles() {
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = analyst_uc(
        vec![
            titled(1, "⏰ крон · ран", Status::Analys),
            titled(2, "", Status::Analys),
        ],
        state,
        sessions.clone(),
    );
    assert_eq!(u.tick().await.unwrap(), 0, "⏰ и пустой тайтл — мимо волны");
    assert!(sessions.spawned.lock().unwrap().is_empty());
}

#[tokio::test]
async fn analyst_cooldown_holds_row() {
    let inner = MemStateInner {
        analys_ms: vec![(1, 2000)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = analyst_uc(
        vec![titled(1, "свежая", Status::Analys)],
        state,
        sessions.clone(),
    );
    assert_eq!(u.tick().await.unwrap(), 0, "кулдаун 15 минут держит строку");
    assert!(sessions.spawned.lock().unwrap().is_empty());
}

#[tokio::test]
async fn analyst_escalates_after_three_cycles() {
    let inner = MemStateInner {
        analys_loops: vec![(1, Status::Analys, 3)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let (u, board) = analyst_uc(
        vec![titled(1, "закольцованная", Status::Analys)],
        state.clone(),
        sessions.clone(),
    );
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 0, "четвёртая волна не спавнится — эскалация");
    assert!(sessions.spawned.lock().unwrap().is_empty());
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|l| l == "status#1=NeedsAttention"),
        "строка в Needs attention: {:?}",
        *log
    );
    assert!(
        log.iter().any(|l| l.starts_with("comment#1=")),
        "коммент владельцу"
    );
    assert!(
        state.0.lock().unwrap().analys_loops.is_empty(),
        "счётчик после эскалации сброшен"
    );
}

#[tokio::test]
async fn analyst_resets_stale_cycle_counter() {
    // строка уехала из цикла (Todo) — счётчик кругов устарел
    let inner = MemStateInner {
        analys_loops: vec![(2, Status::Analys, 2)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = analyst_uc(
        vec![titled(2, "в пайпе", Status::Todo)],
        state.clone(),
        sessions,
    );
    u.tick().await.unwrap();
    assert!(
        state.0.lock().unwrap().analys_loops.is_empty(),
        "вне цикла Analys↔Review Analys счётчик сбрасывается"
    );
}

#[tokio::test]
async fn reviewer_spawns_for_review_analys_wave() {
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = review_uc(
        vec![titled(1, "проработанная", Status::ReviewAnalys)],
        state,
        sessions.clone(),
    );
    assert_eq!(u.tick().await.unwrap(), 1);
    let specs = sessions.specs.lock().unwrap();
    assert!(matches!(
        specs[0].agent,
        AgentSlot::Role(Agent::AnalystReviewer)
    ));
    assert!(specs[0].model.is_none(), "модель несёт деф-файл");
}

#[tokio::test]
async fn reviewer_alive_skip_and_cooldown() {
    // парность: живой ревьювер — спавна нет
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::with_live("analyst-reviewer"));
    let (u, _) = review_uc(vec![titled(1, "x", Status::ReviewAnalys)], state, sessions);
    assert_eq!(u.tick().await.unwrap(), 0);
    // кулдаун по строке
    let inner = MemStateInner {
        analys_review_ms: vec![(1, 2000)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = review_uc(vec![titled(1, "x", Status::ReviewAnalys)], state, sessions);
    assert_eq!(
        u.tick().await.unwrap(),
        0,
        "ретрай умершей сессии — не чаще 15 мин"
    );
}

#[tokio::test]
async fn full_circle_wave_then_review() {
    // полный круг на моках: create-эквивалент (Analys) → волна аналитика →
    // «аналитик» перевёл в Review Analys → ревьювер → approve → Todo
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    let (a, aboard) = analyst_uc(
        vec![titled(7, "заявка агента", Status::Analys)],
        state.clone(),
        sessions.clone(),
    );
    assert_eq!(a.tick().await.unwrap(), 1, "волна забрала заявку");
    // «аналитик» отработал: проработано → Review Analys
    aboard
        .set_status("B", 7, Status::ReviewAnalys)
        .await
        .unwrap();
    assert_eq!(sessions.spawned.lock().unwrap().len(), 1);
    // повторный тик аналитика: строка уже в Review Analys — волна пуста
    assert_eq!(a.tick().await.unwrap(), 0);
    // ревьювер забирает волну
    let (r, rboard) = review_uc(
        vec![titled(7, "заявка агента", Status::ReviewAnalys)],
        state.clone(),
        sessions.clone(),
    );
    assert_eq!(r.tick().await.unwrap(), 1, "парная сессия ревьювера");
    // «ревьювер» approve → Todo
    rboard.set_status("B", 7, Status::Todo).await.unwrap();
    assert_eq!(
        sessions.spawned.lock().unwrap().len(),
        2,
        "аналитик + ревьювер: одна сессия на роль"
    );
    // следующий тик ревьювера: очередь пуста
    assert_eq!(r.tick().await.unwrap(), 0);
}

// ===== Дельта ревью PR #343: charge-after-spawn, гвард ревью-контура, мультиборд =====

/// Гейтвей с падающим create_session (сбой спавна).
struct FailingCreate;

#[async_trait::async_trait]
impl AgentSessionGateway for FailingCreate {
    async fn create_session(&self, _s: &SessionSpec) -> UsecaseResult<String> {
        Err(crate::error::UsecaseError::Session(
            "бюджет исчерпан (мок)".into(),
        ))
    }
    async fn prompt(&self, _s: &str, _t: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn run_command(&self, _s: &str, _n: &str, _a: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn delete_session(&self, _s: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, _s: &str) -> Liveness {
        Liveness::Alive
    }
    async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
        Ok(None)
    }
    async fn last_assistant_reply(
        &self,
        _s: &str,
    ) -> UsecaseResult<Option<crate::ports::AssistantReply>> {
        Ok(None)
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<crate::ports::SessionInfo>> {
        Ok(vec![])
    }
    async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
}

/// Доски с раздельными строками (мультиборд-кейс C3).
struct FakeMultiBoard {
    boards: Vec<(String, Vec<Task>)>,
    /// доски, чтение которых падает (кейс prune при частичном сбое)
    fail_reads: Vec<String>,
    log: Mutex<Vec<String>>,
}

#[async_trait]
impl TaskBoardRepo for FakeMultiBoard {
    async fn read_board(&self, board: &str) -> UsecaseResult<Vec<Task>> {
        if self.fail_reads.iter().any(|b| b == board) {
            return Err(crate::error::UsecaseError::Board(format!(
                "{board}: GitHub API (мок)"
            )));
        }
        Ok(self
            .boards
            .iter()
            .find(|(b, _)| b == board)
            .map(|(_, rows)| rows.clone())
            .unwrap_or_default())
    }
    async fn set_status(&self, _b: &str, n: u64, s: Status) -> UsecaseResult<()> {
        self.log.lock().unwrap().push(format!("status#{n}={s:?}"));
        Ok(())
    }
    async fn set_session(&self, _b: &str, _n: u64, _s: Slot, _sess: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn clear_session(&self, _b: &str, _n: u64, _s: Slot) -> UsecaseResult<()> {
        Ok(())
    }
    async fn issue_fields(&self, _n: u64) -> UsecaseResult<TaskFields> {
        Ok(TaskFields::default())
    }
    async fn post_comment(&self, n: u64, _body: &str) -> UsecaseResult<()> {
        self.log.lock().unwrap().push(format!("comment#{n}"));
        Ok(())
    }
    async fn close_issue(&self, _n: u64) -> UsecaseResult<()> {
        Ok(())
    }
    async fn latest_comments(&self, _n: u64) -> UsecaseResult<Vec<CommentRef>> {
        Ok(vec![])
    }
    async fn read_issue(&self, _b: &str, n: u64) -> UsecaseResult<crate::ports::IssueView> {
        let task = self
            .boards
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|t| t.number() == n)
            .cloned()
            .unwrap_or_else(|| Task::new(n, String::new(), Status::Empty));
        Ok(crate::ports::IssueView {
            task,
            body: String::new(),
            comments: vec![],
        })
    }
    async fn create_issue(
        &self,
        _b: &str,
        _p: &str,
        _title: &str,
        _body: &str,
    ) -> UsecaseResult<u64> {
        Ok(1)
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

#[tokio::test]
async fn analyst_failed_spawn_does_not_charge_cycle_or_cooldown() {
    // канон tick_tests failed_spawn_does_not_charge_loop_or_cooldown:
    // сбой гейтвея не сжигает круг волны и не заряжает кулдаун строки
    let state = Arc::new(MemState(Mutex::default()));
    let (u, _) = analyst_uc(
        vec![titled(1, "a", Status::Analys)],
        state.clone(),
        Arc::new(FailingCreate),
    );
    assert!(u.tick().await.is_err(), "гейтвей упал — тик с ошибкой");
    let inner = state.0.lock().unwrap();
    assert!(inner.analys_loops.is_empty(), "круг волны не сожжён");
    assert!(inner.analys_ms.is_empty(), "кулдаун не заряжен");
}

#[tokio::test]
async fn reviewer_failed_spawn_does_not_charge_loop_or_cooldown() {
    let state = Arc::new(MemState(Mutex::default()));
    let (u, _) = review_uc(
        vec![titled(1, "x", Status::ReviewAnalys)],
        state.clone(),
        Arc::new(FailingCreate),
    );
    assert!(u.tick().await.is_err(), "гейтвей упал — тик с ошибкой");
    let inner = state.0.lock().unwrap();
    assert!(
        inner.analys_review_loops.is_empty(),
        "попытка ревью не сожжена"
    );
    assert!(inner.analys_review_ms.is_empty(), "кулдаун не заряжен");
}

#[tokio::test]
async fn reviewer_loop_escalates_repeated_spawns() {
    // R2-C2: ревьювер-спавны при неизменном статусе — SpawnLoop (порог >4,
    // канон слотовых спавнов); систематически умирающий ревьювер не ретраится вечно
    let inner = MemStateInner {
        analys_review_loops: vec![(1, Status::ReviewAnalys, 4)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let (u, board) = review_uc(
        vec![titled(1, "застрявшая", Status::ReviewAnalys)],
        state.clone(),
        sessions.clone(),
    );
    assert_eq!(
        u.tick().await.unwrap(),
        0,
        "пятый спавн запрещён — эскалация"
    );
    assert!(sessions.spawned.lock().unwrap().is_empty());
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|l| l == "status#1=NeedsAttention"),
        "{:?}",
        *log
    );
    assert!(log.iter().any(|l| l.starts_with("comment#1")));
    assert!(
        state.0.lock().unwrap().analys_review_loops.is_empty(),
        "счётчик после эскалации сброшен"
    );
}

#[tokio::test]
async fn analyst_wave_per_board_own_checkout_and_project() {
    // R2-C3: чекаут/проект/методичка — своей доски, не первой строки волны
    let sessions = Arc::new(FakeSessions::new());
    let board = Arc::new(FakeMultiBoard {
        boards: vec![
            ("B1".into(), vec![titled(1, "проект-один", Status::Analys)]),
            ("B2".into(), vec![titled(2, "проект-два", Status::Analys)]),
        ],
        fail_reads: vec![],
        log: Mutex::default(),
    });
    let u = AnalystUseCase {
        board: board.clone(),
        boards: vec![
            AnalystBoard {
                project: "proj-1".into(),
                board: "B1".into(),
                checkout: "/checkout-1".into(),
            },
            AnalystBoard {
                project: "proj-2".into(),
                board: "B2".into(),
                checkout: "/checkout-2".into(),
            },
        ],
        sessions: sessions.clone(),
        state: Arc::new(MemState(Mutex::default())),
        clock: Arc::new(FixedClock),
    };
    assert_eq!(u.tick().await.unwrap(), 2, "по сессии на доску");
    let specs = sessions.specs.lock().unwrap();
    assert_eq!(specs[0].directory, "/checkout-1");
    assert_eq!(specs[1].directory, "/checkout-2");
    let prompts = sessions.prompts.lock().unwrap();
    assert!(prompts[0].contains("proj-1"), "{}", prompts[0]);
    assert!(prompts[0].contains("#1"));
    assert!(prompts[1].contains("proj-2"), "{}", prompts[1]);
    assert!(prompts[1].contains("#2"));
}

#[tokio::test]
async fn analyst_prunes_loops_of_gone_rows() {
    // R2-L3: строка удалена с доски — счётчик из state вычищается
    let inner = MemStateInner {
        analys_loops: vec![(9, Status::Analys, 1)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = analyst_uc(
        vec![titled(1, "живая", Status::Analys)],
        state.clone(),
        sessions,
    );
    u.tick().await.unwrap();
    let inner = state.0.lock().unwrap();
    assert!(
        !inner.analys_loops.iter().any(|(n, _, _)| *n == 9),
        "счётчик мёртвой строки вычищен: {:?}",
        inner.analys_loops
    );
    assert!(
        inner.analys_loops.iter().any(|(n, _, _)| *n == 1),
        "счётчик живой строки на месте"
    );
}

// ===== Дельта ревью R2: пары гвардов не разъезжаются, сброс на выходе, prune обоих =====

#[tokio::test]
async fn reviewer_guard_pairs_survive_priority_sort() {
    // R2-H1: сортировка волны до построения гвардов — счётчик пишется
    // своей строке. Доска отдаёт [#2 (Empty, свежая), #1 (High, 1 повтор)]:
    // после сортировки волна [#1, #2], счётчики — по 2 и 1 соответственно.
    let inner = MemStateInner {
        analys_review_loops: vec![(1, Status::ReviewAnalys, 1)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let mut low = titled(2, "пустой приоритет", Status::ReviewAnalys);
    low.set_priority(uwuwu_domain::Priority::Empty);
    let mut high = titled(1, "высокий", Status::ReviewAnalys);
    high.set_priority(uwuwu_domain::Priority::High);
    // порядок строк доски: low первым, high вторым — сортировка переставит
    let (u, _) = review_uc(
        vec![low, high],
        state.clone(),
        Arc::new(FakeSessions::new()),
    );
    assert_eq!(u.tick().await.unwrap(), 1);
    let inner = state.0.lock().unwrap();
    let got = |n: u64| {
        inner
            .analys_review_loops
            .iter()
            .find(|(num, _, _)| *num == n)
            .map(|(_, _, r)| *r)
    };
    assert_eq!(got(1), Some(2), "своя пара: 1+1=2");
    assert_eq!(got(2), Some(1), "своя пара: 0+1=1");
}

#[tokio::test]
async fn reviewer_guard_resets_when_row_leaves_review_analys() {
    // R2-C1: легитимный перезаход (ревьювер вернул в Analys) — счётчик
    // ревью-спавнов с нуля, ложной эскалации дорабатываемых строк нет
    let inner = MemStateInner {
        analys_review_loops: vec![(5, Status::ReviewAnalys, 3)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = analyst_uc(
        vec![titled(5, "на доработке", Status::Analys)],
        state.clone(),
        sessions,
    );
    u.tick().await.unwrap();
    assert!(
        state.0.lock().unwrap().analys_review_loops.is_empty(),
        "счётчик ревью-спавнов сброшен при выходе из Review Analys"
    );
}

#[tokio::test]
async fn reviewer_guard_survives_while_row_in_review_analys() {
    // живой умирающий ревьювер: строка НЕ покидала Review Analys —
    // счётчик копится (гвард R1-C2 работает)
    let inner = MemStateInner {
        analys_review_loops: vec![(7, Status::ReviewAnalys, 2)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = analyst_uc(
        vec![titled(7, "ждёт ревью", Status::ReviewAnalys)],
        state.clone(),
        sessions,
    );
    u.tick().await.unwrap();
    assert_eq!(
        state.0.lock().unwrap().analys_review_loops.len(),
        1,
        "строка в Review Analys — счётчик не тронут"
    );
}

#[tokio::test]
async fn analyst_prunes_review_loops_of_gone_rows() {
    // R2-L1: review-loop ключи ушедших строк вычищаются вместе с аналитическими
    let inner = MemStateInner {
        analys_review_loops: vec![(9, Status::ReviewAnalys, 1)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = analyst_uc(
        vec![titled(1, "живая", Status::Analys)],
        state.clone(),
        sessions,
    );
    u.tick().await.unwrap();
    assert!(
        state.0.lock().unwrap().analys_review_loops.is_empty(),
        "мёртвый review-loop вычищен"
    );
}

#[tokio::test]
async fn prune_skipped_when_a_board_read_fails() {
    // дельта R3-L2: непрочитанная доска → её строки не в live-наборе;
    // prune откладывается, счётчики живых строк доски не стираются
    let inner = MemStateInner {
        analys_loops: vec![(1, Status::Analys, 2)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let board = Arc::new(FakeMultiBoard {
        // #1 в Review Analys: мимо волны и мимо stale-сброса — тик изме­няет
        // только prune, который при сбое B2 откладывается
        boards: vec![
            (
                "B1".into(),
                vec![titled(1, "живая застрявшая", Status::ReviewAnalys)],
            ),
            ("B2".into(), vec![titled(2, "живая вторая", Status::Analys)]),
        ],
        fail_reads: vec!["B2".into()],
        log: Mutex::default(),
    });
    let u = AnalystUseCase {
        board: board.clone(),
        boards: vec![
            AnalystBoard {
                project: "p".into(),
                board: "B1".into(),
                checkout: "/c1".into(),
            },
            AnalystBoard {
                project: "p".into(),
                board: "B2".into(),
                checkout: "/c2".into(),
            },
        ],
        sessions,
        state: state.clone(),
        clock: Arc::new(FixedClock),
    };
    u.tick().await.unwrap();
    assert!(
        state
            .0
            .lock()
            .unwrap()
            .analys_loops
            .iter()
            .any(|(n, _, r)| *n == 1 && *r == 2),
        "счётчик живой строки непрочитанной доски не стёрт"
    );
}

/// Заглушка issue-полей: close-путь полей не пишет.
struct FakeFields;

#[async_trait]
impl crate::ports::IssueFieldStore for FakeFields {
    async fn store(&self, _: u64, _: &[(&str, Option<String>)]) -> UsecaseResult<()> {
        Ok(())
    }
    async fn load(&self, _: u64) -> UsecaseResult<std::collections::HashMap<String, String>> {
        Ok(Default::default())
    }
}

/// Доска с per-board валидацией мутаций (как живой GitHubHub: set_status
/// на номер, не живущий на доске — громкая ошибка «строка не на доске»).
struct CloseBoard {
    boards: Vec<(String, Vec<u64>)>,
    log: Mutex<Vec<String>>,
}

impl CloseBoard {
    fn board_of(&self, board: &str) -> UsecaseResult<&Vec<u64>> {
        self.boards
            .iter()
            .find(|(b, _)| b == board)
            .map(|(_, rows)| rows)
            .ok_or_else(|| crate::error::UsecaseError::Board(format!("нет доски {board}")))
    }
}

#[async_trait]
impl TaskBoardRepo for CloseBoard {
    async fn read_board(&self, board: &str) -> UsecaseResult<Vec<Task>> {
        let rows = self.board_of(board)?;
        Ok(rows
            .iter()
            .map(|n| Task::new(*n, format!("строка {n}"), Status::Todo))
            .collect())
    }
    async fn set_status(&self, b: &str, n: u64, s: Status) -> UsecaseResult<()> {
        if !self.board_of(b)?.contains(&n) {
            return Err(crate::error::UsecaseError::Board(format!(
                "строка #{n} не на доске «{b}»"
            )));
        }
        self.log.lock().unwrap().push(format!("status#{n}={s:?}"));
        Ok(())
    }
    async fn set_session(&self, _: &str, _: u64, _: Slot, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn clear_session(&self, _: &str, _: u64, _: Slot) -> UsecaseResult<()> {
        Ok(())
    }
    async fn issue_fields(&self, _: u64) -> UsecaseResult<TaskFields> {
        Ok(TaskFields::default())
    }
    async fn post_comment(&self, _: u64, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn close_issue(&self, _: u64) -> UsecaseResult<()> {
        Ok(())
    }
    async fn latest_comments(&self, _: u64) -> UsecaseResult<Vec<CommentRef>> {
        Ok(vec![])
    }
    async fn read_issue(&self, _: &str, _: u64) -> UsecaseResult<crate::ports::IssueView> {
        Err(crate::error::UsecaseError::Board("мок".into()))
    }
    async fn create_issue(&self, _: &str, _: &str, _: &str, _: &str) -> UsecaseResult<u64> {
        Ok(1)
    }
    async fn rename_issue(&self, _: u64, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn patch_cells(&self, _: u64, _: crate::ports::FmMutator) -> UsecaseResult<()> {
        Ok(())
    }
    async fn close_issue_ext(&self, _: u64, _: bool) -> UsecaseResult<()> {
        Ok(())
    }
    async fn replace_body(&self, _: u64, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn task_close_resolves_board_of_the_row_multiboard() {
    // регресс-пин #1135 (живой прогон): вердикт аналитика «дубль/мусор →
    // task_close not_planned» обязан работать на любой доске волны
    // (мультиборд C3), а не только на первой доске конфига. Живой факт:
    // close(number) резолвит первую доску конфига и падает «строка #N
    // не на доске «Задачи uwuwu»» для строк второй доски — дубль
    // остаётся в Analys, цикл-гвард эскалирует владельцу.
    // Ожидаемое поведение: close резолвит доску строки (или идемпотентно
    // Done на всех досках конфига) и закрывает строку второй доски.
    let board = Arc::new(CloseBoard {
        boards: vec![("B1".into(), vec![1]), ("B2".into(), vec![5])],
        log: Mutex::default(),
    });
    let fields: Arc<dyn crate::ports::IssueFieldStore> = Arc::new(FakeFields);
    let writer = crate::tasks::write::TaskWriteUseCase {
        board: board.clone(),
        fields,
        boards: vec![("uwuwu".into(), "B1".into()), ("origa".into(), "B2".into())],
    };
    use crate::tasks::write::TaskWriter;
    writer
        .close(5, true)
        .await
        .expect("close строки второй доски волны — вердикт аналитика «дубль»");
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|e| e == "status#5=Done"),
        "строка дубля на второй доске переведена в Done, лог: {log:?}"
    );
    assert!(
        log.iter().all(|e| !e.contains("status#1=")),
        "строки первой доски close не трогает, лог: {log:?}"
    );
}
