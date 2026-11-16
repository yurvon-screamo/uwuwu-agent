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
        Ok(vec![])
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

struct FakeSessions {
    prompts: Mutex<Vec<String>>,
    spawned: Mutex<Vec<String>>,
    /// Спеки create_session (контракты спавна: агент/модель/каталог).
    specs: Mutex<Vec<SessionSpec>>,
    /// ms последней активности любой сессии (сталл-детект-тесты).
    activity_ms: u64,
}

#[async_trait]
impl AgentSessionGateway for FakeSessions {
    async fn create_session(&self, spec: &SessionSpec) -> UsecaseResult<String> {
        let id = format!("ses_{}", self.spawned.lock().unwrap().len() + 1);
        self.spawned.lock().unwrap().push(id.clone());
        self.specs.lock().unwrap().push(spec.clone());
        Ok(id)
    }
    async fn prompt(&self, session: &str, text: &str) -> UsecaseResult<()> {
        self.prompts
            .lock()
            .unwrap()
            .push(format!("{session}:{}", text.len()));
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
        Ok(Some(self.activity_ms))
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
    resets: Vec<(u64, Status)>,
    /// сталл-детект: (number, статус раунда, ms пинга)
    stalls: Vec<(u64, Status, u64)>,
}
struct MemState(Mutex<MemStateInner>);
impl TickStateStore for MemState {
    fn last_comment_id(&self, _b: &str, _n: u64) -> Option<String> {
        None
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
    fn last_triage_ms(&self, _b: &str, _n: u64) -> Option<u64> {
        None
    }
    fn mark_triage_spawn(&self, _b: &str, _n: u64, _at: u64) {}
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
    });
    let sessions = Arc::new(FakeSessions {
        prompts: Mutex::default(),
        spawned: Mutex::default(),
        specs: Mutex::default(),
        activity_ms: 1000,
    });
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
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
    };
    (u, board, sessions, prs)
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

#[tokio::test]
async fn stall_detector_pings_then_flags() {
    // Паттерн 16.11 (#1151/#1135/#384): ход stage-сессии завершён
    // (тишина), статус не переведён — тик пингует агента комментом,
    // после двух пингов и дальнейшей тишины — Needs attention.
    use crate::pipeline::tick::{STALL_PING_EVERY_MS, STALL_SILENCE_MS};
    use std::sync::atomic::{AtomicU64, Ordering};
    let now: u64 = 1_000_000_000;
    let mut t = task(21, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_live").unwrap(),
    );
    t.apply_fields(f);
    let board = Arc::new(FakeBoard {
        rows: vec![t],
        log: Mutex::default(),
    });
    let sessions = Arc::new(FakeSessions {
        prompts: Mutex::default(),
        spawned: Mutex::default(),
        specs: Mutex::default(),
        // сессия молчит час+ — ход завершён без статусного перевода
        activity_ms: now - STALL_SILENCE_MS - 60_000,
    });
    struct AdvClock(AtomicU64);
    impl Clock for AdvClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }
    let clock = Arc::new(AdvClock(AtomicU64::new(now)));
    let u = TickUseCase {
        board: board.clone(),
        sessions,
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
        }),
        clock: clock.clone(),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
    };
    // тик 1: тишина > часа → первый автопинг
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.stall_pings, 1, "первый автопинг");
    assert!(board
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("comment#21=")));
    // тик 2 сразу: интервал 2ч не прошёл — без пинга
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.stall_pings, 0, "интервал пинга не прошёл");
    // +2ч: второй пинг
    clock
        .0
        .fetch_add(STALL_PING_EVERY_MS + 60_000, Ordering::SeqCst);
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.stall_pings, 1, "второй автопинг");
    // +2ч ещё: эскалация — Needs attention + коммент владельцу
    clock
        .0
        .fetch_add(STALL_PING_EVERY_MS + 60_000, Ordering::SeqCst);
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.stall_flagged, 1, "эскалация сталла");
    assert!(board
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("status#21=NeedsAttention")));
    // сразу после эскалации — без дубли (интервал держит)
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.stall_flagged + out.stall_pings,
        0,
        "без дублей эскалации"
    );
}

#[tokio::test]
async fn stage_spawn_carries_model_from_server_map() {
    // #1154 (решение владельца 17.11): модель спавна — карта сервера
    // (SpawnModels), каждый спавн несёт её в теле сессии; деф-файлы
    // агентов моделей не несут. Dev-стадия — default-ключ карты.
    // Arrange
    let (u, _, sessions, _) = usecase(vec![task(1, Status::InProgress)], PrState::Missing);

    // Act
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();

    // Assert
    assert_eq!(out.spawns_done, 1, "спавн стадии состоялся");
    let specs = sessions.specs.lock().unwrap();
    assert_eq!(specs.len(), 2, "bootstrap + work");
    let want = uwuwu_domain::SpawnModel::parse("zai-coding-plan/glm-5.3").unwrap();
    for spec in specs.iter() {
        assert_eq!(spec.model, want.model, "модель спавна = default карты");
        assert_eq!(spec.variant, None, "default — без варианта");
    }
}
