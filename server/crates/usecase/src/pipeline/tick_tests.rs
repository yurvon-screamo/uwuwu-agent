//! Юнит-тесты тика на in-memory фейках портов (тест-стратегия L2:
//! юзкейсы — с фейками, без сети/досок).

use crate::error::{UsecaseError, UsecaseResult};
use crate::pipeline::tick::{BoardPlan, Mode, TickPlan, TickUseCase, PROBE_DEGRADE_MIN_ROWS};
use crate::ports::{
    AgentSessionGateway, Clock, CommentRef, DecisionJudge, DreamerMark, JudgeCfg, Liveness,
    PrGateway, PrState, QuotaProbe, SessionSpec, TaskBoardRepo, TickStateStore,
};
use crate::router::SessionRouter;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uwuwu_domain::{
    Agent, BranchName, PeakWindow, PrUrl, Priority, Slot, SpawnPolicy, Status, Task, TaskFields,
    UtcMinute,
};

struct FakeBoard {
    /// Mutex — тест-хореографии волн (#1412) добавляют строки между тиками
    /// (кандидаты приходят волнами, как ночные волны конвейера).
    rows: std::sync::Mutex<Vec<Task>>,
    log: Mutex<Vec<String>>,
    /// Журнал комментов задач (будильник-сценарии: latest_comments).
    comments: Vec<CommentRef>,
    /// Отказ enrichment (issue_fields) — skip-строка тика (#1163).
    fields_fails: bool,
    /// Отказ set_session (записи ячейки) — сирота-зачистка (#1163).
    set_session_fails: bool,
    /// Отказ clear_session (снятия ячейки) — краш-окно гашения (#1412).
    clear_session_fails: AtomicBool,
    /// Отказ set_status (эскалационный ретрай: счётчик стоит на капе —
    /// #1311, ревью L2).
    status_fails: AtomicBool,
}

#[async_trait]
impl TaskBoardRepo for FakeBoard {
    async fn read_board(&self, _board: &str) -> UsecaseResult<Vec<Task>> {
        Ok(self.rows.lock().unwrap().clone())
    }
    async fn set_status(
        &self,
        _b: &str,
        n: u64,
        s: Status,
    ) -> UsecaseResult<crate::ports::StatusDelivery> {
        if self.status_fails.load(Ordering::Relaxed) {
            return Err(UsecaseError::Session("set_status недоступен (мок)".into()));
        }
        self.log.lock().unwrap().push(format!("status#{n}={s:?}"));
        Ok(crate::ports::StatusDelivery::Confirmed {
            actual: crate::naming::status_name(s).to_string(),
        })
    }
    async fn row_status(&self, _b: &str, n: u64) -> UsecaseResult<Option<Status>> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.number() == n)
            .map(|t| t.status()))
    }
    async fn row_exists(&self, _b: &str, n: u64) -> UsecaseResult<bool> {
        Ok(self.rows.lock().unwrap().iter().any(|t| t.number() == n))
    }
    async fn set_session(&self, _b: &str, n: u64, _slot: Slot, s: &str) -> UsecaseResult<()> {
        if self.set_session_fails {
            return Err(UsecaseError::Session("set_session недоступен (мок)".into()));
        }
        self.log.lock().unwrap().push(format!("session#{n}={s}"));
        Ok(())
    }
    async fn set_branch(
        &self,
        _b: &str,
        n: u64,
        branch: &uwuwu_domain::BranchName,
    ) -> UsecaseResult<()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("branch#{n}={}", branch.as_str()));
        Ok(())
    }
    async fn set_board_cells(
        &self,
        _b: &str,
        _n: u64,
        _p: Option<&str>,
        _a: Option<bool>,
        _d: Option<bool>,
    ) -> UsecaseResult<()> {
        Ok(())
    }

    async fn clear_session(&self, _b: &str, n: u64, slot: Slot) -> UsecaseResult<()> {
        if self.clear_session_fails.load(Ordering::Relaxed) {
            return Err(UsecaseError::Session(
                "clear_session недоступен (мок)".into(),
            ));
        }
        self.log
            .lock()
            .unwrap()
            .push(format!("clear#{n}={:?}", slot));
        Ok(())
    }
    async fn issue_fields(&self, n: u64) -> UsecaseResult<uwuwu_domain::TaskFields> {
        // Отказ enrichment (#1163): строка тиком пропускается
        if self.fields_fails {
            return Err(UsecaseError::Session("fields недоступны (мок)".into()));
        }
        // enrichment-фейк: поля несут строки (как настоящий hub из REST)
        Ok(self
            .rows
            .lock()
            .unwrap()
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
                    Slot::TestReview,
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
                    // зеркально мапперу носителя (#1155): ветка есть —
                    // кодовый PullRequest, нет — безветочный артефакт
                    if f.branch().is_some() {
                        let _ = f.open_pr(p);
                    } else {
                        f.set_artifact(p);
                    }
                }
                f
            })
            .unwrap_or_default())
    }
    async fn post_comment(&self, n: u64, body: &str) -> UsecaseResult<String> {
        let mut log = self.log.lock().unwrap();
        let id = format!("posted-{}", log.len() + 1);
        log.push(format!("comment#{n}={body}"));
        Ok(id)
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
            .lock()
            .unwrap()
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
        _kind: Option<&str>,
    ) -> UsecaseResult<u64> {
        let n = self.rows.lock().unwrap().len() as u64 + 100;
        self.log.lock().unwrap().push(format!("create:{title}"));
        Ok(n)
    }
    async fn set_issue_type(&self, _n: u64, _kind: &str) -> UsecaseResult<()> {
        Ok(())
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
    async fn archive_row(&self, _b: &str, _n: u64) -> UsecaseResult<()> {
        Ok(())
    }
    async fn issues_with_title(&self, _title: &str) -> UsecaseResult<Vec<u64>> {
        Ok(Vec::new())
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
    /// ms последней активности любой сессии (сталл-детект/wake-тесты);
    /// None — активность недоступна (fail-open own-теста #1204).
    activity_ms: Option<u64>,
    /// Id мёртвых сессий (liveness-сценарии кик-разморозки/респавна).
    dead_sessions: Mutex<Vec<String>>,
    /// Недоступность list_sessions (fallback-сценарий CUP-подсчёта).
    list_fails: bool,
    /// Последнее сообщение сессии — финальный ответ агента (idle).
    idle_reply: bool,
    /// спеки созданных сессий (агент/директория — для ассертов)
    specs: Mutex<Vec<SessionSpec>>,
    /// Живые сессии исполнителя — слепки для second source (#1163):
    /// синтетические роли аналитики и матрица гарда (директория/финал/след).
    live: Mutex<Vec<LiveSighting>>,
    /// Сессии с ходом в полёте (busy по union-скану, #1231).
    busy_ids: Mutex<Vec<String>>,
    /// Синтетические записи union-скана (регрессы busy-учёта: сессии 2+
    /// хода, непайплайн-агенты) — попадают в список как есть.
    extra: Mutex<Vec<crate::ports::SessionInfo>>,
    /// Отказ prompt (сирота спавна #1163).
    prompt_fails: bool,
    /// Сессии, доставка в которые отклоняется 404 (dead-session #1288).
    prompt_lost: Mutex<Vec<String>>,
    /// 404 на доставку в ЛЮБУЮ сессию (T2: свежие сессии респавна тоже).
    prompt_lost_all: bool,
    /// Журнал удалений сессий (ассерты сирота-зачистки).
    deleted: Mutex<Vec<String>>,
    /// Сессии с недостоверной пробой активности (Err) — срез честности
    /// пробы #1281: изоляция построчная, вердикт тишины не выдаётся.
    activity_fails_sessions: Mutex<Vec<String>>,
    /// Гейт недоступен целиком (окно заглушки/рестарт) — liveness Unknown
    /// на все сессии (#1281 срез 4: эскалация по недостоверной пробе).
    liveness_unknown: bool,
    /// Журнал абортов сессий (преемпшен #1259).
    aborted: Mutex<Vec<String>>,
    /// Журнал пинов агента слота (#1300): `{session}|{agent}`.
    switched: Mutex<Vec<String>>,
    /// Отказ abort_session (гашение не удалось — кандидат остаётся held).
    abort_fails: AtomicBool,
    /// Бюджетная проба spawn_allowed (false — преемпшен не начинается).
    spawn_probe: AtomicBool,
}

/// Слепок живой сессии в фейке: (id, agent, directory, outcome, updated).
/// total_tokens — кандидатность дримера (токены > 0 = реальная работа).
#[derive(Clone)]
struct LiveSighting {
    id: String,
    agent: String,
    directory: String,
    outcome: String,
    updated_ms: u64,
    total_tokens: u64,
}

impl FakeSessions {
    fn new() -> Self {
        Self {
            prompts: Mutex::default(),
            spawned: Mutex::default(),
            specs: Mutex::default(),
            live: Mutex::default(),
            activity_ms: Some(1000),
            dead_sessions: Mutex::default(),
            list_fails: false,
            idle_reply: false,
            busy_ids: Mutex::default(),
            extra: Mutex::default(),
            prompt_fails: false,
            prompt_lost: Mutex::default(),
            prompt_lost_all: false,
            deleted: Mutex::default(),
            activity_fails_sessions: Mutex::default(),
            liveness_unknown: false,
            aborted: Mutex::default(),
            switched: Mutex::default(),
            abort_fails: AtomicBool::new(false),
            spawn_probe: AtomicBool::new(true),
        }
    }
    fn with_live(agent: &str) -> Self {
        Self {
            live: Mutex::new(vec![LiveSighting {
                id: format!("ses_live_{agent}"),
                agent: agent.into(),
                directory: String::new(),
                outcome: String::new(),
                updated_ms: 0,
                total_tokens: 0,
            }]),
            ..Self::new()
        }
    }
    /// Живая сессия в env-директории строки — сценарий сироты/клейма (#1163).
    fn with_env_session(
        id: &str,
        directory: &str,
        agent: &str,
        outcome: &str,
        updated_ms: u64,
    ) -> Self {
        Self {
            live: Mutex::new(vec![LiveSighting {
                id: id.into(),
                agent: agent.into(),
                directory: directory.into(),
                outcome: outcome.into(),
                updated_ms,
                total_tokens: 0,
            }]),
            ..Self::new()
        }
    }
    /// Завершённая сессия с финалом и токенами — дример-кандидат.
    fn with_finished(agent: &str, outcome: &str) -> Self {
        Self {
            live: Mutex::new(vec![LiveSighting {
                id: format!("ses_live_{agent}"),
                agent: agent.into(),
                directory: String::new(),
                outcome: outcome.into(),
                updated_ms: 0,
                total_tokens: 1000,
            }]),
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
        let lost = self
            .prompt_lost
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == session);
        if lost || self.prompt_lost_all {
            return Err(UsecaseError::SessionNotFound(format!(
                "prompt {session}: HTTP 404 Not Found (мок #1288)"
            )));
        }
        if self.prompt_fails {
            return Err(UsecaseError::Session("prompt недоступен (мок)".into()));
        }
        self.prompts
            .lock()
            .unwrap()
            .push(format!("{session}|{text}"));
        Ok(())
    }
    async fn run_command(&self, _s: &str, _n: &str, _a: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn liveness(&self, s: &str) -> Liveness {
        if self.liveness_unknown {
            return Liveness::Unknown;
        }
        if self.dead_sessions.lock().unwrap().iter().any(|d| d == s) {
            Liveness::Dead
        } else {
            Liveness::Alive
        }
    }
    async fn delete_session(&self, s: &str) -> UsecaseResult<()> {
        self.deleted.lock().unwrap().push(s.into());
        Ok(())
    }
    async fn abort_session(&self, s: &str) -> UsecaseResult<()> {
        if self.abort_fails.load(Ordering::SeqCst) {
            return Err(UsecaseError::Session("abort недоступен (мок)".into()));
        }
        self.aborted.lock().unwrap().push(s.into());
        Ok(())
    }
    async fn switch_agent(&self, s: &str, agent: Agent) -> UsecaseResult<()> {
        self.switched
            .lock()
            .unwrap()
            .push(format!("{s}|{}", crate::agent_name(agent)));
        Ok(())
    }
    fn spawn_allowed(&self) -> bool {
        self.spawn_probe.load(Ordering::SeqCst)
    }
    async fn last_activity_ms(&self, s: &str) -> UsecaseResult<Option<u64>> {
        if self
            .activity_fails_sessions
            .lock()
            .unwrap()
            .iter()
            .any(|x| x == s)
        {
            return Err(UsecaseError::Session(
                "activity probe недоступен (мок #1281)".into(),
            ));
        }
        Ok(self.activity_ms)
    }
    async fn last_assistant_reply(
        &self,
        _s: &str,
    ) -> UsecaseResult<Option<crate::ports::AssistantReply>> {
        Ok(self.idle_reply.then(|| crate::ports::AssistantReply {
            created_ms: self.activity_ms.unwrap_or_default(),
            text: String::new(),
            finished: true,
            is_latest: true,
            finish: Some("stop".into()),
        }))
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<crate::ports::SessionInfo>> {
        if self.list_fails {
            return Err(crate::error::UsecaseError::Session(
                "list недоступен (мок)".into(),
            ));
        }
        // spawned = созданные тиком сессии (idle между ходами — кап не
        // держат, #1244), мёртвые вне списка: list и GET согласованы,
        // сессия, которую serve 404-ит, в list не попадает (контракт
        // second source #1163); live = синтетические роли для alive-гвардов
        // тестов аналитики (#1135) и матрица гарда спавна (#1163);
        // extra = записи union-скана (#1231).
        let dead = self.dead_sessions.lock().unwrap().clone();
        let busy = self.busy_ids.lock().unwrap().clone();
        let mut out: Vec<crate::ports::SessionInfo> = self
            .spawned
            .lock()
            .unwrap()
            .iter()
            .filter(|id| !dead.contains(*id))
            .map(|id| crate::ports::SessionInfo {
                id: id.clone(),
                agent: String::new(),
                directory: String::new(),
                updated_ms: 0,
                outcome: String::new(),
                total_tokens: 0,
                busy: busy.contains(id),
            })
            .collect();
        for s in self.live.lock().unwrap().iter() {
            let id = s.id.clone();
            out.push(crate::ports::SessionInfo {
                busy: busy.contains(&id),
                id,
                agent: s.agent.clone(),
                directory: s.directory.clone(),
                updated_ms: s.updated_ms,
                outcome: s.outcome.clone(),
                total_tokens: s.total_tokens,
            });
        }
        out.extend(self.extra.lock().unwrap().iter().cloned());
        Ok(out)
    }
    async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
}

struct FakePrs {
    state: PrState,
    merges: Mutex<Vec<String>>,
    /// URL-аргументы probe — фикс «артефакт доезжает до gateway» (#1155)
    probes: Mutex<Vec<Option<String>>>,
}

#[async_trait]
impl PrGateway for FakePrs {
    async fn probe(&self, _r: &str, u: Option<&str>, _b: Option<&str>) -> UsecaseResult<PrState> {
        self.probes.lock().unwrap().push(u.map(str::to_string));
        Ok(self.state)
    }
    async fn merge(&self, _r: &str, pr: &str) -> UsecaseResult<()> {
        self.merges.lock().unwrap().push(pr.into());
        Ok(())
    }
}

pub(crate) struct FixedClock;
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
    /// Метки дример-обработки (dreamer-сценарии тиков).
    dreamer_marks: BTreeMap<String, DreamerMark>,
    /// Связки «исходная → дример-сессия» (wakeup-сценарии, #1306).
    dreamer_links: BTreeMap<String, String>,
    /// Дневной счётчик дримера: (дата JST, спавнов).
    dreamer_day: Option<(String, u32)>,
    /// (number, slot) → (status, repeats)
    loops: Vec<(u64, Slot, Status, u32)>,
    /// (number, rounds) — цикл-гвард ревью-контура (#1311)
    review_rounds: Vec<(u64, u32)>,
    /// (number, status, rounds) — цикл-гвард аналитики (#1135)
    analys_loops: Vec<(u64, Status, u32)>,
    /// (number, status, repeats) — цикл-гвард ревью-контура
    analys_review_loops: Vec<(u64, Status, u32)>,
    /// (number, ts) — кулдауны волн аналитика/ревьювера
    /// (board, number, ts) — ключи борд-скоупные, как в реальном store
    /// ({board}:{number}:analys); number-only маскировал мультиборд-оверселл
    analys_ms: Vec<(String, u64, u64)>,
    analys_review_ms: Vec<(String, u64, u64)>,
    resets: Vec<(u64, Status)>,
    /// сталл-детект: (number, статус раунда, ms пинга)
    stalls: Vec<(u64, Status, u64)>,
    /// потерянные доставки: (number, статус раунда) — счёт по журналу (#1288)
    lost: Vec<(u64, Status)>,
    /// mark_spawn-журнал: (number, slot, ms) — кулдаун-сценарии (#1163)
    spawn_ms: Vec<(u64, Slot, u64)>,
    /// реестр серверных постов (own-тест self-эха #1204)
    own: BTreeMap<String, u64>,
}
pub(crate) struct MemState(Mutex<MemStateInner>);

impl MemState {
    /// Пустое состояние: no-op-реестры для тестов вне тика (write/т.п.).
    pub(crate) fn empty() -> Self {
        Self(Mutex::default())
    }
}
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
    fn dreamer_mark(&self, s: &str) -> Option<DreamerMark> {
        self.0.lock().unwrap().dreamer_marks.get(s).copied()
    }
    fn mark_dreamer(&self, s: &str, mark: DreamerMark) {
        self.0.lock().unwrap().dreamer_marks.insert(s.into(), mark);
    }
    fn dreamer_session(&self, s: &str) -> Option<String> {
        self.0.lock().unwrap().dreamer_links.get(s).cloned()
    }
    fn link_dreamer_session(&self, s: &str, d: &str) {
        self.0
            .lock()
            .unwrap()
            .dreamer_links
            .insert(s.into(), d.into());
    }
    fn prune_dreamer(&self, live: &std::collections::HashSet<String>) {
        let mut g = self.0.lock().unwrap();
        g.dreamer_marks.retain(|id, _| live.contains(id));
        g.dreamer_links.retain(|source, _| live.contains(source));
    }
    fn dreamer_day_spawns(&self, day: &str) -> u32 {
        self.0
            .lock()
            .unwrap()
            .dreamer_day
            .as_ref()
            .filter(|(d, _)| d == day)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }
    fn mark_dreamer_spawn(&self, day: &str) {
        let mut g = self.0.lock().unwrap();
        let n = g
            .dreamer_day
            .as_ref()
            .filter(|(d, _)| d == day)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        g.dreamer_day = Some((day.into(), n + 1));
    }
    fn last_spawn_ms(&self, _b: &str, n: u64, s: Slot) -> Option<u64> {
        self.0
            .lock()
            .unwrap()
            .spawn_ms
            .iter()
            .rev()
            .find(|(num, slot, _)| *num == n && *slot == s)
            .map(|(_, _, ts)| *ts)
    }
    fn mark_spawn(&self, _b: &str, n: u64, s: Slot, at: u64) {
        self.0.lock().unwrap().spawn_ms.push((n, s, at));
    }
    fn clear_spawn_ms(&self, _b: &str, n: u64, s: Slot) {
        self.0
            .lock()
            .unwrap()
            .spawn_ms
            .retain(|(num, slot, _)| !(*num == n && *slot == s));
    }
    fn last_analys_ms(&self, b: &str, n: u64) -> Option<u64> {
        self.0
            .lock()
            .unwrap()
            .analys_ms
            .iter()
            .rev()
            .find(|(board, num, _)| board == b && *num == n)
            .map(|(_, _, ts)| *ts)
    }
    fn mark_analys_spawn(&self, b: &str, n: u64, at: u64) {
        self.0.lock().unwrap().analys_ms.push((b.into(), n, at));
    }
    fn last_analys_review_ms(&self, b: &str, n: u64) -> Option<u64> {
        self.0
            .lock()
            .unwrap()
            .analys_review_ms
            .iter()
            .rev()
            .find(|(board, num, _)| board == b && *num == n)
            .map(|(_, _, ts)| *ts)
    }
    fn mark_analys_review_spawn(&self, b: &str, n: u64, at: u64) {
        self.0
            .lock()
            .unwrap()
            .analys_review_ms
            .push((b.into(), n, at));
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
    fn clear_stall(&self, _b: &str, n: u64) {
        self.0.lock().unwrap().stalls.retain(|(num, ..)| *num != n);
    }
    fn lost_session(&self, _b: &str, n: u64) -> Option<(Status, u32)> {
        let g = self.0.lock().unwrap();
        let last = g.lost.iter().rev().find(|(num, _)| *num == n)?;
        let count = g
            .lost
            .iter()
            .filter(|(num, st)| *num == n && st == &last.1)
            .count() as u32;
        Some((last.1, count))
    }
    fn mark_lost_session(&self, _b: &str, n: u64, status: Status) {
        self.0.lock().unwrap().lost.push((n, status));
    }
    fn clear_lost_session(&self, _b: &str, n: u64) {
        self.0.lock().unwrap().lost.retain(|(num, _)| *num != n);
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
    fn clear_spawn_loop(&self, _b: &str, n: u64, s: Slot) {
        self.0
            .lock()
            .unwrap()
            .loops
            .retain(|(num, slot, _, _)| !(*num == n && *slot == s));
    }
    fn reset_stale_spawn_loops(&self, _b: &str, n: u64, current: Status) {
        let mut g = self.0.lock().unwrap();
        g.resets.push((n, current));
        g.loops
            .retain(|(num, _, st, _)| !(*num == n && *st != current));
    }
    fn review_rounds(&self, _b: &str, n: u64) -> u32 {
        self.0
            .lock()
            .unwrap()
            .review_rounds
            .iter()
            .rev()
            .find(|(num, _)| *num == n)
            .map(|(_, r)| *r)
            .unwrap_or(0)
    }
    fn set_review_rounds(&self, _b: &str, n: u64, rounds: u32) {
        let mut g = self.0.lock().unwrap();
        if let Some(e) = g.review_rounds.iter_mut().find(|(num, _)| *num == n) {
            e.1 = rounds;
        } else {
            g.review_rounds.push((n, rounds));
        }
    }
    fn own_comment(&self, id: &str) -> bool {
        self.0.lock().unwrap().own.contains_key(id)
    }
    fn mark_own_comment(&self, id: &str, posted_ms: u64) {
        self.0.lock().unwrap().own.insert(id.to_string(), posted_ms);
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
        rows: rows.into(),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(FakeSessions::new());
    let prs = Arc::new(FakePrs {
        state: pr,
        merges: Mutex::default(),
        probes: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: prs.clone(),
        clock: Arc::new(FixedClock),
        state,
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
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

/// Общий билдер тик-фикстуры: fake-компоненты + юзкейс (дедуп семейства
/// билдеров — ревью #1310 Low; в поле TickUseCase меняется одно место).
fn fixture_usecase(
    rows: Vec<Task>,
    comments: Vec<CommentRef>,
    state: Arc<MemState>,
    clock: Arc<dyn Clock>,
    sessions: FakeSessions,
) -> (TickUseCase, Arc<FakeBoard>, Arc<FakeSessions>, Arc<FakePrs>) {
    let board = Arc::new(FakeBoard {
        rows: rows.into(),
        log: Mutex::default(),
        comments,
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(sessions);
    let prs = Arc::new(FakePrs {
        state: PrState::Missing,
        merges: Mutex::default(),
        probes: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: prs.clone(),
        clock,
        state,
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    (u, board, sessions, prs)
}

/// Юзкейс в пик-окне с настраиваемыми комментами (будильник-сценарии).
fn peak_usecase(
    rows: Vec<Task>,
    comments: Vec<CommentRef>,
    state: Arc<MemState>,
) -> (TickUseCase, Arc<FakeBoard>, Arc<FakeSessions>, Arc<FakePrs>) {
    fixture_usecase(
        rows,
        comments,
        state,
        Arc::new(PeakClock),
        FakeSessions::new(),
    )
}

/// Юзкейс с готовым моком сессий (сценарии list_fails и пр.).
fn usecase_with_sessions(
    rows: Vec<Task>,
    pr: PrState,
    sessions: FakeSessions,
    state: Arc<MemState>,
) -> (TickUseCase, Arc<FakeBoard>, Arc<FakeSessions>, Arc<FakePrs>) {
    let board = Arc::new(FakeBoard {
        rows: rows.into(),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(sessions);
    let prs = Arc::new(FakePrs {
        state: pr,
        merges: Mutex::default(),
        probes: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: prs.clone(),
        clock: Arc::new(FixedClock),
        state,
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
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

/// Роутинг тика «проект → executor» (#1248): юзкейс на двух фейк-гейтвеях
/// (дефолт kisa + named redmi), проект → named — как memory-multi конфиг.
/// Возвращает юзкейс и оба гейтвея (default, named).
fn routed_usecase(
    rows: Vec<Task>,
    comments: Vec<CommentRef>,
    clock: Arc<dyn Clock>,
    default_gw: Arc<FakeSessions>,
    named_gw: Arc<FakeSessions>,
) -> TickUseCase {
    TickUseCase {
        board: Arc::new(FakeBoard {
            rows: rows.into(),
            log: Mutex::default(),
            comments,
            fields_fails: false,
            set_session_fails: false,
            clear_session_fails: AtomicBool::new(false),
            status_fails: AtomicBool::new(false),
        }),
        sessions: Arc::new(SessionRouter::new(
            "kisa",
            default_gw.clone(),
            Vec::new(),
            std::collections::HashMap::from([(
                "1xgames".to_string(),
                named_gw.clone() as Arc<dyn AgentSessionGateway>,
            )]),
        )),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock,
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    }
}

/// План тика с routed-доской (проект 1xgames).
fn routed_plan(mode: Mode) -> TickPlan {
    TickPlan {
        mode,
        policy: SpawnPolicy {
            cup: 7,
            peak: PeakWindow {
                enabled: false,
                start_min: 0,
                end_min: 0,
            },
        },
        boards: vec![BoardPlan {
            project: "1xgames".into(),
            board: "B".into(),
            repo: "a/c".into(),
            checkout: "/tmp/checkout".into(),
            env_root: "/tmp/env".into(),
        }],
    }
}

/// Пин lifecycle-роутинга (#1248): Design-спавн routed-проекта — create
/// (bootstrap и рабочая) И начальный prompt уходят в один гейтвей —
/// исполнителя проекта; дефолтный prompt не получил вовсе.
#[tokio::test]
async fn spawn_design_routes_create_and_initial_prompt_to_project_executor() {
    let default_gw = Arc::new(FakeSessions::new());
    let named_gw = Arc::new(FakeSessions::new());
    let u = routed_usecase(
        vec![task(438, Status::Design)],
        Vec::new(),
        Arc::new(FixedClock),
        default_gw.clone(),
        named_gw.clone(),
    );
    let out = u.run(&routed_plan(Mode::Live)).await.unwrap();
    assert_eq!(out.spawns_done, 1, "Design-строка заспавнена");
    // bootstrap + рабочая сессия — на исполнителе проекта
    assert_eq!(
        named_gw.specs.lock().unwrap().len(),
        2,
        "create_session на named-исполнителе (bootstrap + рабочая)"
    );
    assert!(
        default_gw.specs.lock().unwrap().is_empty(),
        "дефолт не создаёт сессии routed-проекта"
    );
    // начальный prompt рабочей сессии — на исполнителе проекта
    let named_prompts = named_gw.prompts.lock().unwrap();
    assert_eq!(
        named_prompts.len(),
        1,
        "начальный prompt на named-исполнителе, получено: {named_prompts:?}"
    );
    let (session, _) = named_prompts[0].split_once('|').expect("session|text");
    let named_ids = named_gw.spawned.lock().unwrap();
    assert!(
        named_ids.contains(&session.to_string()),
        "промптится сессия, созданная на том же исполнителе"
    );
    assert!(
        default_gw.prompts.lock().unwrap().is_empty(),
        "дефолтный гейтвей начальный prompt НЕ получил"
    );
}

/// Пин lifecycle-роутинга (#1248): будильник живой stage-сессии routed-проекта
/// ходит в гейтвей проекта; дефолтный prompt не получил.
#[tokio::test]
async fn wakeup_prompt_routes_to_project_executor() {
    let mut t = task(440, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_live").unwrap(),
    );
    t.apply_fields(f);
    let default_gw = Arc::new(FakeSessions::new());
    // активность сессии старше коммента — fresh-доставка
    let named_gw = Arc::new(FakeSessions {
        activity_ms: Some(1000),
        ..FakeSessions::new()
    });
    let u = routed_usecase(
        vec![t],
        vec![CommentRef {
            id: "c1".into(),
            created_ms: 3000,
            body: "свежий коммент".into(),
        }],
        Arc::new(FixedClock),
        default_gw.clone(),
        named_gw.clone(),
    );
    let out = u.run(&routed_plan(Mode::Live)).await.unwrap();
    assert_eq!(out.wakeups, 1, "будильник доставлен");
    let named_prompts = named_gw.prompts.lock().unwrap();
    assert!(
        named_prompts.iter().any(|p| p.contains("Wakeup")),
        "wakeup-prompt на гейтвее проекта: {named_prompts:?}"
    );
    assert!(
        default_gw.prompts.lock().unwrap().is_empty(),
        "дефолтный гейтвей wakeup-prompt НЕ получил"
    );
}

/// Пин lifecycle-роутинга (#1248): мягкий сталл-пинг (прямой промпт) уходит
/// в гейтвей проекта; дефолтный prompt не получил.
#[tokio::test]
async fn soft_ping_prompt_routes_to_project_executor() {
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
    let mut t = task(441, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_idle").unwrap(),
    );
    t.apply_fields(f);
    let default_gw = Arc::new(FakeSessions::new());
    // молчание 16 мин, ход завершён (idle) — мягкий пинг прямым промптом
    let named_gw = Arc::new(FakeSessions {
        activity_ms: Some(100_000_000 - 16 * 60_000),
        idle_reply: true,
        ..FakeSessions::new()
    });
    let u = routed_usecase(
        vec![t],
        Vec::new(),
        Arc::new(BigClock),
        default_gw.clone(),
        named_gw.clone(),
    );
    let out = u.run(&routed_plan(Mode::Live)).await.unwrap();
    assert_eq!(out.soft_pings, 1, "мягкий пинг ушёл");
    let named_prompts = named_gw.prompts.lock().unwrap();
    assert!(
        named_prompts.iter().any(|p| p.contains("Soft ping")),
        "soft-ping на гейтвее проекта: {named_prompts:?}"
    );
    // пин агента слота и на мягком пинге (#1300)
    let switched = named_gw.switched.lock().unwrap();
    assert!(
        switched.iter().any(|s| s == "ses_dev_idle|developer"),
        "soft-ping пинит developer: {switched:?}"
    );
    assert!(
        default_gw.prompts.lock().unwrap().is_empty(),
        "дефолтный гейтвей soft-ping НЕ получил"
    );
}

/// Задача In Progress с живой дев-сессией (wake-сценарии #1204).
fn dev_task(n: u64, session: &str) -> Task {
    let mut t = task(n, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(Slot::Dev, uwuwu_domain::SessionRef::parse(session).unwrap());
    t.apply_fields(f);
    t
}

/// Гейтвей с N busy-сессиями union-скана — харнес held_cup-пинов
/// сайдкаров (#1313): dreamer/analyst/analyst_review решают по busy.
/// outcome задаёт кандидатность филлеров для дримера (is_fresh_candidate).
pub(crate) fn gateway_with_busy_sessions(
    count: usize,
    outcome: &str,
) -> Arc<dyn AgentSessionGateway> {
    let sessions = FakeSessions::new();
    {
        let mut extra = sessions.extra.lock().unwrap();
        for i in 0..count {
            extra.push(crate::ports::SessionInfo {
                id: format!("ses_busy_{i}"),
                agent: String::new(),
                directory: String::new(),
                updated_ms: 0,
                outcome: outcome.to_string(),
                total_tokens: 100,
                busy: true,
            });
        }
    }
    Arc::new(sessions)
}

/// Доска с заданными строками — харнес контроль-ранов волн.
pub(crate) fn board_with_rows(rows: Vec<Task>) -> Arc<dyn TaskBoardRepo> {
    Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(rows),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    })
}

/// Чистый state тика для сайдкар-пинов.
pub(crate) fn empty_tick_state() -> Arc<dyn TickStateStore> {
    Arc::new(MemState(Mutex::default()))
}

/// Юзкейс будильника: настраиваемые комменты, активность и idle-фаза хода
/// (сценарии self-эха #1204).
fn wake_usecase(
    rows: Vec<Task>,
    comments: Vec<CommentRef>,
    activity_ms: Option<u64>,
    idle_reply: bool,
    state: Arc<MemState>,
) -> (TickUseCase, Arc<FakeSessions>, Arc<MemState>) {
    let board = Arc::new(FakeBoard {
        rows: rows.into(),
        log: Mutex::default(),
        comments,
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(FakeSessions {
        switched: Mutex::default(),
        prompts: Mutex::default(),
        spawned: Mutex::default(),
        activity_ms,
        dead_sessions: Mutex::default(),
        list_fails: false,
        idle_reply,
        busy_ids: Mutex::default(),
        extra: Mutex::default(),
        prompt_fails: false,
        prompt_lost: Mutex::default(),
        prompt_lost_all: false,
        deleted: Mutex::default(),
        specs: Mutex::default(),
        live: Mutex::default(),
        activity_fails_sessions: Mutex::default(),
        liveness_unknown: false,
        aborted: Mutex::default(),
        abort_fails: AtomicBool::new(false),
        spawn_probe: AtomicBool::new(true),
    });
    let u = TickUseCase {
        board,
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state: state.clone(),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    (u, sessions, state)
}

#[tokio::test]
async fn own_comment_of_holding_session_is_not_delivered_and_baseline_moves() {
    // #1204 (self-эхо): агент опубликовал коммент в свой ход, ход закрылся
    // (активность новее коммента) — эхо-будки нет, бейзлайн двигается тихо.
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_own_comment("own-echo", 10);
    // геометрия прод-эха: seen-бейзлайн стоит, хвост = свой коммент
    state.set_last_comment_id("Задачи uwuwu", 61, "old-seen");
    let (u, sessions, state) = wake_usecase(
        vec![dev_task(61, "ses_dev_61")],
        vec![
            CommentRef {
                id: "old-seen".into(),
                created_ms: 1000,
                body: "старое".into(),
            },
            CommentRef {
                id: "own-echo".into(),
                created_ms: 3000,
                body: "мой отчёт".into(),
            },
        ],
        Some(5000),
        true,
        state,
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 0, "свой коммент не будит");
    assert!(sessions.prompts.lock().unwrap().is_empty());
    assert_eq!(
        state.0.lock().unwrap().comments.last().map(String::as_str),
        Some("own-echo"),
        "бейзлайн двигается тихо — хвост не приедет в следующей доставке"
    );
}

#[tokio::test]
async fn own_comment_before_first_session_wake_is_sliced_without_baseline() {
    // #1204 (транзишн-класс NOTICED, live-репродакция 16.11): MCP-коммент,
    // опубликованный до спавна свежей сессии (seen-бейзлайна нет, активность
    // новее коммента — спавн и ход после поста), не доставляется
    // idle-фоллбеком: хвост целиком свой → тихий сдвиг бейзлайна.
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_own_comment("triage-echo", 10);
    let (u, sessions, state) = wake_usecase(
        vec![dev_task(61, "ses_dev_61")],
        vec![CommentRef {
            id: "triage-echo".into(),
            created_ms: 3000,
            body: "старый триаж-коммент".into(),
        }],
        Some(5000),
        true,
        state,
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 0, "коммент до спавна не будит свежую сессию");
    assert!(sessions.prompts.lock().unwrap().is_empty());
    assert_eq!(
        state.0.lock().unwrap().comments.last().map(String::as_str),
        Some("triage-echo"),
        "бейзлайн тихо встал на последний — эхо не вернётся следующим тиком"
    );
}

#[tokio::test]
async fn mixed_tail_delivers_foreign_comments_only() {
    // #1204: хвост из своего и чужого — чужой доставляется, свой срезается,
    // seen двигается на фактический последний (здесь — чужой).
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_own_comment("own-echo", 10);
    state.set_last_comment_id("Задачи uwuwu", 61, "old-seen");
    let (u, sessions, state) = wake_usecase(
        vec![dev_task(61, "ses_dev_61")],
        vec![
            CommentRef {
                id: "old-seen".into(),
                created_ms: 1000,
                body: "старое".into(),
            },
            CommentRef {
                id: "own-echo".into(),
                created_ms: 3000,
                body: "мой отчёт".into(),
            },
            CommentRef {
                id: "owner-reply".into(),
                created_ms: 7000,
                body: "ответ владельца".into(),
            },
        ],
        Some(5000),
        true,
        state,
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 1, "чужой будит");
    let sent = sessions.prompts.lock().unwrap().join("\n");
    assert!(sent.contains("ответ владельца"), "чужой в доставке: {sent}");
    assert!(!sent.contains("мой отчёт"), "свой срезан: {sent}");
    assert_eq!(
        state.0.lock().unwrap().comments.last().map(String::as_str),
        Some("owner-reply"),
        "seen на фактическом последнем"
    );
}

#[tokio::test]
async fn user_comment_wakes_holder_as_before() {
    // #1204 (регресс-гард): юзерский коммент мимо реестра — будит как
    // раньше, даже если создан в окно активности сессии.
    let state = Arc::new(MemState(Mutex::default()));
    let (u, sessions, _) = wake_usecase(
        vec![dev_task(61, "ses_dev_61")],
        vec![CommentRef {
            id: "user-q".into(),
            created_ms: 3000,
            body: "вопрос владельца".into(),
        }],
        Some(5000),
        true,
        state,
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 1);
    let sent = sessions.prompts.lock().unwrap().join("\n");
    assert!(sent.contains("вопрос владельца"));
}

#[tokio::test]
async fn fresh_agent_comment_of_another_stage_wakes_holder() {
    // #1204 (регресс-гард): вердикт другой стадии (в реестре — любой
    // MCP-пост, но свежее активности адресата) — foreign, будит.
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_own_comment("verdict", 10);
    let (u, sessions, _) = wake_usecase(
        vec![dev_task(61, "ses_dev_61")],
        vec![CommentRef {
            id: "verdict".into(),
            created_ms: 7000,
            body: "request_changes ревьювера".into(),
        }],
        Some(5000),
        true,
        state,
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 1);
    let sent = sessions.prompts.lock().unwrap().join("\n");
    assert!(sent.contains("request_changes"));
}

#[tokio::test]
async fn own_comment_mid_turn_with_live_journal_is_not_delivered() {
    // #1204 Low-2 (детерминированная часть): свой коммент в активном ходе
    // (журнал жив — активность ≥ коммента) не доставляется и seen не
    // двигает — доставка повторится после ответа (at-least-once).
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_own_comment("own-echo", 10);
    let (u, sessions, state) = wake_usecase(
        vec![dev_task(61, "ses_dev_61")],
        vec![CommentRef {
            id: "own-echo".into(),
            created_ms: 3000,
            body: "мой отчёт".into(),
        }],
        Some(5000),
        false,
        state,
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 0);
    assert!(sessions.prompts.lock().unwrap().is_empty());
    assert!(
        state.0.lock().unwrap().comments.is_empty(),
        "seen не двигается при работающем агенте — доставка повторится"
    );
}

#[tokio::test]
async fn unknown_activity_fails_open_and_delivers_tail() {
    // #1204 (fail-open): активность недоступна — срез не делается,
    // лишняя эхо-будка лучше потерянного коммента (семантика TS).
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_own_comment("own-echo", 10);
    let (u, sessions, state) = wake_usecase(
        vec![dev_task(61, "ses_dev_61")],
        vec![CommentRef {
            id: "own-echo".into(),
            created_ms: 3000,
            body: "мой отчёт".into(),
        }],
        None,
        true,
        state,
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 1, "без активности — доставляем всё");
    let sent = sessions.prompts.lock().unwrap().join("\n");
    assert!(sent.contains("мой отчёт"));
    assert_eq!(
        state.0.lock().unwrap().comments.last().map(String::as_str),
        Some("own-echo")
    );
}

/// Честная лента для comment-верификации (#1331): пост попадает в
/// контрольное чтение; прочие носители — пусто.
#[derive(Default)]
struct CommentBoard {
    comments: Mutex<Vec<CommentRef>>,
}

#[async_trait]
impl TaskBoardRepo for CommentBoard {
    async fn read_board(&self, _board: &str) -> UsecaseResult<Vec<Task>> {
        Ok(Vec::new())
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
    async fn set_board_cells(
        &self,
        _b: &str,
        _n: u64,
        _p: Option<&str>,
        _a: Option<bool>,
        _d: Option<bool>,
    ) -> UsecaseResult<()> {
        Ok(())
    }
    async fn row_status(&self, _b: &str, _n: u64) -> UsecaseResult<Option<Status>> {
        Ok(None)
    }
    // строк задач фейк не несёт — только комментарии (close на нём честно
    // отвечает «не на досках»)
    async fn row_exists(&self, _b: &str, _n: u64) -> UsecaseResult<bool> {
        Ok(false)
    }
    async fn set_session(&self, _b: &str, _n: u64, _slot: Slot, _s: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_branch(&self, _b: &str, _n: u64, _branch: &BranchName) -> UsecaseResult<()> {
        Ok(())
    }
    async fn clear_session(&self, _b: &str, _n: u64, _slot: Slot) -> UsecaseResult<()> {
        Ok(())
    }
    async fn post_comment(&self, _n: u64, body: &str) -> UsecaseResult<String> {
        let mut c = self.comments.lock().unwrap();
        let id = format!("fakec-{}", c.len() + 1);
        c.push(CommentRef {
            id: id.clone(),
            created_ms: 0,
            body: body.to_string(),
        });
        Ok(id)
    }
    async fn close_issue(&self, _n: u64) -> UsecaseResult<()> {
        Ok(())
    }
    async fn latest_comments(&self, _n: u64) -> UsecaseResult<Vec<CommentRef>> {
        Ok(self.comments.lock().unwrap().clone())
    }
    async fn read_issue(&self, _b: &str, n: u64) -> UsecaseResult<crate::ports::IssueView> {
        Ok(crate::ports::IssueView {
            task: Task::new(n, String::new(), Status::Empty),
            body: String::new(),
            comments: vec![],
        })
    }
    async fn issue_fields(&self, _n: u64) -> UsecaseResult<TaskFields> {
        Ok(TaskFields::default())
    }
    async fn create_issue(
        &self,
        _b: &str,
        _p: &str,
        _title: &str,
        _body: &str,
        _kind: Option<&str>,
    ) -> UsecaseResult<u64> {
        Ok(1)
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

#[tokio::test]
async fn mcp_comment_marks_registry_with_node_id() {
    // #1204 (Low-1 ревью): единственная точка маркировки — writer.comment()
    // кладёт node_id ответа поста в реестр; own-тест wake-пути режет эхо
    // именно по этому id. Лента фейка честная (пост попадает в контрольное
    // чтение #1331).
    let board = Arc::new(CommentBoard::default());
    let state = Arc::new(MemState(Mutex::default()));
    let writer = crate::tasks::write::TaskWriteUseCase {
        board: board.clone(),
        fields: Arc::new(FakeFields),
        boards: vec![("uwuwu".into(), "B".into())],
        clock: Arc::new(FixedClock),
        state: state.clone(),
    };
    use crate::tasks::write::TaskWriter;
    writer.comment(61, "мой отчёт", None).await.unwrap();
    assert!(
        state.own_comment("fakec-1"),
        "MCP-пост промаркирован node_id ответа"
    );
}

#[tokio::test]
async fn empty_node_id_post_is_not_marked() {
    // #1204 (ветка fail-open маркировки): пустой node_id (нестандартный
    // ответ POST) не попадает в реестр — коммент доставлен, срез не врёт.
    let board = Arc::new(CloseBoard {
        boards: vec![("B".into(), vec![61])],
        log: Mutex::default(),
    });
    let state = Arc::new(MemState(Mutex::default()));
    let writer = crate::tasks::write::TaskWriteUseCase {
        board: board.clone(),
        fields: Arc::new(FakeFields),
        boards: vec![("uwuwu".into(), "B".into())],
        clock: Arc::new(FixedClock),
        state: state.clone(),
    };
    use crate::tasks::write::TaskWriter;
    writer.comment(61, "текст", None).await.unwrap();
    assert!(
        !state.own_comment(""),
        "пустой id не маркируется — иначе empty-id класс давал бы ложные срезы"
    );
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

/// Пин агента слота в спавн-конверте (#1300): дев-сессия рождается с
/// явным `agent=developer` и моделью из developer-ключа карты моделей
/// (ключ "default" остаётся bootstrap-сессиям) — опора на ambient-дефолт
/// исполнителя подменяла агента дев-сессии (инцидент #1044).
#[tokio::test]
async fn dev_spawn_pins_the_developer_agent() {
    // Кастомная карта с разведёнными ключами default/developer — пин
    // перехода модели: рабочая дев-сессия берёт developer-ключ.
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![task(1, Status::InProgress)]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(FakeSessions::new());
    let mut map = std::collections::HashMap::new();
    map.insert(
        "default".to_string(),
        uwuwu_domain::SpawnModel::parse("p/default").unwrap(),
    );
    map.insert(
        "developer".to_string(),
        uwuwu_domain::SpawnModel::parse("p/dev").unwrap(),
    );
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::new(map)),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 1);
    let specs = sessions.specs.lock().unwrap();
    assert_eq!(specs.len(), 2, "bootstrap + work");
    let boot = &specs[0];
    let work = &specs[1];
    assert!(
        matches!(boot.agent, AgentSlot::Default),
        "bootstrap-сессия остаётся без агент-пина"
    );
    assert_eq!(boot.model.id(), "default", "bootstrap — default-ключ карты");
    assert!(
        matches!(work.agent, AgentSlot::Role(Agent::Developer)),
        "дев-сессия рождается с явным developer, получено {:?}",
        work.agent
    );
    assert_eq!(work.model.provider(), "p");
    assert_eq!(
        work.model.id(),
        "dev",
        "модель дева — из developer-ключа карты (переход с default-ключа осознанный)"
    );
}

/// Тайтл спавн-сессии несёт эмодзи-маркер стадии между номером и тайтлом
/// (задача #1212); bootstrap наследует тем же значением.
#[tokio::test]
async fn spawn_title_carries_stage_marker() {
    // Dev-слот (In Progress): маркер 🔨, bootstrap и work — одинаково
    let (u, _, sessions, _) = usecase(vec![task(1, Status::InProgress)], PrState::Missing);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    {
        let specs = sessions.specs.lock().unwrap();
        assert_eq!(specs.len(), 2, "bootstrap + work");
        assert_eq!(specs[0].title, "bootstrap #1 🔨 t1");
        assert_eq!(specs[1].title, "#1 🔨 t1");
    }

    // PlanReview-слот: маркер 🔍 и агент plan-reviewer
    let (u, _, sessions, _) = usecase(vec![task(2, Status::ReviewPlan)], PrState::Missing);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    let specs = sessions.specs.lock().unwrap();
    let work = specs.last().expect("рабочая сессия");
    assert_eq!(work.title, "#2 🔍 t2");
    assert!(
        matches!(work.agent, AgentSlot::Role(Agent::PlanReviewer)),
        "ревью-план спавнится кастомным агентом"
    );
}

#[tokio::test]
async fn live_spawn_writes_branch_cell_from_worktree_name() {
    // #1166: фабрика пишет Branch при спавне — PR-финализация без ручного
    // `--branch` перестаёт рендериться «—» (рид гейтится веткой).
    let (u, board, _sessions, _) = usecase(vec![task(7, Status::InProgress)], PrState::Missing);
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 1);
    let log = board.log.lock().unwrap().clone();
    let branch = log
        .iter()
        .find(|l| l.starts_with("branch#7="))
        .expect("Branch-ячейка записана фабрикой");
    assert_eq!(branch, "branch#7=work/task-7", "имя ветки ворктри task-env");
    let branch_pos = log.iter().position(|l| l.starts_with("branch#")).unwrap();
    let session_pos = log.iter().position(|l| l.starts_with("session#")).unwrap();
    assert!(
        branch_pos < session_pos,
        "ячейка пишется до привязки сессии (env уже готов)"
    );
}

#[tokio::test]
async fn cup_holds_spawn_in_live() {
    // #1244: кап держат фактические запуски (busy + состоявшиеся спавны
    // тика), не очередь кандидатов — 8 кандидатов при cap 7: 7 спавнов
    // проходят, излишек ждёт следующего тика (старая формула считала
    // кандидатов активными и холдила всех: дедлок бэклога)
    let rows: Vec<Task> = (1..=8).map(|n| task(n, Status::InProgress)).collect();
    let (u, board, _sessions, _) = usecase(rows, PrState::Missing);
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 7, "бэклог заполняет кап до края");
    assert_eq!(out.held_cup, 1, "излишек кандидатов ждёт следующего тика");
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("status#")),
        "статусы не трогаем (сессии пишем — это спавны)"
    );
}

/// Фикстура носителя мержа: ветка (+PR) + automerge — для кейсов
/// действия Automerge.
fn carrier(t: &mut Task, n: u64, with_pr: bool) {
    let mut f = TaskFields::default();
    f.set_branch(BranchName::parse(&format!("work/task-{n}")).unwrap());
    if with_pr {
        f.open_pr(PrUrl::parse(&format!("https://github.com/a/c/pull/{n}")).unwrap())
            .unwrap();
    }
    t.apply_fields(f);
    t.set_automerge(true);
}

#[tokio::test]
async fn automerge_merged_is_noop_row_stays_review() {
    // #1232: PR уже влит (вручную/раньше) — no-op: merge-API не зовётся,
    // Done/close не ставятся, строка ждёт владельца в Review
    // (инцидент #1151/#1162 — ложный bounce смерженной строки недопустим)
    let mut t = task(3, Status::UserReview);
    carrier(&mut t, 3, true);
    let (u, board, _, prs) = usecase(vec![t], PrState::Merged);
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.automerged, 0, "no-op не считается мержем");
    assert_eq!(prs.merges.lock().unwrap().len(), 0, "merge-API не зовётся");
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter().any(|l| l.contains("status#3")),
        "статус не трогается: {log:?}"
    );
    assert!(!log.iter().any(|l| l.contains("close#3")));
}

#[tokio::test]
async fn automerge_red_ci_bounces_to_in_progress() {
    let mut t = task(4, Status::UserReview);
    carrier(&mut t, 4, true);
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
    assert!(
        log.iter().any(|l| l.contains("comment#4=")),
        "bounce несёт коммент: {log:?}"
    );
    assert!(!log.iter().any(|l| l.contains("close#4")));
}

#[tokio::test]
async fn automerge_green_merges_row_stays_review() {
    // #1232: зелёный мержит, но строка остаётся в Review — Done всегда
    // за владельцем
    let mut t = task(5, Status::UserReview);
    carrier(&mut t, 5, true);
    let (u, board, _, prs) = usecase(
        vec![t],
        PrState::Open {
            mergeable: true,
            ci_green: true,
        },
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(prs.merges.lock().unwrap().len(), 1);
    assert_eq!(out.automerged, 1);
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter().any(|l| l.contains("status#5")),
        "статус не трогается (остаётся Review): {log:?}"
    );
    assert!(!log.iter().any(|l| l.contains("close#5")));
}

#[tokio::test]
async fn automerge_conflict_bounces_to_in_progress() {
    let mut t = task(8, Status::UserReview);
    carrier(&mut t, 8, true);
    let (u, board, _, _) = usecase(
        vec![t],
        PrState::Open {
            mergeable: false,
            ci_green: true,
        },
    );
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|l| l.contains("status#8=InProgress")),
        "{log:?}"
    );
}

#[tokio::test]
async fn automerge_missing_pr_with_branch_bounces() {
    let mut t = task(6, Status::UserReview);
    carrier(&mut t, 6, false);
    let (u, board, _, _) = usecase(vec![t], PrState::Missing);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|l| l.contains("status#6=InProgress")),
        "{log:?}"
    );
}

#[tokio::test]
async fn automerge_pending_is_quiet() {
    // mergeable считается / чеки бегут — вердикт следующего тика,
    // строку не трогаем (#1232: ложный конфликт/green недопустим)
    let mut t = task(9, Status::UserReview);
    carrier(&mut t, 9, true);
    let (u, board, _, _) = usecase(vec![t], PrState::Pending);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter()
            .any(|l| l.contains("status#9") || l.contains("comment#9")),
        "pending тих: {log:?}"
    );
}

#[tokio::test]
async fn automerge_foreign_url_is_quiet_row_stays_review() {
    // #1269: PR-URL неуправляемого вендора (GHE-форма на чужом хосте,
    // Gitea без токена, API-отказ инстанса) — Foreign: PR есть, bounce
    // «PR не найден» ложен; строка ждёт владельца в Review
    let mut t = task(11, Status::UserReview);
    carrier(&mut t, 11, true);
    let (u, board, _, prs) = usecase(vec![t], PrState::Foreign);
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(prs.merges.lock().unwrap().len(), 0, "merge не зовётся");
    assert_eq!(out.automerged, 0);
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter()
            .any(|l| l.contains("status#11") || l.contains("comment#11")),
        "foreign тих: строка ждёт владельца — {log:?}"
    );
}

#[tokio::test]
async fn automerge_docs_only_is_not_probed() {
    // docs-only (нет PR и ветки) — домен не решает Automerge: тик не
    // прощупывает PR, строка ждёт владельца в Review (#1232)
    let mut t = task(7, Status::UserReview);
    t.set_automerge(true);
    let (u, board, _, prs) = usecase(vec![t], PrState::Missing);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(prs.merges.lock().unwrap().len(), 0);
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter()
            .any(|l| l.contains("status#7") || l.contains("comment#7")),
        "docs-only строка не трогается: {log:?}"
    );
}

#[tokio::test]
async fn branchless_pr_cell_is_not_probed_and_waits_for_owner() {
    // #1155 + #1232: безветочная ячейка (Artifact) — мержить нечего, тик
    // строку не прощупывает (никаких probe/bounce/Done) — она ждёт владельца
    // в Review; URL при этом виден на карточке (fm, см. e2e tools_tests).
    // До #1232 исходом был Done «без PR» — класс ложного Done закрыт гейтом.
    let mut t = task(8, Status::UserReview);
    t.set_automerge(true);
    let mut f = TaskFields::default();
    f.set_artifact(PrUrl::parse("https://github.com/a/c/pull/9").unwrap());
    t.apply_fields(f);
    let (u, board, _, prs) = usecase(vec![t], PrState::Missing);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        prs.probes.lock().unwrap().is_empty(),
        "нечего мержить — probe не зовётся"
    );
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter().any(|l| l.contains("status#8")),
        "строка ждёт владельца — статус не меняется"
    );
    assert!(!log.iter().any(|l| l.contains("close#8")));
}

#[tokio::test]
async fn branchless_real_pull_url_also_waits_for_owner() {
    // #1155 (пин 52fda41, семантика #1232): даже РЕАЛЬНЫЙ github pull-URL
    // на безветочной строке не мерж-гейт — ветки нет, кодовый цикл не начат;
    // строка ждёт владельца (PR виден на карточке — fm рендерит Artifact),
    // данные чинятся у источника (#1166 — фабрика пишет Branch при спавне)
    let mut t = task(9, Status::UserReview);
    t.set_automerge(true);
    let mut f = TaskFields::default();
    f.set_artifact(PrUrl::parse("https://github.com/a/c/pull/9").unwrap());
    t.apply_fields(f);
    let (u, board, _, prs) = usecase(
        vec![t],
        PrState::Open {
            mergeable: true,
            ci_green: true,
        },
    );
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        prs.probes.lock().unwrap().is_empty() && prs.merges.lock().unwrap().is_empty(),
        "без ветки кодового цикла нет — probe/merge не зовутся"
    );
    let log = board.log.lock().unwrap();
    assert!(!log.iter().any(|l| l.contains("status#9")));
    assert!(!log.iter().any(|l| l.contains("close#9")));
}

#[tokio::test]
async fn artifact_url_with_branch_waits_for_owner() {
    // #1286: docs-only с фабричной веткой и артефакт-URL (wiki — инстанс
    // #1159) в PR-ячейке — гейт не пускает строку в automerge-пробу:
    // probe не зовётся, bounce «ветка есть, PR не найден» не случается,
    // строка ждёт владельца в Review. Интеграционный провод доменного
    // гейта (decide → Observe) до мок-hubа: на master тест был красным
    // (Automerge → probe → bounce).
    let mut t = task(21, Status::UserReview);
    t.set_automerge(true);
    let mut f = TaskFields::default();
    f.set_branch(BranchName::parse("work/task-1159").unwrap());
    f.open_pr(PrUrl::parse("https://github.com/a/c/wiki/Runbook").unwrap())
        .unwrap();
    t.apply_fields(f);
    let (u, board, _, prs) = usecase(vec![t], PrState::Missing);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        prs.probes.lock().unwrap().is_empty(),
        "URL не PR-формы — проба не зовётся вовсе"
    );
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter()
            .any(|l| l.contains("status#21") || l.contains("comment#21")),
        "docs-only с веткой не бончится: {log:?}"
    );
    assert!(!log.iter().any(|l| l.contains("close#21")));
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
        // между кругами проходит время > кулдауна слота (фейк-часы заморожены —
        // кулдаун иначе держал бы спавн круга: прод-семантика, #1163)
        state.0.lock().unwrap().spawn_ms.clear();
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
async fn fourth_consecutive_request_changes_escalates() {
    // #1311: кап ревью-раундов. Переход ReviewResult → InProgress —
    // констатация request_changes (ревьювер вернул строку в работу);
    // 4-й подряд без approve эскалирует строку в Needs attention с
    // дайджестом вместо назначения 5-го раунда. Баг (до фикса): attention-
    // гвард слепа к смене статуса, цикл #1248 докрутился до раунда 12.
    let state = Arc::new(MemState(Mutex::default()));
    for round in 0..5u32 {
        // между кругами проходит время > кулдауна слота (фейк-часы
        // заморожены — кулдаун иначе держал бы спавн круга, #1163)
        state.0.lock().unwrap().spawn_ms.clear();
        let (u, board, _, _) = usecase_with_state(
            vec![task(40, Status::InProgress)],
            PrState::Missing,
            state.clone(),
        );
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        if round == 4 {
            // 4-й подряд request_changes: эскалация вместо 5-го раунда
            let log = board.log.lock().unwrap();
            assert!(
                log.iter()
                    .any(|l| l.starts_with("status#40=NeedsAttention")),
                "4-й подряд request_changes — строка в Needs attention: {log:?}"
            );
            assert!(
                log.iter()
                    .any(|l| l.starts_with("comment#40=⚠︎ Review-round cap")),
                "дайджест эскалации владельцу: {log:?}"
            );
            assert_eq!(
                out.spawns_done, 0,
                "5-й раунд не назначается — конвейер бросает строку"
            );
            break;
        }
        assert_eq!(
            out.spawns_done, 1,
            "раунд {round}: кап не достигнут, дев работает"
        );
        // строка уезжает на ревью: In Progress → Ревью результата
        let mut t = task(40, Status::ReviewResult);
        let mut f = TaskFields::default();
        f.set_session(
            Slot::Dev,
            uwuwu_domain::SessionRef::parse(&format!("ses_dev{round}")).unwrap(),
        );
        t.apply_fields(f);
        let (u2, _, _, _) = usecase_with_state(vec![t], PrState::Missing, state.clone());
        let _ = u2.run(&plan(Mode::Live, 7)).await.unwrap();
    }
}

#[tokio::test]
async fn review_rounds_approve_resets_counter() {
    // approve (ReviewResult → Testing) — цикл сошёлся: счётчик с нуля,
    // следующий цикл считает заново и не наследует старый баланс
    let state = Arc::new(MemState(Mutex::default()));
    // request_changes #1: ReviewResult → InProgress
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((42, Status::ReviewResult));
    let (u, _, _, _) = usecase_with_state(
        vec![task(42, Status::InProgress)],
        PrState::Missing,
        state.clone(),
    );
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        state.review_rounds("B", 42),
        1,
        "первый request_changes — счёт 1"
    );
    // approve: строка уехала в тестирование — сброс
    let (u2, _, _, _) = usecase_with_state(
        vec![task(42, Status::Testing)],
        PrState::Missing,
        state.clone(),
    );
    u2.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        state.review_rounds("B", 42),
        0,
        "approve — цикл сошёлся, счётчик с нуля"
    );
    // новый цикл: снова на ревью и снова вернулся — счёт 1, не 2
    let (u3, _, _, _) = usecase_with_state(
        vec![task(42, Status::ReviewResult)],
        PrState::Missing,
        state.clone(),
    );
    u3.run(&plan(Mode::Live, 7)).await.unwrap();
    let (u4, _, _, _) = usecase_with_state(
        vec![task(42, Status::InProgress)],
        PrState::Missing,
        state.clone(),
    );
    u4.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        state.review_rounds("B", 42),
        1,
        "новый цикл после approve считается с нуля"
    );
}

#[tokio::test]
async fn escalation_digest_names_last_review_session() {
    // #1311 (допокрытие тестировщика): дайджест эскалации несёт id последней
    // ревью-сессии из слота строки — владелец видит, чей раунд сорвался
    let state = Arc::new(MemState(Mutex::default()));
    // баланс 3: три предыдущих request_changes; прошлый тик видел ревью
    state.0.lock().unwrap().review_rounds.push((50, 3));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((50, Status::ReviewResult));
    // ревью-сессия в слоте результата — её id попадает в дайджест
    let mut t = task(50, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::ResultReview,
        uwuwu_domain::SessionRef::parse("ses_rev_live").unwrap(),
    );
    t.apply_fields(f);
    let (u, board, _, _) = usecase_with_state(vec![t], PrState::Missing, state.clone());
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    let digest = log
        .iter()
        .find(|l| l.starts_with("comment#50=⚠︎ Review-round cap"))
        .expect("дайджест эскалации");
    assert!(
        digest.contains("the last review session: ses_rev_live"),
        "id ревью-сессии в дайджесте: {digest}"
    );
    assert_eq!(state.review_rounds("B", 50), 0, "эскалация — свежий бюджет");
}

#[tokio::test]
async fn escalation_digest_without_session_is_honest_dash() {
    // пустой слот ревью (сессия не дожила до эскалации) — честное «—»,
    // не выдуманный id
    let state = Arc::new(MemState(Mutex::default()));
    state.0.lock().unwrap().review_rounds.push((51, 3));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((51, Status::ReviewResult));
    let (u, board, _, _) =
        usecase_with_state(vec![task(51, Status::InProgress)], PrState::Missing, state);
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    let digest = log
        .iter()
        .find(|l| l.starts_with("comment#51=⚠︎ Review-round cap"))
        .expect("дайджест эскалации");
    assert!(
        digest.contains("the last review session: —"),
        "пустой слот — «—» в дайджесте: {digest}"
    );
}

#[tokio::test]
async fn test_review_cycle_counts_and_escalates_with_test_review_digest() {
    // #1413: пара тест-ревью цикла (ReviewTesting → Testing) считается
    // тем же капом; дайджест эскалации называет сессию TestReview-слота
    let state = Arc::new(MemState(Mutex::default()));
    // счёт 1: первый возврат тест-ревьювера (ReviewTesting → Testing)
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((52, Status::ReviewTesting));
    let (u, _, _, _) = usecase_with_state(
        vec![task(52, Status::Testing)],
        PrState::Missing,
        state.clone(),
    );
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        state.review_rounds("B", 52),
        1,
        "возврат тест-ревьювера — счёт 1"
    );
    // баланс 3 + ещё один возврат — кап: эскалация с TestReview-дайджестом
    state.0.lock().unwrap().review_rounds.push((53, 3));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((53, Status::ReviewTesting));
    let mut t = task(53, Status::Testing);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::TestReview,
        uwuwu_domain::SessionRef::parse("ses_test_rev_live").unwrap(),
    );
    t.apply_fields(f);
    let (u2, board, _, _) = usecase_with_state(vec![t], PrState::Missing, state.clone());
    u2.run(&plan(Mode::Live, 7)).await.unwrap();
    let log = board.log.lock().unwrap();
    let digest = log
        .iter()
        .find(|l| l.starts_with("comment#53=⚠︎ Review-round cap"))
        .expect("дайджест эскалации тест-цикла");
    assert!(
        digest.contains("the last review session: ses_test_rev_live"),
        "id тест-ревью-сессии в дайджесте: {digest}"
    );
    assert_eq!(state.review_rounds("B", 53), 0, "эскалация — свежий бюджет");
}

#[tokio::test]
async fn test_review_cycle_approve_resets_counter() {
    // #1413: approve тест-ревьювера (ReviewTesting → UserReview) — цикл
    // сошёлся, счётчик с нуля
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((54, Status::ReviewTesting));
    let (u, _, _, _) = usecase_with_state(
        vec![task(54, Status::UserReview)],
        PrState::Missing,
        state.clone(),
    );
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        state.review_rounds("B", 54),
        0,
        "approve — цикл сошёлся, счётчик с нуля"
    );
}

#[tokio::test]
async fn review_rounds_retry_spawn_does_not_count() {
    // ретрай спавна ревьювера в том же статусе (мёртвая сессия того же
    // раунда) — не request_changes: счётчик не инкрементится
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((43, Status::ReviewResult));
    state.0.lock().unwrap().spawn_ms.clear();
    let (u1, _, _, _) = usecase_with_state(
        vec![task(43, Status::ReviewResult)],
        PrState::Missing,
        state.clone(),
    );
    u1.run(&plan(Mode::Live, 7)).await.unwrap();
    state.0.lock().unwrap().spawn_ms.clear();
    let (u2, _, _, _) = usecase_with_state(
        vec![task(43, Status::ReviewResult)],
        PrState::Missing,
        state.clone(),
    );
    u2.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        state.review_rounds("B", 43),
        0,
        "спавн без смены статуса — тот же раунд"
    );
}

#[tokio::test]
async fn review_rounds_observe_mode_does_not_write() {
    // observe-режим: решения в лог, state не пишется — счётчик не двигается
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((44, Status::ReviewResult));
    let (u, _, _, _) = usecase_with_state(
        vec![task(44, Status::InProgress)],
        PrState::Missing,
        state.clone(),
    );
    u.run(&plan(Mode::Observe, 7)).await.unwrap();
    assert_eq!(
        state.review_rounds("B", 44),
        0,
        "observe не накручивает счётчики"
    );
}

#[tokio::test]
async fn escalation_set_status_failure_retries_next_transition() {
    // #1311 (ревью L2): единственная ветка деградации гварда — отказ
    // set_status на эскалации. Счётчик остаётся на капе (никаких сбросов),
    // N+1-й count-переход эскалирует снова; случайный сброс при отказе
    // потерял бы стоп-кран навсегда.
    let state = Arc::new(MemState(Mutex::default()));
    for round in 0..6u32 {
        state.0.lock().unwrap().spawn_ms.clear();
        let (u, board, _, _) = usecase_with_state(
            vec![task(47, Status::InProgress)],
            PrState::Missing,
            state.clone(),
        );
        if round == 4 {
            board.status_fails.store(true, Ordering::Relaxed);
        }
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        if round < 4 {
            assert_eq!(
                out.spawns_done, 1,
                "раунд {round}: кап не достигнут, дев работает"
            );
        } else if round == 4 {
            // кап достигнут, но set_status отказал: эскалации нет —
            // раунд 5 проходит (деградация), счётчик остаётся на капе
            let log = board.log.lock().unwrap();
            assert!(
                !log.iter()
                    .any(|l| l.starts_with("status#47=NeedsAttention")),
                "отказ set_status — статус не переведён: {log:?}"
            );
            assert!(
                !log.iter().any(|l| l.starts_with("comment#47=")),
                "дайджест при отказе set_status не уходит: {log:?}"
            );
            assert_eq!(out.spawns_done, 1, "раунд 5 проходит при деградации");
            assert_eq!(
                state.review_rounds("B", 47),
                4,
                "счётчик стоит на капе — ретрай на следующем переходе"
            );
            board.status_fails.store(false, Ordering::Relaxed);
        } else {
            // N+1-й count-переход: эскалация повторяется успешно
            let log = board.log.lock().unwrap();
            assert!(
                log.iter()
                    .any(|l| l.starts_with("status#47=NeedsAttention")),
                "ретрай эскалации — строка в Needs attention: {log:?}"
            );
            assert!(
                log.iter()
                    .any(|l| l.starts_with("comment#47=⚠︎ Review-round cap")),
                "дайджест уходит при успешном ретрае: {log:?}"
            );
            assert_eq!(out.spawns_done, 0, "6-й раунд не назначается");
            assert_eq!(
                state.review_rounds("B", 47),
                0,
                "после успешной эскалации — свежий бюджет"
            );
            break;
        }
        // строка уезжает на ревью и возвращается: очередной request_changes
        let mut t = task(47, Status::ReviewResult);
        let mut f = TaskFields::default();
        f.set_session(
            Slot::Dev,
            uwuwu_domain::SessionRef::parse(&format!("ses_fix{round}")).unwrap(),
        );
        t.apply_fields(f);
        let (u2, _, _, _) = usecase_with_state(vec![t], PrState::Missing, state.clone());
        let _ = u2.run(&plan(Mode::Live, 7)).await.unwrap();
    }
}

#[tokio::test]
async fn plan_review_loop_escalates_by_same_cap() {
    // #1311 (план-контур): цикл Design ↔ Ревью плана дыряв так же —
    // 4-й подряд request_changes плана (ReviewPlan → Design) эскалирует
    // строку вместо назначения 5-го круга
    let state = Arc::new(MemState(Mutex::default()));
    for round in 0..5u32 {
        state.0.lock().unwrap().spawn_ms.clear();
        let (u, board, _, _) = usecase_with_state(
            vec![task(45, Status::Design)],
            PrState::Missing,
            state.clone(),
        );
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        if round == 4 {
            let log = board.log.lock().unwrap();
            assert!(
                log.iter()
                    .any(|l| l.starts_with("status#45=NeedsAttention")),
                "план-контур: 4-й подряд request_changes — Needs attention: {log:?}"
            );
            assert!(
                log.iter()
                    .any(|l| l.starts_with("comment#45=⚠︎ Review-round cap")),
                "дайджест эскалации владельцу: {log:?}"
            );
            assert_eq!(out.spawns_done, 0, "5-й круг плана не назначается");
            break;
        }
        assert_eq!(
            out.spawns_done, 1,
            "круг {round}: кап не достигнут, дизайнер работает"
        );
        // дизайнер отправил план на ревью: Design → Ревью плана
        let mut t = task(45, Status::ReviewPlan);
        let mut f = TaskFields::default();
        f.set_session(
            Slot::Design,
            uwuwu_domain::SessionRef::parse(&format!("ses_dsn{round}")).unwrap(),
        );
        t.apply_fields(f);
        let (u2, _, _, _) = usecase_with_state(vec![t], PrState::Missing, state.clone());
        let _ = u2.run(&plan(Mode::Live, 7)).await.unwrap();
    }
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
    // cup=2, busy=0: мёртвая сессия не держит кап, кандидат — очередь;
    // респавн и кандидат оба проходят (active_now 0→2), никто не ждёт
    let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(
        out.spawns_done, 2,
        "респавн мёртвой строки и кандидат — оба в слоте капа"
    );
    assert_eq!(out.held_cup, 0, "мёртвая сессия и очередь кап не держат");
}

#[tokio::test]
async fn dead_stage_and_candidate_hold_blind_when_list_unavailable() {
    // инверсия деградации (#1263): исполнитель проекта недоступен — спавны
    // не идут вовсе (hold без зрения), CUP-гейт не достигается; статусный
    // консервативный подсчёт остаётся для смешанного здоровья исполнителей
    let mut dead = task(42, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_42").unwrap(),
    );
    dead.apply_fields(f);
    let candidate = task(43, Status::InProgress);
    let sessions = FakeSessions {
        switched: Mutex::default(),
        prompts: Mutex::default(),
        spawned: Mutex::default(),
        activity_ms: Some(1000),
        dead_sessions: Mutex::default(),
        list_fails: true,
        idle_reply: false,
        specs: Mutex::default(),
        live: Mutex::default(),
        busy_ids: Mutex::default(),
        extra: Mutex::default(),
        prompt_fails: false,
        prompt_lost: Mutex::default(),
        prompt_lost_all: false,
        deleted: Mutex::default(),
        activity_fails_sessions: Mutex::default(),
        liveness_unknown: false,
        aborted: Mutex::default(),
        abort_fails: AtomicBool::new(false),
        spawn_probe: AtomicBool::new(true),
    };
    let (u, _, _, _) = usecase_with_sessions(
        vec![dead, candidate],
        PrState::Missing,
        sessions,
        Arc::new(MemState(Mutex::default())),
    );
    let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out.spawns_done, 0, "слепой спавн невозможен — hold (#1263)");
    assert_eq!(out.held_blind, 1, "кандидат в слепом hold",);
    assert_eq!(
        out.wakeups, 0,
        "строка с probe-живой сессией прошла Observe: будки и доставки нет"
    );
    assert_eq!(out.held_cup, 0, "до CUP-гейта слепой hold не доходит");
}

#[tokio::test]
async fn mixed_health_cup_counts_only_reachable_busy() {
    // Отказной исполнитель не морозит живой (#1450): redmi (дефолт)
    // недоступен, доска p на живом kisa. Бывший strict-фолбэк считал бы
    // кап по статусам всех строк (2: мёртвая сессия + кандидат) — тот же
    // механизм сутки держал прод при капе 7 и 37 активных строках
    // (инцидент 21.11). Теперь кап = реальный busy живых списков (0):
    // мёртвая сессия строки 66 респавнится, кандидат 67 спавнится.
    // Строки самого отказного держит слепой hold (#1263), зрение проекта
    // чужой отказ не гасит.
    let rows = || {
        let mut session_row = task(66, Status::InProgress);
        let mut f = TaskFields::default();
        f.set_session(
            Slot::Dev,
            uwuwu_domain::SessionRef::parse("ses_dead_66").unwrap(),
        );
        session_row.apply_fields(f);
        vec![session_row, task(67, Status::InProgress)]
    };
    let build = || {
        let mut redmi = FakeSessions::new();
        redmi.list_fails = true;
        let kisa = FakeSessions::new();
        // list и GET согласованы: сессии нет в list → она мёртва и в
        // liveness (контракт second source #1163)
        kisa.dead_sessions
            .lock()
            .unwrap()
            .push("ses_dead_66".into());
        let mut by_project = std::collections::HashMap::new();
        by_project.insert(
            "p".to_string(),
            Arc::new(kisa) as Arc<dyn AgentSessionGateway>,
        );
        let router = SessionRouter::new(
            "redmi",
            Arc::new(redmi) as Arc<dyn AgentSessionGateway>,
            vec![("kisa".to_string(), by_project["p"].clone())],
            by_project,
        );
        // prev = текущий статус: мёртвая сессия строки 66 идёт
        // респавн-путём (не кик-разморозкой)
        let state = Arc::new(MemState(Mutex::default()));
        state
            .0
            .lock()
            .unwrap()
            .statuses
            .push((66, Status::InProgress));
        let board = Arc::new(FakeBoard {
            rows: std::sync::Mutex::new(rows()),
            log: Mutex::default(),
            comments: Vec::default(),
            fields_fails: false,
            set_session_fails: false,
            clear_session_fails: AtomicBool::new(false),
            status_fails: AtomicBool::new(false),
        });
        let prs = Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        });
        TickUseCase {
            board: board.clone(),
            sessions: Arc::new(router),
            prs: prs.clone(),
            clock: Arc::new(FixedClock),
            state,
            models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
            boot_ms: 0,
            judge: None,
            judge_cfg: Default::default(),
            quota: None,
        }
    };
    let out = build().run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.sessions_busy, 0,
        "кап = busy живых списков, не статусный счёт fallback'а"
    );
    assert_eq!(
        out.spawns_done, 2,
        "респавн мёртвой строки и кандидат проходят — деградация не паралич"
    );
    assert_eq!(out.held_cup, 0);
    assert_eq!(out.held_blind, 0, "чужой отказ не слепит зрение проекта");
}

#[tokio::test]
async fn foreign_executor_failure_does_not_blind_project_guard() {
    // пер-проектное зрение (#1263): упал ЧУЖОЙ исполнитель (дефолтный
    // redmi) — зрение проекта p (свой исполнитель kisa) живо: сестра в env
    // держит спавн как live-hold, а не как слепой; без сестры спавн идёт
    let sister = FakeSessions::with_env_session("ses_sis_65", "/tmp/env/65", "", "", 2000);
    let mut redmi = FakeSessions::new();
    redmi.list_fails = true;
    let mut by_project = std::collections::HashMap::new();
    by_project.insert(
        "p".to_string(),
        Arc::new(sister) as Arc<dyn AgentSessionGateway>,
    );
    let router = SessionRouter::new(
        "redmi",
        Arc::new(redmi) as Arc<dyn AgentSessionGateway>,
        vec![("kisa".to_string(), by_project["p"].clone())],
        by_project,
    );
    let state = Arc::new(MemState(Mutex::default()));
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![task(65, Status::InProgress)]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    // master-break #1155×#1263 (см. соседний тест): probes не догнали
    let prs = Arc::new(FakePrs {
        state: PrState::Missing,
        merges: Mutex::default(),
        probes: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(router),
        prs: prs.clone(),
        clock: Arc::new(FixedClock),
        state,
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.held_live, 1,
        "зрение проекта живо при чужом отказе: сестра держит live-hold"
    );
    assert_eq!(out.held_blind, 0, "чужой отказ не слепит проект");
    assert_eq!(out.spawns_done, 0);
}

#[tokio::test]
async fn cup_counts_second_turn_sessions() {
    // Регресс #1231 (12 активных при cap=7): outcome — атрибут последнего
    // завершённого хода и при новом ходе не очищается. Сессия стадии на
    // 2+ ходе (busy, финал уже был) раньше выпадала из alive-фильтра
    // outcome.is_empty() и кап не держала — утренний штурм будильников
    // делал невидимыми почти все тратящие сессии.
    let mut working = task(150, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_2nd_150").unwrap(),
    );
    working.apply_fields(f);
    let candidate = task(151, Status::InProgress);
    let (u, _, sessions, _) = usecase(vec![working, candidate], PrState::Missing);
    sessions
        .extra
        .lock()
        .unwrap()
        .push(crate::ports::SessionInfo {
            id: "ses_2nd_150".into(),
            agent: "developer".into(),
            updated_ms: 0,
            outcome: "succeeded".into(),
            total_tokens: 5000,
            directory: String::new(),
            busy: true,
        });
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(
        out.spawns_done, 0,
        "busy-сессия 2+ хода держит кап — кандидат ждёт"
    );
    assert_eq!(out.held_cup, 1);
    assert_eq!(out.sessions_busy, 1, "наблюдаемость: busy в исходе тика");
}

#[tokio::test]
async fn cup_busy_counts_non_pipeline_sessions() {
    // Единый учёт (#1231): busy-сессия без строки доски (бот, крон-ран,
    // сайдкар, ручная сессия владельца) тратит тот же LLM-бюджет —
    // union-скан считает ВСЕ ходы исполнителя, не только stage-строки.
    let candidate = task(160, Status::InProgress);
    let (u, _, sessions, _) = usecase(vec![candidate], PrState::Missing);
    sessions
        .extra
        .lock()
        .unwrap()
        .push(crate::ports::SessionInfo {
            id: "ses_bot".into(),
            agent: String::new(),
            updated_ms: 0,
            outcome: String::new(),
            total_tokens: 9000,
            directory: String::new(),
            busy: true,
        });
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(
        out.spawns_done, 0,
        "непайплайн busy-сессия держит единый кап"
    );
    assert_eq!(out.held_cup, 1);
}

/// Пин агента слота на будильнике (#1300): доставка хода дев-строке
/// предваряется switch_agent — легаси-безымянная дев-сессия получает
/// developer до промпта (инцидент #1044: wake перебросил кости резолва
/// исполнителя, ход делал аналитик).
#[tokio::test]
async fn wake_pins_the_slot_agent() {
    let (u, sessions, _) = wake_usecase(
        vec![dev_task(1, "ses_dev")],
        vec![CommentRef {
            id: "c1".into(),
            created_ms: 3000,
            body: "владелец: продолжай".into(),
        }],
        Some(1000),
        true, // ход завершён (idle) — доставка сразу
        Arc::new(MemState(Mutex::default())),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups, 1, "wakeup доставлен");
    let switched = sessions.switched.lock().unwrap();
    assert!(
        switched.iter().any(|s| s == "ses_dev|developer"),
        "wake пинит developer на дев-сессии до промпта: {switched:?}"
    );
}

/// CUP-гвард будильника (#1231): доставка промпта в idle-сессию — новый
/// ход, тот же LLM-слот. При полном капе будильник откладывается;
/// seen-маркер не пишется — доставка повторится на свободном тике
/// (комменты не теряются).
#[tokio::test]
async fn wakeup_held_when_cup_full() {
    // #1231: доставка промпта в idle-сессию — новый ход, тот же LLM-слот.
    // При полном капе будильник откладывается; seen-маркер не пишется —
    // доставка повторится на свободном тике (комменты не теряются).
    let state = Arc::new(MemState(Mutex::default()));
    let mut t = task(170, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_wake_170").unwrap(),
    );
    t.apply_fields(f);
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![t]),
        log: Mutex::default(),
        comments: vec![CommentRef {
            id: "c1".into(),
            created_ms: 3000,
            body: "свежий коммент владельца".into(),
        }],
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(FakeSessions {
        activity_ms: Some(1000), // коммент свежее активности сессии → fresh
        ..Default::default()
    });
    // сама строка idle + чужой ход в полёте — единый кап занят
    for (id, busy) in [("ses_wake_170", false), ("ses_other", true)] {
        sessions
            .extra
            .lock()
            .unwrap()
            .push(crate::ports::SessionInfo {
                id: id.into(),
                agent: String::new(),
                directory: String::new(),
                updated_ms: 0,
                outcome: "succeeded".into(),
                total_tokens: 0,
                busy,
            });
    }
    let prs = Arc::new(FakePrs {
        state: PrState::Missing,
        merges: Mutex::default(),
        probes: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: prs.clone(),
        clock: Arc::new(FixedClock),
        state: state.clone(),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert!(
        sessions.prompts.lock().unwrap().is_empty(),
        "при полном капе будильник не доставляется"
    );
    assert_eq!(out.wakeups, 0);
    assert_eq!(out.wakeups_deferred, 1);
    assert!(
        state.0.lock().unwrap().comments.is_empty(),
        "seen не пишется — доставка повторится на свободном тике"
    );
}

#[tokio::test]
async fn wakeup_delivery_occupies_cup_slot() {
    // Доставленный будильник занимает слот капа в этом же тике (#1231):
    // кандидат на спавн после доставки ждёт следующего тика — массовый
    // штурм комментариев/бэклога не плодит ходы сверх капа.
    let state = Arc::new(MemState(Mutex::default()));
    let mut woken = task(180, Status::InProgress);
    let mut fw = TaskFields::default();
    fw.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_wake_180").unwrap(),
    );
    woken.apply_fields(fw);
    let candidate = task(181, Status::InProgress);
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![woken, candidate]),
        log: Mutex::default(),
        comments: vec![CommentRef {
            id: "c180".into(),
            created_ms: 3000,
            body: "свежий коммент владельца".into(),
        }],
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(FakeSessions {
        activity_ms: Some(1000),
        ..Default::default()
    });
    sessions
        .extra
        .lock()
        .unwrap()
        .push(crate::ports::SessionInfo {
            id: "ses_wake_180".into(),
            agent: String::new(),
            updated_ms: 0,
            outcome: "succeeded".into(),
            total_tokens: 0,
            directory: String::new(),
            busy: false,
        });
    let prs = Arc::new(FakePrs {
        state: PrState::Missing,
        merges: Mutex::default(),
        probes: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: prs.clone(),
        clock: Arc::new(FixedClock),
        state: state.clone(),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(
        sessions.prompts.lock().unwrap().len(),
        1,
        "будильник доставлен"
    );
    assert_eq!(out.wakeups, 1);
    assert_eq!(
        out.held_cup, 1,
        "слот занят доставленным ходом — кандидат ждёт следующего тика"
    );
    assert_eq!(out.spawns_done, 0);
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
    // cup=2, busy=0: оба респавна проходят (active_now 0→2), кандидат
    // упирается в наполненный кап — оверселла нет
    let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out.spawns_done, 2, "два респавна заполняют кап");
    assert_eq!(out.held_cup, 1, "кандидат удержан наполненным капом");
    assert_eq!(out.wakeups_deferred, 0, "респавны не отложены — busy=0");
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
            rows: std::sync::Mutex::new(vec![t]),
            log: Mutex::default(),
            fields_fails: false,
            set_session_fails: false,
            clear_session_fails: AtomicBool::new(false),
            status_fails: AtomicBool::new(false),
            comments: vec![CommentRef {
                id: "verdict".into(),
                created_ms: 3000,
                body: "вердикт".into(),
            }],
        });
        let sess = Arc::new(FakeSessions {
            switched: Mutex::default(),
            prompts: Mutex::default(),
            spawned: Mutex::default(),
            activity_ms: Some(5000),
            dead_sessions: Mutex::default(),
            list_fails: false,
            idle_reply: false,
            specs: Mutex::default(),
            live: Mutex::default(),
            busy_ids: Mutex::default(),
            extra: Mutex::default(),
            prompt_fails: false,
            prompt_lost: Mutex::default(),
            prompt_lost_all: false,
            deleted: Mutex::default(),
            activity_fails_sessions: Mutex::default(),
            liveness_unknown: false,
            aborted: Mutex::default(),
            abort_fails: AtomicBool::new(false),
            spawn_probe: AtomicBool::new(true),
        });
        let prs = Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        });
        let uc = TickUseCase {
            board: board.clone(),
            sessions: Arc::new(SessionRouter::uniform(sess.clone())),
            prs: prs.clone(),
            clock: Arc::new(FixedClock),
            state: state.clone(),
            models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
            boot_ms: 0,
            judge: None,
            judge_cfg: Default::default(),
            quota: None,
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

    // агент завершил ход ДО коммента (активность ниже времени коммента —
    // fresh-путь), тот же seen-стейт: следующий тик доставляет коммент
    let mut t2 = task(60, Status::InProgress);
    let mut f2 = TaskFields::default();
    f2.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dev_60").unwrap(),
    );
    t2.apply_fields(f2);
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![t2]),
        log: Mutex::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
        comments: vec![CommentRef {
            id: "verdict".into(),
            created_ms: 3000,
            body: "вердикт".into(),
        }],
    });
    let sess2 = Arc::new(FakeSessions {
        switched: Mutex::default(),
        prompts: Mutex::default(),
        spawned: Mutex::default(),
        activity_ms: Some(1000),
        dead_sessions: Mutex::default(),
        list_fails: false,
        idle_reply: false,
        specs: Mutex::default(),
        live: Mutex::default(),
        busy_ids: Mutex::default(),
        extra: Mutex::default(),
        prompt_fails: false,
        prompt_lost: Mutex::default(),
        prompt_lost_all: false,
        deleted: Mutex::default(),
        activity_fails_sessions: Mutex::default(),
        liveness_unknown: false,
        aborted: Mutex::default(),
        abort_fails: AtomicBool::new(false),
        spawn_probe: AtomicBool::new(true),
    });
    let u2 = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess2.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state,
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
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
        rows: std::sync::Mutex::new(vec![t]),
        log: Mutex::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
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
        switched: Mutex::default(),
        prompts: Mutex::default(),
        spawned: Mutex::default(),
        activity_ms: Some(900),
        dead_sessions: Mutex::default(),
        list_fails: false,
        idle_reply: false,
        specs: Mutex::default(),
        live: Mutex::default(),
        busy_ids: Mutex::default(),
        extra: Mutex::default(),
        prompt_fails: false,
        prompt_lost: Mutex::default(),
        prompt_lost_all: false,
        deleted: Mutex::default(),
        activity_fails_sessions: Mutex::default(),
        liveness_unknown: false,
        aborted: Mutex::default(),
        abort_fails: AtomicBool::new(false),
        spawn_probe: AtomicBool::new(true),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state: state.clone(),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
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
        rows: std::sync::Mutex::new(vec![t]),
        log: Mutex::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
        comments: vec![CommentRef {
            id: "verdict".into(),
            created_ms: 3000,
            body: "вердикт ревьювера".into(),
        }],
    });
    // агент ответил ПОСЛЕ коммента: activity 5000 > created 3000 — но ход
    // завершён (idle_reply: последнее сообщение — финальный ответ)
    let sess = Arc::new(FakeSessions {
        activity_ms: Some(5000),
        idle_reply: true,
        prompt_fails: false,
        prompt_lost: Mutex::default(),
        prompt_lost_all: false,
        deleted: Mutex::default(),
        ..Default::default()
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state,
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
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
        rows: std::sync::Mutex::new(vec![t]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        // молчание ~30 мин (tool-call висит), idle_reply = false
        activity_ms: Some(100_000_000 - 30 * 60_000),
        idle_reply: false,
        specs: Mutex::default(),
        live: Mutex::default(),
        busy_ids: Mutex::default(),
        extra: Mutex::default(),
        prompt_fails: false,
        prompt_lost: Mutex::default(),
        prompt_lost_all: false,
        deleted: Mutex::default(),
        ..Default::default()
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
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
        rows: std::sync::Mutex::new(vec![t]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        // молчание 16 мин, ход завершён (idle)
        activity_ms: Some(100_000_000 - 16 * 60_000),
        idle_reply: true,
        prompt_fails: false,
        prompt_lost: Mutex::default(),
        prompt_lost_all: false,
        deleted: Mutex::default(),
        ..Default::default()
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.soft_pings, 1, "мягкий пинг ушёл в сессию");
    assert!(sess
        .prompts
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.contains("Soft ping")));
}

#[tokio::test]
async fn soft_ping_held_when_cup_full_then_delivered_on_free_slot() {
    // #1313: мягкий пинг — запуск хода, тот же LLM-слот, что будильник и
    // спавн (UC-05 «все новые запуски ходов откладываются»). Кап полон —
    // доставка держится без mark_stall_ping (ретрай следующим тиком),
    // на освободившемся слоте уходит.
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
        rows: std::sync::Mutex::new(vec![t]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        // молчание 16 мин, ход завершён (idle)
        activity_ms: Some(100_000_000 - 16 * 60_000),
        idle_reply: true,
        ..FakeSessions::new()
    });
    // кап полон: чужая busy-сессия занимает единственный слот
    sess.extra.lock().unwrap().push(crate::ports::SessionInfo {
        id: "ses_cap_filler".into(),
        agent: String::new(),
        directory: String::new(),
        updated_ms: 0,
        outcome: String::new(),
        total_tokens: 100,
        busy: true,
    });
    let state = Arc::new(MemState(Mutex::default()));
    let u = TickUseCase {
        board,
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: state.clone(),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.soft_pings, 0, "кап полон — мягкий пинг не доставляется");
    assert_eq!(out.soft_pings_deferred, 1, "холд пинга наблюдаем");
    assert!(
        !sess
            .prompts
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("Мягкий пинг")),
        "промпта нет"
    );
    assert!(
        state.0.lock().unwrap().stalls.is_empty(),
        "mark_stall_ping при холде не пишется — доставка ретраится"
    );
    // слот освободился — пинг уходит следующим тиком
    sess.extra.lock().unwrap().clear();
    let out2 = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out2.soft_pings, 1, "свободный слот — пинг доставлен");
    assert_eq!(out2.soft_pings_deferred, 0);
}

/// #1313 (ревью High): доставленный пинг занимает слот в счётчике тика —
/// две idle-строки со ставшими ходами при капе 1 получают РОВНО ОДИН пинг
/// за тик (вторая видит инкремент и держится); без инкремента масс-сталл
/// оверселлит кап (класс общего счётчика #1200/#1244).
#[tokio::test]
async fn soft_ping_mass_stall_oversell_is_capped() {
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
    let rows: Vec<Task> = [71, 72]
        .iter()
        .map(|n| {
            let mut t = task(*n, Status::InProgress);
            let mut f = TaskFields::default();
            f.set_session(
                Slot::Dev,
                uwuwu_domain::SessionRef::parse(&format!("ses_dev_{n}")).unwrap(),
            );
            t.apply_fields(f);
            t
        })
        .collect();
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(rows),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        activity_ms: Some(100_000_000 - 16 * 60_000),
        idle_reply: true,
        ..FakeSessions::new()
    });
    let u = TickUseCase {
        board,
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(
        out.soft_pings, 1,
        "масс-сталл при капе 1: ровно один пинг за тик — инкремент слота виден второй строке"
    );
    assert_eq!(
        out.soft_pings_deferred, 1,
        "вторая строка держится уже обновлённым счётчиком"
    );
    assert_eq!(
        sess.prompts.lock().unwrap().len(),
        1,
        "промпт один — оверселла нет"
    );
}

/// Инцидент 18.11 (#1281): рестарт uwuwu-server не должен парковать живые
/// stage-сессии сталл-детектом. Геометрия прод: persisted count=2 (два
/// автопинга ДО рестарта, volume /data переживает пересоздание контейнера),
/// первый тик нового процесса — эскалация по чужому раунду пингов без
/// единого доставленного в этом процессе пинга. Эскалация требует пингов,
/// доставленных в текущем процессе: раунд из-под рестарта недействителен,
/// строка получает свежий раунд (пинг), не парковку.
#[tokio::test]
async fn restored_counters_from_previous_process_do_not_escalate_on_first_tick() {
    const NOW: u64 = 100_000_000;
    struct BigClock;
    impl Clock for BigClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            NOW
        }
    }
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![dev_task(64, "ses_dev_64")]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        // silence 25 мин (≥ soft gate 15), ход завершён (idle)
        activity_ms: Some(NOW - 25 * 60_000),
        idle_reply: true,
        ..Default::default()
    });
    // persisted стейт из-под рестарта: два пинга раунда, последний 35 мин
    // назад — ДО старта текущего процесса (boot_ms = NOW)
    let seeded = MemState(Mutex::new(MemStateInner {
        stalls: vec![
            (64, Status::InProgress, NOW - 40 * 60_000),
            (64, Status::InProgress, NOW - 35 * 60_000),
        ],
        ..Default::default()
    }));
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(seeded),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: NOW,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.stall_flagged, 0,
        "первый тик после рестарта не эскалирует по до-рестартным каунтерам"
    );
    assert_eq!(
        out.soft_pings, 1,
        "строка получила свежий раунд: живой пинг"
    );
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("status#64=NeedsAttention")),
        "строка не паркуется без доставленных в процессе пингов"
    );
}

/// Контр-тест той же механики: пинги раунда, доставленные В текущем
/// процессе (после boot), эскалируют по обычной семантике — сброс
/// раунда не отменяет сталл-детект как класс.
#[tokio::test]
async fn counters_earned_by_current_process_still_escalate() {
    const NOW: u64 = 100_000_000;
    struct BigClock;
    impl Clock for BigClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            NOW
        }
    }
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![dev_task(64, "ses_dev_64")]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        activity_ms: Some(NOW - 25 * 60_000),
        idle_reply: true,
        ..Default::default()
    });
    // процесс стартовал 50 мин назад; оба пинга раунда — после старта
    let seeded = MemState(Mutex::new(MemStateInner {
        stalls: vec![
            (64, Status::InProgress, NOW - 40 * 60_000),
            (64, Status::InProgress, NOW - 35 * 60_000),
        ],
        ..Default::default()
    }));
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(seeded),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: NOW - 50 * 60_000,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.stall_flagged, 1,
        "раунд, заработанный текущим процессом, эскалирует как раньше"
    );
    // Различение причин (#1281): подпись эскалации — действенный
    // diagnose-сигнал, а не «после двух автопингов» без вердикта пробы
    let log = board.log.lock().unwrap();
    let comment = log
        .iter()
        .find(|l| l.starts_with("comment#64="))
        .expect("эскалация постит коммент");
    assert!(
        comment.contains("with the session alive"),
        "подпись различает причину: агент молчит при живой сессии"
    );
    assert!(
        comment.contains("executor probe: alive"),
        "вердикт пробы в подписи"
    );
    assert!(
        comment.contains("auto-pings in the round: 2"),
        "счётчик доставленных пингов раунда в подписи"
    );
}

/// Срез 4 (#1281): недостоверная проба живости в момент эскалации —
/// эскалация не выполняется, отказ фиксируется как probe_failed
/// (повтор на следующем тике, когда проба вернётся к правде).
#[tokio::test]
async fn escalation_deferred_when_liveness_probe_unreliable() {
    const NOW: u64 = 100_000_000;
    struct BigClock;
    impl Clock for BigClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            NOW
        }
    }
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![dev_task(64, "ses_dev_64")]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        activity_ms: Some(NOW - 25 * 60_000),
        idle_reply: true,
        liveness_unknown: true,
        ..Default::default()
    });
    // раунд, заработанный текущим процессом (boot давно): обычная
    // эскалация заблокирована только недостоверной пробой
    let seeded = MemState(Mutex::new(MemStateInner {
        stalls: vec![
            (64, Status::InProgress, NOW - 40 * 60_000),
            (64, Status::InProgress, NOW - 35 * 60_000),
        ],
        ..Default::default()
    }));
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(seeded),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: NOW - 50 * 60_000,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.stall_flagged, 0,
        "эскалация не выполняется по недостоверной пробе"
    );
    assert_eq!(out.probe_failed, 1, "отказ пробы виден в outcome");
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("NeedsAttention")),
        "строка не паркуется, пока проба недостоверна"
    );
}

/// Пин ADR-101 (#1313): Design-фантом (ход умер, запись сессии жива,
/// тишина) проходит сталл-детект наравне с In Progress — эскалация
/// уводит статус из LLM-активных, слот CUP освобождается.
#[tokio::test]
async fn design_phantom_stall_escalates_and_frees_cup_slot() {
    const NOW: u64 = 100_000_000;
    struct BigClock;
    impl Clock for BigClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            NOW
        }
    }
    let mut design = task(45, Status::Design);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Design,
        uwuwu_domain::SessionRef::parse("ses_design_45").unwrap(),
    );
    design.apply_fields(f);
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![design]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        activity_ms: Some(NOW - 25 * 60_000),
        idle_reply: true,
        ..Default::default()
    });
    // раунд пингов Design-статуса, заработанный текущим процессом
    let seeded = MemState(Mutex::new(MemStateInner {
        stalls: vec![
            (45, Status::Design, NOW - 40 * 60_000),
            (45, Status::Design, NOW - 35 * 60_000),
        ],
        ..Default::default()
    }));
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(seeded),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: NOW - 50 * 60_000,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.stall_flagged, 1,
        "Design-фантом эскалирует — сталл-детект слот-генеричен"
    );
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("status#45=NeedsAttention")),
        "статус ушёл из LLM-активных — слот капа свободен"
    );
}

/// Срез честности пробы (#1281): недостоверная проба активности (Err) —
/// отказ сталл-прохода строки фиксируется в outcome как probe_failed,
/// без эскалации и без записи каунтеров пингов.
#[tokio::test]
async fn probe_failure_records_outcome_without_escalation_or_counters() {
    const NOW: u64 = 100_000_000;
    struct BigClock;
    impl Clock for BigClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            NOW
        }
    }
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![dev_task(64, "ses_dev_64")]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        activity_ms: Some(NOW - 25 * 60_000),
        idle_reply: true,
        activity_fails_sessions: Mutex::new(vec!["ses_dev_64".into()]),
        ..Default::default()
    });
    let state = Arc::new(MemState(Mutex::default()));
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: state.clone(),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.probe_failed, 1, "недостоверная проба видна в outcome");
    assert_eq!(out.stall_flagged, 0, "эскалации по недостоверной пробе нет");
    assert_eq!(
        out.soft_pings, 0,
        "пинг по ненадёжному вердикту не отправляется"
    );
    assert!(
        state.0.lock().unwrap().stalls.is_empty(),
        "каунтеры пингов не пишутся при недостоверной пробе"
    );
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("status#64=NeedsAttention")),
        "строка не паркуется по недостоверной пробе"
    );
}

/// Фейк пробы квоты (#1477): состояние переключается тестом — Exhausted /
/// Ok / Err (сцена спада окна = самовозобновление); счётчик вызовов
/// пинит «проба — только на сталл-решении».
struct FakeQuota {
    state: std::sync::Mutex<FakeQuotaState>,
    calls: std::sync::atomic::AtomicUsize,
}
#[derive(Clone, Copy)]
enum FakeQuotaState {
    Exhausted,
    Ok,
    Err,
}
impl FakeQuota {
    fn new(state: FakeQuotaState) -> Self {
        Self {
            state: std::sync::Mutex::new(state),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}
#[async_trait::async_trait]
impl QuotaProbe for FakeQuota {
    async fn probe(&self) -> UsecaseResult<crate::ports::QuotaSnapshot> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match *self.state.lock().unwrap() {
            FakeQuotaState::Exhausted => Ok(crate::ports::QuotaSnapshot {
                exhausted: true,
                window_pct: Some(100),
                reset_at_ms: None,
            }),
            FakeQuotaState::Ok => Ok(crate::ports::QuotaSnapshot {
                exhausted: false,
                window_pct: Some(53),
                reset_at_ms: None,
            }),
            FakeQuotaState::Err => Err(UsecaseError::Session("quota probe (мок)".into())),
        }
    }
}

/// Сталл-сцена с пробой квоты: In Progress-строка с дев-сессией в глубокой
/// тишине и раундом пингов, заработанным текущим процессом.
fn quota_stall_usecase(
    board: Arc<FakeBoard>,
    sess: Arc<FakeSessions>,
    state: Arc<MemState>,
    quota: Option<Arc<dyn QuotaProbe>>,
) -> TickUseCase {
    const NOW: u64 = 100_000_000;
    struct BigClock;
    impl Clock for BigClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            NOW
        }
    }
    TickUseCase {
        board,
        sessions: Arc::new(SessionRouter::uniform(sess)),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state,
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: NOW - 50 * 60_000,
        judge: None,
        judge_cfg: Default::default(),
        quota,
    }
}

fn quota_stall_fixture(
    quota: Option<Arc<dyn QuotaProbe>>,
) -> (Arc<FakeBoard>, Arc<MemState>, TickUseCase) {
    const NOW: u64 = 100_000_000;
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![dev_task(64, "ses_dev_64")]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        activity_ms: Some(NOW - 25 * 60_000),
        idle_reply: true,
        ..FakeSessions::new()
    });
    // раунд пингов текущего процесса: сталл-точка достигнута
    let state = Arc::new(MemState(Mutex::new(MemStateInner {
        stalls: vec![
            (64, Status::InProgress, NOW - 40 * 60_000),
            (64, Status::InProgress, NOW - 35 * 60_000),
        ],
        ..Default::default()
    })));
    let u = quota_stall_usecase(board.clone(), sess, state.clone(), quota);
    (board, state, u)
}

/// Лимитный гейт (#1477): окно исчерпано — тишина классифицирована как
/// лимитная: ни пингов, ни эскалации, ни записи каунтеров раунда.
#[tokio::test]
async fn limit_exhaustion_holds_row_without_pings_escalation_or_counters() {
    let (board, state, u) =
        quota_stall_fixture(Some(Arc::new(FakeQuota::new(FakeQuotaState::Exhausted))));
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.limit_paused, 1, "лимитная пауза видна в outcome");
    assert_eq!(out.stall_flagged, 0, "эскалации по лимитной тишине нет");
    assert_eq!(out.stall_pings, 0, "коммент-пинг на паузе не отправляется");
    assert_eq!(out.soft_pings, 0, "мягкий пинг на паузе не отправляется");
    assert_eq!(
        state.0.lock().unwrap().stalls.len(),
        2,
        "каунтеры раунда не растут — пост-резетный пинг не задержан"
    );
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("status#64=NeedsAttention")),
        "строка не паркуется: лимитная тишина не сбой сессии"
    );
}

/// Окно живо (проба ok) — старая механика без изменений: раунд
/// исчерпан, эскалация как раньше.
#[tokio::test]
async fn quota_probe_ok_keeps_old_mechanics() {
    let (board, _state, u) =
        quota_stall_fixture(Some(Arc::new(FakeQuota::new(FakeQuotaState::Ok))));
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.limit_paused, 0, "исчерпания нет — паузы нет");
    assert_eq!(
        out.stall_flagged, 1,
        "раунд исчерпан, сессия жива — эскалация по старой механике"
    );
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("status#64=NeedsAttention")),
        "парковка обычного сталла не заблокирована гейтом"
    );
}

/// Err пробы квоты — старая механика немедленно: отказ вспомогательной
/// пробы не парализует эскалацию сталл-детекта.
#[tokio::test]
async fn quota_probe_err_keeps_old_mechanics() {
    let (_board, _state, u) =
        quota_stall_fixture(Some(Arc::new(FakeQuota::new(FakeQuotaState::Err))));
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.limit_paused, 0, "Err пробы — паузы нет");
    assert_eq!(
        out.stall_flagged, 1,
        "эскалация живёт при недоступной пробе квоты"
    );
}

/// Самовозобновление (#1477): спад процента между тиками — строка снимает
/// паузу и получает обычный мягкий пинг (idle-ход завершён без транзакции).
/// Гейт не пишет каунтеры — пост-резетный (пост-спадный) пинг не задержан.
#[tokio::test]
async fn limit_pause_releases_when_window_drains() {
    let quota = Arc::new(FakeQuota::new(FakeQuotaState::Exhausted));
    let (_board, state, u) = quota_stall_fixture(Some(quota.clone()));
    // чистый state: раунд пингов не начат — пауза держит строку ДО первого
    // пинга, а не после исчерпания раунда
    state.0.lock().unwrap().stalls.clear();
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.limit_paused, 1,
        "первый тик: окно исчерпано — строка на паузе"
    );
    assert_eq!(
        out.soft_pings, 0,
        "пауза до пинга: мягкий пинг не сжигается в лимитном окне"
    );
    assert!(
        state.0.lock().unwrap().stalls.is_empty(),
        "пауза не пишет каунтеры — раунд не начат"
    );
    *quota.state.lock().unwrap() = FakeQuotaState::Ok;
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.limit_paused, 0, "спад процента — пауза не продлевается");
    assert_eq!(
        out.soft_pings, 1,
        "самовозобновление: обычный мягкий пинг после спада"
    );
}

/// Гейт до судьи (#1477, ревью раунд 2 Common-1): сцена с ВКЛЮЧЁННЫМ
/// контуром судьи (enabled=true, hard-гейт тишины, judge-стадия) —
/// лимитная тишина не сжигает вызов decision-модели; контроль — та же
/// сцена с живым окном: судья вызывается. Перенос гейта ниже судьи
/// краснит первый прогон (вызов случится на лимитной тишине).
#[tokio::test]
async fn limit_pause_skips_judge_call() {
    const NOW: u64 = 100_000_000;
    struct FreezeClock;
    impl Clock for FreezeClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            NOW
        }
    }
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![judge_row(
            64,
            Status::InProgress,
            "ses_dev_64",
            Slot::Dev,
        )]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    // тишина 70 мин, ход не завершён: hard-гейт (60 мин) звучит,
    // In Progress — judge-стадия: старая механика зовёт судью
    let sess = Arc::new(FakeSessions {
        activity_ms: Some(NOW - 70 * 60_000),
        idle_reply: false,
        ..FakeSessions::new()
    });
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::InProgress);
    let state = Arc::new(MemState(Mutex::default()));
    let quota = Arc::new(FakeQuota::new(FakeQuotaState::Exhausted));
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess)),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FreezeClock),
        state,
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: NOW - 50 * 60_000,
        judge: Some(judge.clone()),
        judge_cfg: JudgeCfg {
            enabled: true,
            ..JudgeCfg::default()
        },
        quota: Some(quota.clone()),
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.limit_paused, 1,
        "лимитная пауза сработала на judge-стадии"
    );
    assert_eq!(
        out.judge_calls, 0,
        "гейт до судьи: на лимитной тишине decision-модель не вызывается"
    );
    assert!(
        judge.calls.lock().unwrap().is_empty(),
        "судья не видел лимитной тишины"
    );
    // Контроль порядка: окно живо (проба Ok) — тот же сталл-тик зовёт
    // судью; красный при переносе гейта ниже судьи, если лимитная сцена
    // тоже начинала звать судью (заявленное свойство различает порядок)
    *quota.state.lock().unwrap() = FakeQuotaState::Ok;
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.limit_paused, 0, "окно живо — паузы нет");
    assert_eq!(
        out.judge_calls, 1,
        "контроль: при живом окне судья вызывается на том же тике"
    );
}

/// Проба квоты — только на сталл-решении (#1477, ревью L3): свежая
/// строка (тишина ниже гейтов) не дёргает пробу вовсе.
#[tokio::test]
async fn limit_probe_runs_only_at_stall_decision() {
    const NOW: u64 = 100_000_000;
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![dev_task(64, "ses_dev_64")]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        // активность 5 минут назад (300_000 ms) — ниже мягкого гейта тишины
        activity_ms: Some(NOW - 5 * 60_000),
        idle_reply: true,
        ..FakeSessions::new()
    });
    let quota = Arc::new(FakeQuota::new(FakeQuotaState::Ok));
    let u = quota_stall_usecase(
        board,
        sess,
        Arc::new(MemState(Mutex::default())),
        Some(quota.clone()),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        quota.calls.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "свежая строка не доходит до сталл-решения — проба не зовётся"
    );
    assert_eq!(out.limit_paused, 0);
}

/// Изоляция построчная (#1281): массовый отказ пробы не глушит обработку
/// остальных строк — четыре строки слепы, пятая с достоверной пробой
/// получает свой сталл-вердикт; отказные считаются в outcome.
#[tokio::test]
async fn probe_failure_isolated_per_row_other_rows_processed() {
    const NOW: u64 = 100_000_000;
    struct BigClock;
    impl Clock for BigClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            NOW
        }
    }
    let mut rows = Vec::new();
    for n in 61..66u64 {
        rows.push(dev_task(n, &format!("ses_dev_{n}")));
    }
    let board = Arc::new(FakeBoard {
        rows: rows.into(),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        activity_ms: Some(NOW - 16 * 60_000),
        idle_reply: true,
        // четыре строки слепы — ниже порога деградации (5), изоляция чистая
        activity_fails_sessions: Mutex::new(vec![
            "ses_dev_61".into(),
            "ses_dev_62".into(),
            "ses_dev_63".into(),
            "ses_dev_64".into(),
        ]),
        ..Default::default()
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.probe_failed, 4,
        "полоса отказов считается построчно — ниже порога деградации, изоляция чистая"
    );
    assert_eq!(
        out.soft_pings, 1,
        "строка с достоверной пробой обработана: живой пинг ушёл"
    );
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("NeedsAttention")),
        "массовый отказ пробы не паркует строки"
    );
}

/// Полоса отказов пробы достигает порога деградации (#1281): пять слепых
/// строк — probe_failed на уровне PROBE_DEGRADE_MIN_ROWS, run() сигнализирует
/// деградацию одним громким событием stall.probe_degraded; эскалаций и
/// пингов по недостоверным вердиктам нет.
#[tokio::test]
async fn probe_degradation_strip_reaches_threshold_signal() {
    const NOW: u64 = 100_000_000;
    struct BigClock;
    impl Clock for BigClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            NOW
        }
    }
    let mut rows = Vec::new();
    for n in 71..76u64 {
        rows.push(dev_task(n, &format!("ses_dev_{n}")));
    }
    let board = Arc::new(FakeBoard {
        rows: rows.into(),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        activity_fails_sessions: Mutex::new(vec![
            "ses_dev_71".into(),
            "ses_dev_72".into(),
            "ses_dev_73".into(),
            "ses_dev_74".into(),
            "ses_dev_75".into(),
        ]),
        ..Default::default()
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(BigClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.probe_failed, PROBE_DEGRADE_MIN_ROWS,
        "полоса отказов = порогу деградации: громкое событие stall.probe_degraded"
    );
    assert_eq!(out.stall_flagged, 0, "эскалаций по слепым пробам нет");
    assert_eq!(out.soft_pings, 0);
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("NeedsAttention")),
        "деградация пробы не паркует строки"
    );
}

#[test]
fn stage_prompts_spawned_per_slot() {
    // связка мок-стенда с источником (#1383 круг 3): spawn_prompt обязан
    // нести дословный маркер стадии из STAGE_PROMPTS — на нём
    // классифицирует спавн mock_opencode::stage_action_of_prompt
    let board = BoardPlan {
        board: "B".into(),
        project: "p".into(),
        repo: "a/c".into(),
        checkout: "/tmp/checkout".into(),
        env_root: "/tmp/env".into(),
    };
    let task = Task::new(7, "t".into(), Status::Design);
    for (slot, marker) in crate::pipeline::actions::STAGE_PROMPTS {
        let prompt = crate::pipeline::actions::spawn_prompt(&board, &task, *slot);
        assert!(
                prompt.contains(marker),
                "spawn_prompt слота {slot:?} потерял маркер «{marker}» — мок-стенд перестанет классифицировать стадию"
            );
    }
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
    assert!(
        prompt.contains("SLEEPS after the reply"),
        "промпт объясняет сон сессии"
    );
    assert!(
        prompt.contains("Plan review"),
        "финал хода назван для стадии"
    );
    assert!(
        prompt.contains("Needs attention"),
        "вопрос владельцу — легальный финал"
    );
}

#[tokio::test]
async fn spawn_prompt_test_stage_marker_contract() {
    // #1413: дизамбигуация классификации стадий (ревью 2, coupling) —
    // тест-ревьювер классифицируется по уникальному stage-маркеру
    // «тестовая дельта ветки» (mock_opencode::stage_action_of_prompt),
    // tester-промпт маркера НЕ несёт: имя статуса «Ревью тестирования»
    // живёт в его finish-инструкции (чекпойнт pass) и не должно
    // перехватываться. Контракт двух поверхностей пинится на реальном
    // выводе spawn_prompt — дрейф текста краснеет на юните, а не на
    // дорогом прогоне стенда.
    let board = BoardPlan {
        board: "B".into(),
        project: "p".into(),
        repo: "a/c".into(),
        checkout: "/tmp/checkout".into(),
        env_root: "/tmp/env".into(),
    };
    let tester_prompt = crate::pipeline::actions::spawn_prompt(
        &board,
        &Task::new(8, "t".into(), Status::Testing),
        Slot::Test,
    );
    assert!(
        !tester_prompt.contains("the branch's test delta"),
        "tester-промпт без stage-маркера тест-ревьювера"
    );
    // чекпойнт pass тестировщика: развилка по (tester)-коммитам названа
    assert!(
        tester_prompt.contains("Testing review") && tester_prompt.contains("(tester) commits"),
        "finish-инструкция тестировщика несёт чекпойнт pass: {tester_prompt}"
    );
    let test_review_prompt = crate::pipeline::actions::spawn_prompt(
        &board,
        &Task::new(9, "t".into(), Status::ReviewTesting),
        Slot::TestReview,
    );
    assert!(
        test_review_prompt.contains("the branch's test delta"),
        "stage-маркер тест-ревьювера в промпте: {test_review_prompt}"
    );
    assert!(
        test_review_prompt.contains("«Review»") && test_review_prompt.contains("«Testing»"),
        "finish-инструкция тест-ревьювера называет оба вердикт-статуса: {test_review_prompt}"
    );
}

#[tokio::test]
async fn idle_sessions_do_not_hold_cup() {
    // Регресс 17.11 (#1206-семья): «живая» в API конвейерная сессия,
    // спящая между ходами (никогда не финализируется), считалась
    // LLM-тратящей — кап забивался спящими, все новые спавны холдились
    // всю ночь (active=18 при cap=7). Кап держат только busy-строки
    // (ход идёт) и состоявшиеся спавны тика (#1244); idle-сессии
    // не тратят LLM и кап не держат.
    let mut rows = Vec::new();
    for n in 70..80u64 {
        let mut t = task(n, Status::InProgress);
        let mut f = TaskFields::default();
        f.set_session(
            Slot::Dev,
            uwuwu_domain::SessionRef::parse(&format!("ses_sleep_{n}")).unwrap(),
        );
        t.apply_fields(f);
        rows.push(t);
    }
    rows.push(task(99, Status::InProgress)); // кандидат на спавн
    let state = Arc::new(MemState(Mutex::default()));
    let board = Arc::new(FakeBoard {
        rows: rows.into(),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    // все спящие сессии — живые в API (spawned), но idle (ход завершён)
    let sessions = Arc::new(FakeSessions {
        idle_reply: true,
        prompt_fails: false,
        prompt_lost: Mutex::default(),
        prompt_lost_all: false,
        deleted: Mutex::default(),
        ..Default::default()
    });
    sessions
        .spawned
        .lock()
        .unwrap()
        .extend((70..80u64).map(|n| format!("ses_sleep_{n}")));
    let prs = Arc::new(FakePrs {
        state: PrState::Missing,
        merges: Mutex::default(),
        probes: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: prs.clone(),
        clock: Arc::new(FixedClock),
        state,
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.spawns_done, 1,
        "кандидат спавнится: спящие idle-сессии кап не держат"
    );
    assert_eq!(
        out.held_cup, 0,
        "active = busy, очередь кандидатов кап не держит (#1244)"
    );
}

#[tokio::test]
async fn spawn_backlog_fills_cap_without_deadlock() {
    // #1244 (CUP-дедлок): 10 строк-кандидатов без сессий, cap 7 —
    // очередь НЕ держит кап сама на себя: 7 спавнов проходят
    // (busy=0 + состоявшиеся спавны тика), 3 ждут следующего тика.
    // Старая формула считала кандидатов активными: 10 ≥ 7 → held
    // всем навечно, ни один спавн (прод 23:15 UTC: spawns=0, held=24).
    let rows: Vec<Task> = (100..110u64).map(|n| task(n, Status::InProgress)).collect();
    let (u, _, _sessions, _) = usecase(rows, PrState::Missing);
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.spawns_done, 7,
        "бэклог заполняет кап без самоблокировки"
    );
    assert_eq!(out.held_cup, 3, "излишек ждёт следующего тика");
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
    let sessions = Arc::new(FailingCreate);
    let state = Arc::new(MemState(Mutex::default()));
    let (mut u, _, _, _) = usecase(vec![task(40, Status::InProgress)], PrState::Missing);
    u.sessions = Arc::new(SessionRouter::uniform(sessions.clone()));
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
use crate::pipeline::dreamer::DreamerUseCase;
use uwuwu_domain::AgentSlot;

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
    analyst_uc_with_cup(rows, state, sessions, 7)
}

fn review_uc(
    rows: Vec<Task>,
    state: Arc<MemState>,
    sessions: Arc<dyn AgentSessionGateway>,
) -> (AnalystReviewUseCase, Arc<FakeBoard>) {
    review_uc_with_cup(rows, state, sessions, 7)
}

#[tokio::test]
async fn analyst_multiboard_no_oversell_within_tick() {
    // #1244-класс (ревью #1231, раунд 2): снимок CUP один, спавны волн
    // тика занимают слоты — при busy == cap−1 волна второй доски
    // откладывается, а не спавнится поверх (оверселл cap+N−1).
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    sessions
        .extra
        .lock()
        .unwrap()
        .push(fake_session("ses_stage", "developer", "", 100, true));
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![
            titled(400, "заявка 1", Status::Analys),
            titled(401, "заявка 2", Status::Analys),
        ]),
        log: Mutex::default(),
        comments: vec![],
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let u = AnalystUseCase {
        board: board.clone(),
        boards: vec![
            AnalystBoard {
                project: "p".into(),
                board: "B1".into(),
                checkout: "/checkout-1".into(),
            },
            AnalystBoard {
                project: "p".into(),
                board: "B2".into(),
                checkout: "/checkout-2".into(),
            },
        ],
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        state,
        clock: Arc::new(FixedClock),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        cup: 2,
    };
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 1, "вторая волна отложена занятым слотом");
    let specs = sessions.specs.lock().unwrap();
    assert_eq!(specs.len(), 1, "спавн только первой доски");
    assert_eq!(specs[0].directory, "/checkout-1");
}

/// Билдер юзкейса сайдкара аналитики: доска + роутер + кап (общая
/// механика для аналитика и ревьювера, ревью #1231 similar-code).
macro_rules! sidecar_uc_with_cup {
    ($fn_name:ident, $ty:ident) => {
        fn $fn_name(
            rows: Vec<Task>,
            state: Arc<MemState>,
            sessions: Arc<dyn AgentSessionGateway>,
            cup: usize,
        ) -> ($ty, Arc<FakeBoard>) {
            let board = Arc::new(FakeBoard {
                rows: rows.into(),
                log: Mutex::default(),
                comments: vec![],
                fields_fails: false,
                set_session_fails: false,
                clear_session_fails: AtomicBool::new(false),
                status_fails: AtomicBool::new(false),
            });
            let u = $ty {
                board: board.clone(),
                boards: vec![analyst_board()],
                sessions: Arc::new(SessionRouter::uniform(sessions)),
                state,
                clock: Arc::new(FixedClock),
                models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
                cup,
            };
            (u, board.clone())
        }
    };
}
sidecar_uc_with_cup!(analyst_uc_with_cup, AnalystUseCase);
sidecar_uc_with_cup!(review_uc_with_cup, AnalystReviewUseCase);

/// Прогон тика аналитики с предзаряженным стейтом: возвращает стейт
/// после тика (сценарии сброса/чистки гвардов).
async fn analyst_tick_precharged(inner: MemStateInner, rows: Vec<Task>) -> Arc<MemState> {
    let state = Arc::new(MemState(Mutex::new(inner)));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = analyst_uc(rows, state.clone(), sessions);
    u.tick().await.unwrap();
    state
}

fn titled(n: u64, title: &str, status: Status) -> Task {
    Task::new(n, title.into(), status)
}

fn fake_session(
    id: &str,
    agent: &str,
    outcome: &str,
    tokens: u64,
    busy: bool,
) -> crate::ports::SessionInfo {
    crate::ports::SessionInfo {
        id: id.into(),
        agent: agent.into(),
        directory: String::new(),
        updated_ms: 0,
        outcome: outcome.into(),
        total_tokens: tokens,
        busy,
    }
}

#[tokio::test]
async fn analyst_wave_held_when_cup_full() {
    // Единый кап (#1231): волна аналитики — такой же запуск хода, как
    // спавн стадии. При полном капе волна откладывается; гарды и
    // кулдауны строк не заряжаются (отложка — не спавн).
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    sessions
        .extra
        .lock()
        .unwrap()
        .push(fake_session("ses_stage", "developer", "", 100, true));
    let (u, board) = analyst_uc_with_cup(
        vec![titled(300, "заявка на разбор", Status::Analys)],
        state.clone(),
        sessions.clone(),
        1,
    );
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 0, "волна при полном капе не спавнится");
    assert!(sessions.spawned.lock().unwrap().is_empty(), "спавна нет");
    assert!(
        state.0.lock().unwrap().analys_ms.is_empty(),
        "кулдауны строк не заряжаются отложкой"
    );
    assert!(
        state.0.lock().unwrap().analys_loops.is_empty(),
        "цикл-гвард не заряжается отложкой"
    );
    assert!(board.log.lock().unwrap().is_empty(), "статусы не тронуты");
}

#[tokio::test]
async fn analyst_wave_degrades_on_partial_outage() {
    // Смешанное здоровье (ревью #1450): один исполнитель отказной, живой
    // отвечает — волна идёт (busy/alive-гвард по живому union). Бывший
    // strict-скан падал Err на любом отказе и ставил волну.
    let state = Arc::new(MemState(Mutex::default()));
    let live = Arc::new(FakeSessions::new());
    let mut failing = FakeSessions::new();
    failing.list_fails = true;
    let failing = Arc::new(failing);
    let (mut u, _) = analyst_uc_with_cup(
        vec![titled(302, "заявка на разбор", Status::Analys)],
        state,
        live.clone(),
        7,
    );
    u.sessions = Arc::new(SessionRouter::new(
        "kisa",
        live.clone(),
        vec![("redmi".into(), failing)],
        std::collections::HashMap::new(),
    ));
    let spawned = u
        .tick()
        .await
        .expect("частичный отказ — не Err, деградация по исполнителю");
    assert_eq!(
        spawned, 1,
        "волна идёт при частичном отказе — busy по живому union"
    );
    assert_eq!(
        live.specs.lock().unwrap().len(),
        1,
        "спавн ушёл живому исполнителю"
    );
}

#[tokio::test]
async fn analyst_alive_guard_sees_second_turn() {
    // Регресс #1231: outcome — атрибут последнего финала, при новом ходе
    // не очищается. Аналитик на 2+ ходе (wakeup, busy=true, финал уже
    // был) старым outcome-гвардом не замечался → дубль-волна поверх
    // живого аналитика.
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    sessions.extra.lock().unwrap().push(fake_session(
        "ses_analyst",
        "analyst",
        "succeeded",
        71_000,
        true,
    ));
    let (u, _) = analyst_uc_with_cup(
        vec![titled(301, "заявка на разбор", Status::Analys)],
        state,
        sessions.clone(),
        7,
    );
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 0, "живой аналитик (2+ ход) блокирует волну");
    assert!(sessions.spawned.lock().unwrap().is_empty());
}

#[tokio::test]
async fn analyst_review_held_when_cup_full() {
    // Ревью-волна аналитики подчинена единому капу, как спавн стадии.
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    sessions
        .extra
        .lock()
        .unwrap()
        .push(fake_session("ses_stage", "developer", "", 100, true));
    let (u, _) = review_uc_with_cup(
        vec![titled(302, "проработанная заявка", Status::ReviewAnalys)],
        state,
        sessions.clone(),
        1,
    );
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 0, "ревью-волна при полном капе не спавнится");
    assert!(sessions.spawned.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dreamer_held_when_cup_full() {
    // Дример-спавн — тоже запуск хода: при полном капе откладывается.
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
    let sessions = Arc::new(FakeSessions::new());
    // готовая цель дримера (финал, токены, idle > 30 мин) + чужой ход
    sessions.extra.lock().unwrap().push(fake_session(
        "ses_target",
        "developer",
        "succeeded",
        500,
        false,
    ));
    sessions
        .extra
        .lock()
        .unwrap()
        .push(crate::ports::SessionInfo {
            updated_ms: 100_000_000 - 31 * 60_000,
            ..fake_session("ses_stage", "developer", "", 0, true)
        });
    let u = DreamerUseCase {
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        state: Arc::new(MemState(Mutex::default())),
        clock: Arc::new(BigClock),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        checkout: "/tmp/checkout".into(),
        cup: 1,
    };
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 0, "дример-спавн при полном капе отложен");
    assert!(sessions.spawned.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dreamer_revival_of_dreamed_session_reprompts() {
    // #1306: отдрименная сессия получила новые сообщения (revival → финал
    // хода → idle ≥ 30 мин) — дример обязан дообработать дополнение
    // повторным прогоном (ADR-005: инвариант 1:1, wakeup-модель).
    let state = Arc::new(MemState(Mutex::default()));
    // Дрим в T1; revival — активность T2 > T1 (дополнение после дрима).
    state.mark_dreamer("ses_target", DreamerMark::UpTo(DREAM_NOW - 40 * 60_000));
    let sessions = Arc::new(FakeSessions::new());
    sessions
        .extra
        .lock()
        .unwrap()
        .push(crate::ports::SessionInfo {
            updated_ms: DREAM_NOW - 31 * 60_000,
            ..fake_session("ses_target", "developer", "succeeded", 700, false)
        });
    let u = dreamer_uc(state, sessions.clone(), 7);
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 1, "revival отдрименной сессии — повторный прогон");
}

// ---- revival/wakeup контур (#1306, инвариант 1:1 — ADR-005) ----

/// ms «сейчас» дример-сценариев (локальный FixedClock-эквивалент).
const DREAM_NOW: u64 = 100_000_000;

/// DreamerUseCase с локальными часами (now = DREAM_NOW) и канон-моделями.
fn dreamer_uc(state: Arc<MemState>, sessions: Arc<FakeSessions>, cup: usize) -> DreamerUseCase {
    struct DreamClock;
    impl Clock for DreamClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            DREAM_NOW
        }
    }
    DreamerUseCase {
        sessions: Arc::new(SessionRouter::uniform(sessions)),
        state,
        clock: Arc::new(DreamClock),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        checkout: "/tmp/checkout".into(),
        cup,
    }
}

/// Кандидат-источник: дримлен в T1, дополнен в T2 > T1, idle ≥ 30 мин.
fn revived_target() -> crate::ports::SessionInfo {
    crate::ports::SessionInfo {
        updated_ms: DREAM_NOW - 31 * 60_000,
        ..fake_session("ses_target", "developer", "succeeded", 700, false)
    }
}

#[tokio::test]
async fn dreamer_repeat_wakes_linked_session_without_new_spawn() {
    // ADR-005 1:1: revival отдрименной сессии — wakeup ТОЙ ЖЕ дример-сессии
    // («повторный прогон»), нового спавна нет; watermark продвигается.
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_dreamer("ses_target", DreamerMark::UpTo(DREAM_NOW - 40 * 60_000));
    state.link_dreamer_session("ses_target", "ses_dream");
    let sessions = Arc::new(FakeSessions::new());
    sessions.extra.lock().unwrap().push(revived_target());
    // живой носитель из реестра линков (дример-агент — вне кандидатов)
    sessions.extra.lock().unwrap().push(fake_session(
        "ses_dream",
        "dreamer",
        "succeeded",
        9_000,
        false,
    ));
    let u = dreamer_uc(state.clone(), sessions.clone(), 7);
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 1);
    let prompts = sessions.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1, "одно дрим-действие за тик");
    assert!(
        prompts[0].starts_with("ses_dream|"),
        "промпт в связанную дример-сессию"
    );
    assert!(
        prompts[0].contains("Repeat run"),
        "wakeup-промпт повторного прогона"
    );
    assert!(
        prompts[0].contains("ses_target"),
        "промпт называет источник"
    );
    drop(prompts);
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "нового спавна нет — инвариант 1:1"
    );
    assert_eq!(
        state.dreamer_mark("ses_target"),
        Some(DreamerMark::UpTo(DREAM_NOW - 31 * 60_000)),
        "watermark продвинут на позицию revival'а"
    );
}

#[tokio::test]
async fn dreamer_without_revival_stays_silent() {
    // Контроль зелёного фона: дрим без последующей активности — тишина.
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_dreamer("ses_target", DreamerMark::UpTo(DREAM_NOW - 31 * 60_000));
    let sessions = Arc::new(FakeSessions::new());
    sessions.extra.lock().unwrap().push(revived_target());
    let u = dreamer_uc(state, sessions.clone(), 7);
    assert_eq!(u.tick().await.unwrap(), 0, "без revival дример молчит");
    assert!(sessions.prompts.lock().unwrap().is_empty());
    assert!(sessions.spawned.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dreamer_forever_mark_skips_even_after_activity() {
    // Боты (Forever) — не источники опыта: активность их не реанимирует.
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_dreamer("ses_target", DreamerMark::Forever);
    let sessions = Arc::new(FakeSessions::new());
    sessions.extra.lock().unwrap().push(revived_target());
    let u = dreamer_uc(state, sessions.clone(), 7);
    assert_eq!(u.tick().await.unwrap(), 0);
    assert!(sessions.prompts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dreamer_respawns_when_linked_dreamer_session_dead() {
    // Линк на умерший носитель: спавн нового + релинк + повторный прогон.
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_dreamer("ses_target", DreamerMark::UpTo(DREAM_NOW - 40 * 60_000));
    state.link_dreamer_session("ses_target", "ses_gone");
    let sessions = Arc::new(FakeSessions::new());
    sessions.extra.lock().unwrap().push(revived_target());
    let u = dreamer_uc(state.clone(), sessions.clone(), 7);
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 1, "мёртвый носитель — респавн");
    let created = sessions.spawned.lock().unwrap().clone();
    assert_eq!(created.len(), 1);
    assert_eq!(
        state.dreamer_session("ses_target"),
        Some(created[0].clone()),
        "линк перезаписан на новый носитель"
    );
    let prompts = sessions.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].starts_with(&format!("{}|", created[0])),
        "прогон — в новый носитель"
    );
    assert!(
        prompts[0].contains("Post-processing"),
        "новый носитель без контекста дедупа получает полный промпт"
    );
}

#[tokio::test]
async fn dreamer_first_run_links_created_session() {
    // Первый прогон: спавн + связка «исходная → её дример-сессия».
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    sessions.extra.lock().unwrap().push(revived_target());
    let u = dreamer_uc(state.clone(), sessions.clone(), 7);
    let spawned = u.tick().await.unwrap();
    assert_eq!(spawned, 1);
    let created = sessions.spawned.lock().unwrap().clone();
    assert_eq!(created.len(), 1);
    assert_eq!(
        state.dreamer_session("ses_target"),
        Some(created[0].clone()),
        "первый прогон связывает носителя с источником"
    );
    let prompts = sessions.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].contains("Post-processing"),
        "новый носитель получает полный промпт пост-обработки"
    );
}

#[tokio::test]
async fn dreamer_repeat_run_counts_into_day_cap() {
    // Повторный прогон — такой же запуск хода: дневной кап его считает.
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_dreamer("ses_target", DreamerMark::UpTo(DREAM_NOW - 40 * 60_000));
    state.link_dreamer_session("ses_target", "ses_dream");
    let day = crate::pipeline::dreamer::jst_day_key(DREAM_NOW);
    for _ in 0..24 {
        state.mark_dreamer_spawn(&day);
    }
    let sessions = Arc::new(FakeSessions::new());
    sessions.extra.lock().unwrap().push(revived_target());
    sessions.extra.lock().unwrap().push(fake_session(
        "ses_dream",
        "dreamer",
        "succeeded",
        9_000,
        false,
    ));
    let u = dreamer_uc(state, sessions.clone(), 7);
    assert_eq!(
        u.tick().await.unwrap(),
        0,
        "исчерпанный дневной кап держит и повторный прогон"
    );
    assert!(sessions.prompts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dreamer_busy_dreamer_blocks_repeat_wakeup() {
    // Живой дример (ход в полёте) блокирует и wakeup-путь — ≤1 живого.
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_dreamer("ses_target", DreamerMark::UpTo(DREAM_NOW - 40 * 60_000));
    state.link_dreamer_session("ses_target", "ses_dream");
    let sessions = Arc::new(FakeSessions::new());
    sessions.extra.lock().unwrap().push(revived_target());
    sessions.extra.lock().unwrap().push(fake_session(
        "ses_dream",
        "dreamer",
        "succeeded",
        9_000,
        true,
    ));
    let u = dreamer_uc(state, sessions.clone(), 7);
    assert_eq!(
        u.tick().await.unwrap(),
        0,
        "живой дример — спавна и wakeup нет"
    );
    assert!(sessions.prompts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dreamer_prune_drops_dead_session_state() {
    // Гигиена: метки/линки сессий вне живого скана сносятся — рост state
    // ограничен живыми сессиями (500-граница легаси ушла, #1306).
    let state = Arc::new(MemState(Mutex::default()));
    state.mark_dreamer("ses_live", DreamerMark::UpTo(DREAM_NOW - 40 * 60_000));
    state.mark_dreamer("ses_dead", DreamerMark::UpTo(DREAM_NOW - 40 * 60_000));
    state.link_dreamer_session("ses_live", "ses_l1");
    state.link_dreamer_session("ses_dead", "ses_d1");
    let sessions = Arc::new(FakeSessions::new());
    // живая сессия — сама в скане (кандидат); ses_target/ses_dead вне него
    sessions
        .extra
        .lock()
        .unwrap()
        .push(crate::ports::SessionInfo {
            updated_ms: DREAM_NOW - 31 * 60_000,
            ..fake_session("ses_live", "developer", "succeeded", 700, false)
        });
    let u = dreamer_uc(state.clone(), sessions, 7);
    assert_eq!(u.tick().await.unwrap(), 1);
    assert!(
        state.dreamer_mark("ses_dead").is_none(),
        "метка мёртвой сессии снесена"
    );
    assert!(
        state.dreamer_session("ses_dead").is_none(),
        "линк мёртвой сессии снесён"
    );
    assert_eq!(
        state.dreamer_mark("ses_live"),
        Some(DreamerMark::UpTo(DREAM_NOW - 31 * 60_000)),
        "живая сессия обработана штатно"
    );
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
    assert_eq!(
        specs[0].model,
        uwuwu_domain::SpawnModel::parse("zai-coding-plan/glm-5.3-flash#high")
            .unwrap()
            .model,
        "модель аналитика — карта сервера (#1154)"
    );
    assert_eq!(specs[0].variant.as_deref(), Some("high"));
    // Todo-строка мимо волны; обе Analys-строки в пачке, счётчик первой попытки
    let inner = state.0.lock().unwrap();
    assert_eq!(inner.analys_loops.len(), 2, "только строки волны");
    for (n, st, r) in &inner.analys_loops {
        assert_eq!(*st, Status::Analys);
        assert_eq!(*r, 0, "строка #{n}: захват без круга (#1241)");
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
    // живой аналитик — волна ждёт («живой» = ход в полёте, busy; #1231)
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::with_live("analyst"));
    sessions
        .busy_ids
        .lock()
        .unwrap()
        .push("ses_live_analyst".into());
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
    sessions2
        .busy_ids
        .lock()
        .unwrap()
        .push("ses_live_analyst-reviewer".into());
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
        analys_ms: vec![("B".into(), 1, 2000)],
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
    let state = analyst_tick_precharged(
        MemStateInner {
            analys_loops: vec![(2, Status::Analys, 2)],
            ..Default::default()
        },
        vec![titled(2, "в пайпе", Status::Todo)],
    )
    .await;
    assert!(
        state.0.lock().unwrap().analys_loops.is_empty(),
        "вне цикла Analys↔Review Analys счётчик сбрасывается"
    );
}

#[tokio::test]
async fn analyst_verdictless_waves_do_not_burn_cycles() {
    // #1241: счётчик цикл-гварда считает реальные круги (строка сходила
    // в Review Analys и вернулась), а не захваты волны: умирающая/без-
    // вердиктная сессия аналитика круг НЕ сжигает — строка ретраится
    // кулдауном, ложной эскалации в Needs attention нет
    use std::sync::atomic::{AtomicU64, Ordering};
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
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![titled(1, "заявка", Status::Analys)]),
        log: Mutex::default(),
        comments: vec![],
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let clock = Arc::new(AdvClock(AtomicU64::new(2000)));
    let u = AnalystUseCase {
        board: board.clone(),
        boards: vec![analyst_board()],
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        state: state.clone(),
        clock: clock.clone(),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        cup: 7,
    };
    // четыре волны подряд (сессия аналитика гибнет без вердикта, строка
    // не двигается) — старый код сжигал круг за кругом и эскалировал
    for _ in 0..4 {
        assert_eq!(u.tick().await.unwrap(), 1, "волна берёт строку (ретрай)");
        clock.0.fetch_add(16 * 60_000, Ordering::SeqCst); // кулдаун прошёл
    }
    assert_eq!(sessions.spawned.lock().unwrap().len(), 4, "4 ретрай-волны");
    let rounds = state
        .0
        .lock()
        .unwrap()
        .analys_loops
        .iter()
        .find(|(n, _, _)| *n == 1)
        .map(|(_, _, r)| *r);
    assert_eq!(rounds, Some(0), "захват волны круг не сжигает");
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("status#1=")),
        "ложной эскалации нет — строка жива в Analys"
    );
}

/// Скаффолдинг круг-наблюдения (#1241): прелоад пары → тик → пара после.
/// Различие тестов — статус строки и ожидаемая пара.
async fn analyst_pair_after_tick(preload: (Status, u32), row_status: Status) -> (Status, u32) {
    let inner = MemStateInner {
        analys_loops: vec![(1, preload.0, preload.1)],
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    let (u, _) = analyst_uc(
        vec![titled(1, "строка круга", row_status)],
        state.clone(),
        Arc::new(FakeSessions::new()),
    );
    u.tick().await.unwrap();
    let guard = state.0.lock().unwrap();
    guard
        .analys_loops
        .iter()
        .find(|(n, _, _)| *n == 1)
        .map(|(_, st, r)| (*st, *r))
        .unwrap()
}

#[tokio::test]
async fn analyst_real_circle_increments_counter() {
    // #1241: реальный круг — последняя увиденная пара (ReviewAnalys, R),
    // строка снова в Analys: наблюдение инкрементит счётчик
    assert_eq!(
        analyst_pair_after_tick((Status::ReviewAnalys, 1), Status::Analys).await,
        (Status::Analys, 2),
        "реальный круг засчитан, пара — (Analys, 2)"
    );
}

#[tokio::test]
async fn analyst_review_seen_updates_pair_without_increment() {
    // строка ушла в Review Analys — наблюдение фиксирует пару без
    // инкремента: круг завершится только возвратом в Analys
    assert_eq!(
        analyst_pair_after_tick((Status::Analys, 1), Status::ReviewAnalys).await,
        (Status::ReviewAnalys, 1),
        "наблюдение ревью без инкремента"
    );
}

#[tokio::test]
async fn analyst_three_real_rounds_escalate_with_honest_message() {
    // #1241: гвард не вырожден — три РЕАЛЬНЫХ круга (уход в Review Analys
    // и возврат, собранные наблюдением статуса) эскалируют в Needs
    // attention, спавнов после лимита нет, текст коммента владельцу
    // честный («реальных круга», не захваты волн); до лимита строка
    // ретраится волнами как обычно
    use std::sync::atomic::{AtomicU64, Ordering};
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
    let row = |status: Status| vec![titled(1, "закольцованная", status)];
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    let clock = Arc::new(AdvClock(AtomicU64::new(2000)));
    let board = |status: Status| {
        Arc::new(FakeBoard {
            rows: std::sync::Mutex::new(row(status)),
            log: Mutex::default(),
            comments: vec![],
            fields_fails: false,
            set_session_fails: false,
            clear_session_fails: AtomicBool::new(false),
            status_fails: AtomicBool::new(false),
        })
    };
    let mut u = AnalystUseCase {
        board: board(Status::Analys),
        boards: vec![analyst_board()],
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        state: state.clone(),
        clock: clock.clone(),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        cup: 7,
    };
    let pair = |state: &Arc<MemState>| {
        state
            .0
            .lock()
            .unwrap()
            .analys_loops
            .iter()
            .find(|(n, _, _)| *n == 1)
            .map(|(_, st, r)| (*st, *r))
    };
    // захват волны: спавн, пара (Analys, 0), круг не сжигается
    u.tick().await.unwrap();
    assert_eq!(pair(&state), Some((Status::Analys, 0)), "захват без круга");
    let mut cur;
    // круг 1 и круг 2 — пинг-понг: волна на возврате ретраит строку
    for round in 1..=2u32 {
        cur = board(Status::ReviewAnalys);
        u.board = cur.clone();
        u.tick().await.unwrap();
        assert_eq!(
            pair(&state),
            Some((Status::ReviewAnalys, round - 1)),
            "уход в ревью круга {round} зафиксирован"
        );
        clock.0.fetch_add(16 * 60_000, Ordering::SeqCst);
        cur = board(Status::Analys);
        u.board = cur.clone();
        u.tick().await.unwrap();
        assert_eq!(
            pair(&state),
            Some((Status::Analys, round)),
            "круг {round} засчитан"
        );
    }
    assert_eq!(
        sessions.spawned.lock().unwrap().len(),
        3,
        "захват + два возврата — строка ретраится до лимита"
    );
    // круг 3: возврат инкрементит до лимита — эскалация на том же тике
    cur = board(Status::ReviewAnalys);
    u.board = cur.clone();
    u.tick().await.unwrap();
    assert_eq!(
        pair(&state),
        Some((Status::ReviewAnalys, 2)),
        "уход в ревью круга 3 зафиксирован"
    );
    clock.0.fetch_add(16 * 60_000, Ordering::SeqCst);
    cur = board(Status::Analys);
    u.board = cur.clone();
    u.tick().await.unwrap();
    assert_eq!(
        sessions.spawned.lock().unwrap().len(),
        3,
        "за лимитом волна не спавнится"
    );
    let log = cur.log.lock().unwrap().clone(); // борд возврата круга 3 — эскалация здесь
    assert!(
        log.iter().any(|l| l == "status#1=NeedsAttention"),
        "строка ушла в Needs attention: {log:?}"
    );
    let comment = log
        .iter()
        .find(|l| l.starts_with("comment#1="))
        .expect("коммент владельцу есть");
    assert!(
        comment.contains("3 full rounds (Review Analys → Analys)"),
        "текст эскалации честный: {comment:?}"
    );
    assert!(
        !comment.contains("Analys↔Review Analys"),
        "старой формулировки «захваты = круги» нет"
    );
    assert!(
        state.0.lock().unwrap().analys_loops.is_empty(),
        "счётчик после эскалации сброшен"
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
    assert_eq!(
        specs[0].model,
        uwuwu_domain::SpawnModel::parse("zai-coding-plan/glm-5.3-flash#high")
            .unwrap()
            .model,
        "модель ревьювера аналитики — карта сервера (#1154)"
    );
    assert_eq!(specs[0].variant.as_deref(), Some("high"));
}

#[tokio::test]
async fn reviewer_alive_skip_and_cooldown() {
    // парность: живой ревьювер — спавна нет («живой» = ход в полёте, #1231)
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::with_live("analyst-reviewer"));
    sessions
        .busy_ids
        .lock()
        .unwrap()
        .push("ses_live_analyst-reviewer".into());
    let (u, _) = review_uc(vec![titled(1, "x", Status::ReviewAnalys)], state, sessions);
    assert_eq!(u.tick().await.unwrap(), 0);
    // кулдаун по строке
    let inner = MemStateInner {
        analys_review_ms: vec![("B".into(), 1, 2000)],
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
    async fn set_status(
        &self,
        _b: &str,
        n: u64,
        s: Status,
    ) -> UsecaseResult<crate::ports::StatusDelivery> {
        self.log.lock().unwrap().push(format!("status#{n}={s:?}"));
        Ok(crate::ports::StatusDelivery::Confirmed {
            actual: crate::naming::status_name(s).to_string(),
        })
    }
    async fn row_status(&self, board: &str, n: u64) -> UsecaseResult<Option<Status>> {
        Ok(self
            .boards
            .iter()
            .find(|(b, _)| b == board)
            .and_then(|(_, rows)| rows.iter().find(|t| t.number() == n))
            .map(|t| t.status()))
    }
    async fn row_exists(&self, board: &str, n: u64) -> UsecaseResult<bool> {
        Ok(self
            .boards
            .iter()
            .find(|(b, _)| b == board)
            .map(|(_, rows)| rows.iter().any(|t| t.number() == n))
            .unwrap_or(false))
    }
    async fn set_session(&self, _b: &str, _n: u64, _s: Slot, _sess: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_branch(&self, _: &str, _: u64, _: &uwuwu_domain::BranchName) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_board_cells(
        &self,
        _b: &str,
        _n: u64,
        _p: Option<&str>,
        _a: Option<bool>,
        _d: Option<bool>,
    ) -> UsecaseResult<()> {
        Ok(())
    }

    async fn clear_session(&self, _b: &str, _n: u64, _s: Slot) -> UsecaseResult<()> {
        Ok(())
    }
    async fn issue_fields(&self, _n: u64) -> UsecaseResult<TaskFields> {
        Ok(TaskFields::default())
    }
    async fn post_comment(&self, n: u64, _body: &str) -> UsecaseResult<String> {
        self.log.lock().unwrap().push(format!("comment#{n}"));
        Ok(format!("multi-comment#{n}"))
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
        _kind: Option<&str>,
    ) -> UsecaseResult<u64> {
        Ok(1)
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
    assert!(
        state.0.lock().unwrap().own.is_empty(),
        "фабричный пост эскалации мимо реестра серверных постов (#1204) — \
         сигнал будит адресата как foreign"
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
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        state: Arc::new(MemState(Mutex::default())),
        clock: Arc::new(FixedClock),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        cup: 7,
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
    let state = analyst_tick_precharged(
        MemStateInner {
            analys_review_loops: vec![(9, Status::ReviewAnalys, 1)],
            ..Default::default()
        },
        vec![titled(1, "живая", Status::Analys)],
    )
    .await;
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
        // #1 в Review Analys: мимо волны и мимо stale-сброса — тик изменяет
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
        sessions: Arc::new(SessionRouter::uniform(sessions)),
        state: state.clone(),
        clock: Arc::new(FixedClock),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        cup: 7,
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
        "счётчик живой строки прочитанной доски B1 не стёрт (prune отложен из-за сбоя B2)"
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
    async fn set_status(
        &self,
        b: &str,
        n: u64,
        s: Status,
    ) -> UsecaseResult<crate::ports::StatusDelivery> {
        if !self.board_of(b)?.contains(&n) {
            return Err(crate::error::UsecaseError::Board(format!(
                "строка #{n} не на доске «{b}»"
            )));
        }
        self.log.lock().unwrap().push(format!("status#{n}={s:?}"));
        Ok(crate::ports::StatusDelivery::Confirmed {
            actual: crate::naming::status_name(s).to_string(),
        })
    }
    async fn row_status(&self, board: &str, n: u64) -> UsecaseResult<Option<Status>> {
        Ok(self.board_of(board)?.contains(&n).then_some(Status::Todo))
    }
    async fn row_exists(&self, board: &str, n: u64) -> UsecaseResult<bool> {
        Ok(self.board_of(board)?.contains(&n))
    }
    async fn set_session(&self, _: &str, _: u64, _: Slot, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_branch(&self, _: &str, _: u64, _: &uwuwu_domain::BranchName) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_board_cells(
        &self,
        _b: &str,
        _n: u64,
        _p: Option<&str>,
        _a: Option<bool>,
        _d: Option<bool>,
    ) -> UsecaseResult<()> {
        Ok(())
    }

    async fn clear_session(&self, _: &str, _: u64, _: Slot) -> UsecaseResult<()> {
        Ok(())
    }
    async fn issue_fields(&self, _: u64) -> UsecaseResult<TaskFields> {
        Ok(TaskFields::default())
    }
    async fn post_comment(&self, _: u64, _: &str) -> UsecaseResult<String> {
        Ok(String::new())
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
    async fn create_issue(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> UsecaseResult<u64> {
        Ok(1)
    }
    async fn set_issue_type(&self, _: u64, _: &str) -> UsecaseResult<()> {
        Ok(())
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
    async fn archive_row(&self, _: &str, _: u64) -> UsecaseResult<()> {
        Ok(())
    }
    async fn issues_with_title(&self, _: &str) -> UsecaseResult<Vec<u64>> {
        Ok(Vec::new())
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
        clock: Arc::new(FixedClock),
        state: Arc::new(MemState(Mutex::default())),
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

#[tokio::test]
async fn task_close_row_on_first_board_still_closes_there() {
    // ветка скан-цикла close (#1135): строка первой доски конфига
    // закрывается как раньше — мультиборд-скан не сломал старое поведение.
    let board = Arc::new(CloseBoard {
        boards: vec![("B1".into(), vec![1]), ("B2".into(), vec![5])],
        log: Mutex::default(),
    });
    let fields: Arc<dyn crate::ports::IssueFieldStore> = Arc::new(FakeFields);
    let writer = crate::tasks::write::TaskWriteUseCase {
        board: board.clone(),
        fields,
        boards: vec![("uwuwu".into(), "B1".into()), ("origa".into(), "B2".into())],
        clock: Arc::new(FixedClock),
        state: Arc::new(MemState(Mutex::default())),
    };
    use crate::tasks::write::TaskWriter;

    // Act
    writer
        .close(1, false)
        .await
        .expect("close строки первой доски");

    // Assert
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|e| e == "status#1=Done"),
        "строка первой доски переведена в Done на её доске, лог: {log:?}"
    );
    assert!(
        log.iter().all(|e| !e.contains("status#5=")),
        "вторая доска close не трогает, лог: {log:?}"
    );
}

#[tokio::test]
async fn task_close_missing_row_is_a_loud_error_without_mutations() {
    // ветка скан-цикла close (#1135): строки нет ни на одной доске
    // конфига — громкая ошибка, ни одна доска не мутирована.
    let board = Arc::new(CloseBoard {
        boards: vec![("B1".into(), vec![1]), ("B2".into(), vec![5])],
        log: Mutex::default(),
    });
    let fields: Arc<dyn crate::ports::IssueFieldStore> = Arc::new(FakeFields);
    let writer = crate::tasks::write::TaskWriteUseCase {
        board: board.clone(),
        fields,
        boards: vec![("uwuwu".into(), "B1".into()), ("origa".into(), "B2".into())],
        clock: Arc::new(FixedClock),
        state: Arc::new(MemState(Mutex::default())),
    };
    use crate::tasks::write::TaskWriter;

    // Act
    let result = writer.close(99, true).await;

    // Assert
    let err = result.unwrap_err().to_string();
    assert!(err.contains("#99"), "отказ называет искомую строку: {err}");
    let log = board.log.lock().unwrap();
    assert!(
        log.is_empty(),
        "промах close не мутирует доски, лог: {log:?}"
    );
}

// ===== Тайтлы вневых ролей — эмодзи-маркер (#1212): спавн-тайтл каждой
// роли несёт маркер таблицы Agent::emoji; если маппинг отломают — тест падает.

#[tokio::test]
async fn analyst_family_titles_carry_role_marker() {
    // 🧠/⚖️ в тайтлах волновых сессий аналитика и ревью-аналитики
    let state = Arc::new(MemState(Mutex::default()));
    let sessions = Arc::new(FakeSessions::new());
    let (u, _) = analyst_uc(
        vec![titled(1, "заявка", Status::Analys)],
        state.clone(),
        sessions.clone(),
    );
    assert_eq!(u.tick().await.unwrap(), 1);
    let (r, _) = review_uc(
        vec![titled(2, "заявка", Status::ReviewAnalys)],
        state,
        sessions.clone(),
    );
    assert_eq!(r.tick().await.unwrap(), 1);
    let specs = sessions.specs.lock().unwrap();
    let (a, rv) = (
        specs
            .iter()
            .find(|s| matches!(s.agent, AgentSlot::Role(Agent::Analyst)))
            .expect("волна"),
        specs
            .iter()
            .find(|s| matches!(s.agent, AgentSlot::Role(Agent::AnalystReviewer)))
            .expect("ревью-волна"),
    );
    assert_eq!(
        a.title,
        format!("{} аналитика волны", Agent::Analyst.emoji())
    );
    assert_eq!(
        rv.title,
        format!("{} ревью аналитики", Agent::AnalystReviewer.emoji())
    );
}

#[tokio::test]
async fn dreamer_title_carries_marker() {
    // 🌙 в тайтле дример-сессии пост-обработки (кандидат: финал + токены + idle)
    use std::sync::atomic::{AtomicU64, Ordering};
    struct FarClock(AtomicU64);
    impl crate::ports::Clock for FarClock {
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
    let sessions = Arc::new(FakeSessions::with_finished("dev", "готово"));
    let u = crate::pipeline::dreamer::DreamerUseCase {
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        state: Arc::new(MemState(Mutex::default())),
        clock: Arc::new(FarClock(AtomicU64::new(2_000_000))), // > idle-порога 30 мин
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        checkout: "/tmp/checkout".into(),
        cup: 7,
    };
    assert_eq!(u.tick().await.unwrap(), 1);
    let specs = sessions.specs.lock().unwrap();
    let spec = specs.last().expect("дример-сессия");
    assert_eq!(
        spec.title,
        format!("{} дример ses_live_dev", Agent::Dreamer.emoji())
    );
    assert!(matches!(spec.agent, AgentSlot::Role(Agent::Dreamer)));
}

// ===== Гард «одна живая сессия на строку» + зачистка спавн-путей (#1163) =====

/// Часы с большим now_ms — сценарий протухшего следа (зомби-страховка).
struct LateClock;
impl Clock for LateClock {
    fn utc_minute(&self) -> UtcMinute {
        UtcMinute {
            minute_of_day: 100,
            weekday: 3,
        }
    }
    fn now_ms(&self) -> u64 {
        20_000_000
    }
}

fn tick_with_clock(
    rows: Vec<Task>,
    sessions: FakeSessions,
    state: Arc<MemState>,
    clock: Arc<dyn Clock>,
) -> (TickUseCase, Arc<FakeBoard>, Arc<FakeSessions>, Arc<FakePrs>) {
    let board = Arc::new(FakeBoard {
        rows: rows.into(),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(sessions);
    let prs = Arc::new(FakePrs {
        state: PrState::Missing,
        merges: Mutex::default(),
        probes: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: prs.clone(),
        clock,
        state,
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    (u, board, sessions, prs)
}

#[tokio::test]
async fn spawn_held_when_live_session_in_env_dir() {
    // сирота спавна/клейм в гонке с тиком: дев-сессия живёт в env строки
    // при пустой ячейке — второй источник держит дублирующий спавн (#1150)
    let sessions = FakeSessions::with_env_session("ses_orphan", "/tmp/env/50", "", "", 2000);
    let (u, board, sessions, _) = usecase_with_sessions(
        vec![task(50, Status::InProgress)],
        PrState::Missing,
        sessions,
        Arc::new(MemState(Mutex::default())),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "спавн поверх живой сессии env не состоялся"
    );
    assert_eq!(out.held_live, 1, "гард держит и считает hold");
    assert_eq!(out.spawns_done, 0);
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("session#50")),
        "ячейка не перезаписана"
    );
}

#[tokio::test]
async fn spawn_allowed_when_env_session_finished() {
    // финал хода (outcome succeeded) — спавн нового раунда не держится
    let sessions = FakeSessions::with_env_session("ses_done", "/tmp/env/51", "", "succeeded", 2000);
    let (u, _, sessions, _) = usecase_with_sessions(
        vec![task(51, Status::InProgress)],
        PrState::Missing,
        sessions,
        Arc::new(MemState(Mutex::default())),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 1, "завершённая сессия спавну не мешает");
    assert_eq!(out.held_live, 0);
    assert_eq!(sessions.spawned.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn reviewer_env_session_does_not_hold_dev_spawn() {
    // ревьювер, досматривающий вердикт прошлого раунда (та же env-
    // директория), спавн дева не держит — семьи разные
    let sessions =
        FakeSessions::with_env_session("ses_rev", "/tmp/env/52", "code-quality-reviewer", "", 2000);
    let (u, _, _, _) = usecase_with_sessions(
        vec![task(52, Status::InProgress)],
        PrState::Missing,
        sessions,
        Arc::new(MemState(Mutex::default())),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 1, "чужая семья не держит дев-спавн");
    assert_eq!(out.held_live, 0);
}

#[tokio::test]
async fn stale_env_session_does_not_hold_spawn() {
    // зомби-страховка: ход без финала и след протух за окно — спавн идёт
    // (луп-гвард каппирует повтор, строка не зависает навсегда)
    let sessions = FakeSessions::with_env_session(
        "ses_zombie",
        "/tmp/env/53",
        "",
        "",
        1_000_000, // now 20_000_000 — след старше окна 2ч
    );
    let (u, _, sessions, _) = tick_with_clock(
        vec![task(53, Status::InProgress)],
        sessions,
        Arc::new(MemState(Mutex::default())),
        Arc::new(LateClock),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 1, "протухший след спавн не держит");
    assert_eq!(out.held_live, 0);
    assert_eq!(sessions.spawned.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn dead_respawn_cancelled_when_serve_lists_session() {
    // проба лжёт (404 живому ходу, serve-шторм 13.11): сессия числится
    // в списке исполнителя — респавн отменён (#1072 → #1163)
    let mut t = task(54, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_lie_54").unwrap(),
    );
    t.apply_fields(f);
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((54, Status::InProgress));
    let sessions = FakeSessions::with_env_session("ses_lie_54", "/tmp/env/54", "", "", 2000);
    let (u, _, sessions, _) = usecase_with_sessions(vec![t], PrState::Missing, sessions, state);
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_lie_54".into());
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "респавн живой (по second source) сессии не состоялся"
    );
    assert_eq!(out.wakeups_deferred, 0);
    assert_eq!(out.spawns_done, 0);
}

#[tokio::test]
async fn respawn_not_held_by_cooldown_attention_speed() {
    // кулдаун-ЧЕК на респавн не вешается (#1163): он замедлял бы
    // attention-механику (луп-гвард эскалирует час вместо ~15 мин).
    // Свежий mark_spawn respawn не держит — спавн идёт, mark обновляется
    // (единый учёт темпа слота: plain-путь держит по нему спавн).
    let mut t = task(55, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_55").unwrap(),
    );
    t.apply_fields(f);
    let inner = MemStateInner {
        spawn_ms: vec![(55, Slot::Dev, 2000)], // только что спавнились
        ..Default::default()
    };
    let state = Arc::new(MemState(Mutex::new(inner)));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((55, Status::InProgress));
    let (u, _, sessions, _) = usecase_with_sessions(
        vec![t],
        PrState::Missing,
        FakeSessions::new(),
        state.clone(),
    );
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_55".into());
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.spawns_done, 1,
        "респавн мёртвой сессии не замедлен кулдауном"
    );
    assert_eq!(out.wakeups_deferred, 0);
    let marks = state.0.lock().unwrap().spawn_ms.clone();
    assert!(
        marks.iter().any(|(n, s, _)| *n == 55 && *s == Slot::Dev),
        "респавн заряжает mark_spawn: {marks:?}"
    );
}

#[tokio::test]
async fn enrichment_failure_skips_row() {
    // отказ enrichment (#1163): строка пропущена тиком — решение по
    // невидимым ячейкам не принимается, дубль-спавн не случается
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![task(57, Status::InProgress)]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: true,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(FakeSessions::new());
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.enrich_skipped, 1, "строка пропущена, не решена");
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "спавна по невидимым ячейкам нет"
    );
    assert!(
        board.log.lock().unwrap().is_empty(),
        "доска не мутируется по неполным данным"
    );
}

#[tokio::test]
async fn orphan_deleted_when_set_session_fails() {
    // спавн состоялся, запись ячейки упала: сеанс гасится — иначе живой
    // сеанс вне ячейки вечно держит гард, а будить его некому (#1163)
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![task(58, Status::InProgress)]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: true,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(FakeSessions::new());
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.actions_failed, 1, "отказ записи ячейки виден");
    assert_eq!(out.spawns_done, 0);
    let deleted = sessions.deleted.lock().unwrap();
    assert!(
        deleted.len() >= 2,
        "гасятся и work-сирота, и bootstrap: {deleted:?}"
    );
}

#[tokio::test]
async fn orphan_deleted_when_prompt_fails() {
    // промпт не дошёл: сессия без промпта — мусор, гасится симметрично
    // bootstrap-зачистке (#1163)
    let sessions = FakeSessions {
        prompt_fails: true,
        prompt_lost: Mutex::default(),
        prompt_lost_all: false,
        ..FakeSessions::new()
    };
    let (u, _, sessions, _) = usecase_with_sessions(
        vec![task(59, Status::InProgress)],
        PrState::Missing,
        sessions,
        Arc::new(MemState(Mutex::default())),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.actions_failed, 1);
    assert_eq!(out.spawns_done, 0);
    let deleted = sessions.deleted.lock().unwrap();
    assert!(
        deleted.iter().any(|d| d.starts_with("ses_")),
        "work-сессия без промпта удалена: {deleted:?}"
    );
}

#[tokio::test]
async fn dead_respawn_held_when_orphan_in_env() {
    // ревью Common-2: сеанс ячейки мёртв по-настоящему (вне списка), но
    // в env строки живёт сирота из crash-окна — респавн держится env-гардом
    let mut t = task(60, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_60").unwrap(),
    );
    t.apply_fields(f);
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((60, Status::InProgress));
    let sessions = FakeSessions::with_env_session("ses_orphan_60", "/tmp/env/60", "", "", 2000);
    let (u, _, sessions, _) = usecase_with_sessions(vec![t], PrState::Missing, sessions, state);
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_60".into());
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "респавн поверх живой сироты env не состоялся"
    );
    assert_eq!(
        out.held_live, 1,
        "env-гард держит и считает hold на респавне"
    );
    assert_eq!(out.wakeups_deferred, 0);
}

#[tokio::test]
async fn strict_scan_down_plain_spawn_held() {
    // инверсия деградации (#1263): скан исполнителя недоступен — спавн
    // без зрения невозможен (слепой спавн поверх живой сестры = инцидент
    // #1241); hold до восстановления списка, луп-гвард не заряжается
    let mut sessions = FakeSessions::new();
    sessions.list_fails = true;
    let (u, _, _, _) = usecase_with_sessions(
        vec![task(61, Status::InProgress)],
        PrState::Missing,
        sessions,
        Arc::new(MemState(Mutex::default())),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 0, "слепой спавн невозможен — hold (#1263)");
    assert_eq!(out.held_blind, 1);
    assert_eq!(out.held_live, 0);
}

#[tokio::test]
async fn strict_scan_down_dead_respawn_held() {
    // инверсия деградации на respawn-пути (#1263): скан недоступен —
    // second source и env-гард не оцениваются вовсе, респавн держится
    // слепотой до восстановления скана (probe-лжец без списка не решает)
    let mut t = task(62, Status::InProgress);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Dev,
        uwuwu_domain::SessionRef::parse("ses_dead_62").unwrap(),
    );
    t.apply_fields(f);
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((62, Status::InProgress));
    let mut sessions = FakeSessions::new();
    sessions.list_fails = true;
    let (u, _, sessions, _) = usecase_with_sessions(vec![t], PrState::Missing, sessions, state);
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_62".into());
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.spawns_done, 0,
        "слепой респавн невозможен — hold (#1263)"
    );
    assert_eq!(out.held_blind, 1);
    assert_eq!(out.held_live, 0);
}

#[tokio::test]
async fn blind_scan_test_respawn_over_live_sister_holds() {
    // #1263 (красный на pre-fix): список исполнителя недоступен, probe
    // лжёт Dead, в env строки жива сестра (tester) — респавн поверх неё
    // = класс инцидента #1241 (пять сессий в одном env). Инвариант:
    // спавн преемника при живой сестре невозможен — hold без спавна.
    let mut t = task(63, Status::Testing);
    let mut f = TaskFields::default();
    f.set_session(
        Slot::Test,
        uwuwu_domain::SessionRef::parse("ses_dead_63").unwrap(),
    );
    t.apply_fields(f);
    let state = Arc::new(MemState(Mutex::default()));
    state.0.lock().unwrap().statuses.push((63, Status::Testing));
    let mut sessions =
        FakeSessions::with_env_session("ses_sister_63", "/tmp/env/63", "tester", "", 2000);
    sessions.list_fails = true;
    let (u, _, sessions, _) = usecase_with_sessions(vec![t], PrState::Missing, sessions, state);
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_63".into());
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "респавн тест-слота поверх живой сестры при слепом скане невозможен (#1263)"
    );
    assert_eq!(out.held_blind, 1, "слепой hold считается отдельно");
}

#[tokio::test]
async fn blind_scan_plain_spawn_over_live_sister_holds() {
    // #1263 (красный на pre-fix): список недоступен, в env строки живёт
    // дев-сестра — плейн-спавн поверх неё идёт вслепую. Инвариант тот же:
    // hold без спавна.
    let mut sessions = FakeSessions::with_env_session("ses_sister_64", "/tmp/env/64", "", "", 2000);
    sessions.list_fails = true;
    let (u, _, sessions, _) = usecase_with_sessions(
        vec![task(64, Status::InProgress)],
        PrState::Missing,
        sessions,
        Arc::new(MemState(Mutex::default())),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "плейн-спавн поверх живой сестры при слепом скане невозможен (#1263)"
    );
    assert_eq!(out.held_blind, 1);
}

#[tokio::test]
async fn blind_scan_respawn_holds_for_every_stage_slot() {
    // матрица слотов (#1263, ревью Low): инвариант «спавн преемника при
    // живой сестре невозможен» слот-генеричен — Design/Dev (деф-семья),
    // план/результат-ревью и тест (именованные); слепой респавн держится
    // на каждом статусном пути
    let cases = [
        (Status::Design, Slot::Design, ""),
        (Status::InProgress, Slot::Dev, ""),
        (Status::ReviewPlan, Slot::PlanReview, "plan-reviewer"),
        (
            Status::ReviewResult,
            Slot::ResultReview,
            "code-quality-reviewer",
        ),
        (Status::Testing, Slot::Test, "tester"),
    ];
    for (i, (status, slot, agent)) in cases.iter().enumerate() {
        let n = 70 + i as u64;
        let mut t = task(n, *status);
        let mut f = TaskFields::default();
        f.set_session(
            *slot,
            uwuwu_domain::SessionRef::parse(&format!("ses_dead_{n}")).unwrap(),
        );
        t.apply_fields(f);
        let state = Arc::new(MemState(Mutex::default()));
        state.0.lock().unwrap().statuses.push((n, *status));
        let mut sessions = FakeSessions::with_env_session(
            &format!("ses_sister_{n}"),
            &format!("/tmp/env/{n}"),
            agent,
            "",
            2000,
        );
        sessions.list_fails = true;
        let (u, _, sessions, _) = usecase_with_sessions(vec![t], PrState::Missing, sessions, state);
        sessions
            .dead_sessions
            .lock()
            .unwrap()
            .push(format!("ses_dead_{n}"));
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        assert!(
            sessions.spawned.lock().unwrap().is_empty(),
            "слот {slot:?}: слепой респавн поверх живой сестры невозможен (#1263)"
        );
        assert_eq!(out.held_blind, 1, "слот {slot:?}: слепой hold посчитан");
    }
}

#[tokio::test]
async fn blind_hold_charges_no_state_recovery_respawn_passes() {
    // Пин тестировщика #1263 (клейм плана фикса: «луп-гвард и кулдаун на
    // hold не заряжаются — персистенция только при реальном спавне»).
    // Три слепых тика не пишут ни петлю, ни кулдаун; тик возврата
    // исполнителя респавнит строку без эскалации в Needs attention.
    // e2e-фаза 6 доказывает возврат только на plain-пути, этот пин
    // закрывает respawn-путь.
    let mk = || {
        let mut t = task(79, Status::Testing);
        let mut f = TaskFields::default();
        f.set_session(
            Slot::Test,
            uwuwu_domain::SessionRef::parse("ses_dead_79").unwrap(),
        );
        t.apply_fields(f);
        t
    };
    let state = Arc::new(MemState(Mutex::default()));
    state.0.lock().unwrap().statuses.push((79, Status::Testing));

    // Фаза слепоты: список-отказ — hold на каждом тике
    let mut blind = FakeSessions::new();
    blind.list_fails = true;
    let (u, _, blind, _) =
        usecase_with_sessions(vec![mk()], PrState::Missing, blind, state.clone());
    blind
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_79".into());
    for i in 0..3 {
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        assert_eq!(out.held_blind, 1, "тик {i}: слепой hold");
        assert_eq!(out.spawns_done, 0, "тик {i}: слепой спавн невозможен");
    }
    {
        let st = state.0.lock().unwrap();
        assert!(
            st.loops.iter().all(|(n, _, _, _)| *n != 79),
            "слепые hold'ы не заряжают луп-гвард"
        );
        assert!(
            st.spawn_ms.iter().all(|(n, _, _)| *n != 79),
            "слепые hold'ы не заряжают кулдаун"
        );
    }

    // Тик возврата: список жив, мёртвая сессия вне списка (second source
    // молчит), сестры нет — респавн проходит, эскалации нет
    let healthy = FakeSessions::new();
    let (u2, _, healthy, _) =
        usecase_with_sessions(vec![mk()], PrState::Missing, healthy, state.clone());
    healthy
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dead_79".into());
    let out = u2.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        !healthy.spawned.lock().unwrap().is_empty(),
        "после слепых hold'ов респавн на возврате исполнителя проходит (#1263)"
    );
    assert_eq!(out.attention_flagged, 0, "hold'ы не эскалировали петлю");
    assert_eq!(out.held_blind, 0);
}

#[test]
fn env_repo_dir_formula_is_pinned() {
    // ревью Low: формула env-директории одна — спавн и гард строят путь
    // одной функцией; формат пинится тестом (расхождение = тихая слепота)
    assert_eq!(board_plan().env_repo_dir(50), "/tmp/env/50");
    assert_eq!(board_plan().env_repo_dir(1), "/tmp/env/1");
}

/// Регресс-пин #1281 (тестировщик): сброс раунда из-под рестарта должен
/// давать НАСТОЯЩИЙ свежий раунд — до двух пингов текущего процесса до
/// любой эскалации, и подпись эскалации считает доставленные пинги
/// текущего процесса. Сценарий: persisted count=2 (до рестарта) → тик 1:
/// round_stale + пинг; +30 мин тик 2: второй пинг (не эскалация);
/// +30 мин тик 3: эскалация с «автопингов в раунде: 2».
#[tokio::test]
async fn round_stale_reset_gives_true_fresh_round_two_pings_before_escalation() {
    const BOOT: u64 = 100_000_000;
    struct SettableClock(Arc<Mutex<u64>>);
    impl Clock for SettableClock {
        fn utc_minute(&self) -> UtcMinute {
            UtcMinute {
                minute_of_day: 100,
                weekday: 3,
            }
        }
        fn now_ms(&self) -> u64 {
            *self.0.lock().unwrap()
        }
    }
    let now = Arc::new(Mutex::new(BOOT));
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![dev_task(64, "ses_dev_64")]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sess = Arc::new(FakeSessions {
        // сессия молчит весь сценарий (25 мин до первого тика, дальше — тишина)
        activity_ms: Some(BOOT - 25 * 60_000),
        idle_reply: true,
        ..Default::default()
    });
    // persisted стейт из-под рестарта: два пинга ДО старта процесса
    let seeded = MemState(Mutex::new(MemStateInner {
        stalls: vec![
            (64, Status::InProgress, BOOT - 40 * 60_000),
            (64, Status::InProgress, BOOT - 35 * 60_000),
        ],
        ..Default::default()
    }));
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sess.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(SettableClock(now.clone())),
        state: Arc::new(seeded),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: BOOT,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    // тик 1 (сразу после рестарта): раунд чужой — сброс + свежий пинг
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.stall_flagged, 0, "тик 1 не эскалирует");
    assert_eq!(out.soft_pings, 1, "тик 1: свежий пинг раунда");
    // тик 2 (+30 мин): раунд уже текущего процесса — второй пинг, не эскалация
    *now.lock().unwrap() = BOOT + 30 * 60_000;
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.stall_flagged, 0,
        "тик 2: в процессе доставлен только один пинг — эскалации нет"
    );
    assert_eq!(out.soft_pings, 1, "тик 2: второй пинг свежего раунда");
    // тик 3 (+30 мин): два пинга доставлены, тишина продолжается — эскалация
    *now.lock().unwrap() = BOOT + 60 * 60_000;
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.stall_flagged, 1,
        "тик 3: два доставленных пинга исчерпаны — эскалация"
    );
    let log = board.log.lock().unwrap();
    let comment = log
        .iter()
        .find(|l| l.starts_with("comment#64="))
        .expect("эскалация постит коммент");
    assert!(
        comment.contains("auto-pings in the round: 2"),
        "подпись считает пинги ТЕКУЩЕГО процесса (2), а не persisted-мусор: {comment}",
    );
}
#[tokio::test]
async fn stage_spawn_carries_model_from_server_map() {
    // #1154 (решение владельца 17.11): модель спавна — карта сервера
    // (SpawnModels), каждый спавн несёт её в теле сессии; деф-файлы
    // агентов моделей не несут. Дев-семья с #1300 — developer-ключ,
    // bootstrap — default-ключ (канон 18.11 #417: оба flash#high).
    // Arrange
    let (u, _, sessions, _) = usecase(vec![task(1, Status::InProgress)], PrState::Missing);

    // Act
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();

    // Assert
    assert_eq!(out.spawns_done, 1, "спавн стадии состоялся");
    let specs = sessions.specs.lock().unwrap();
    assert_eq!(specs.len(), 2, "bootstrap + work");
    let want = uwuwu_domain::SpawnModel::parse("zai-coding-plan/glm-5.3-flash#high").unwrap();
    for spec in specs.iter() {
        assert_eq!(spec.model, want.model, "модель спавна = default карты");
        assert_eq!(spec.variant, want.variant, "default — вариант карты");
    }
}

#[tokio::test]
async fn review_and_test_spawns_carry_map_models_per_agent() {
    // Low-пин ревью R5: for_agent-ветка spawn_stage_inner (PlanReview/
    // ResultReview/Test) — модель каждой роли из карты, не дефолт.
    // Arrange: по строке на стадию без сессии слота
    let cases = [
        (
            Status::ReviewPlan,
            "plan-reviewer",
            "zai-coding-plan/glm-5.4#high",
        ),
        (
            Status::ReviewResult,
            "code-quality-reviewer",
            "zai-coding-plan/glm-5.4#high",
        ),
        (
            Status::Testing,
            "tester",
            "zai-coding-plan/glm-5.3-flash#high",
        ),
    ];
    for (status, agent_name, raw) in cases {
        let (u, _, sessions, _) = usecase(vec![task(5, status)], PrState::Missing);
        let want = uwuwu_domain::SpawnModel::parse(raw).unwrap();

        // Act
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();

        // Assert
        assert_eq!(out.spawns_done, 1, "{agent_name}: спавн состоялся");
        let specs = sessions.specs.lock().unwrap();
        let work = specs
            .iter()
            .find(|s| !s.bootstrap)
            .expect("рабочая сессия в спеках");
        assert_eq!(work.model, want.model, "{agent_name}: модель из карты");
        assert_eq!(work.variant, want.variant, "{agent_name}: вариант из карты");
    }
}

// ---- преемпшен CUP: понижение приоритета отдаёт слот (#1259) ----

/// Активная строка с привязанной dev-сессией (busy-актив при
/// idle_reply=false).
fn row_with_session(n: u64, session: &str, priority: Priority) -> Task {
    let mut t = task(n, Status::InProgress);
    t.set_priority(priority);
    let mut f = TaskFields::default();
    f.set_session(Slot::Dev, uwuwu_domain::SessionRef::parse(session).unwrap());
    t.apply_fields(f);
    t
}

/// Живая busy-сессия исполнителя (union-скан #1231: outcome пуст, ход в
/// полёте — busy держит кап).
fn live_busy(sessions: &Arc<FakeSessions>, id: &str) {
    sessions.live.lock().unwrap().push(LiveSighting {
        id: id.into(),
        agent: String::new(),
        directory: String::new(),
        outcome: String::new(),
        updated_ms: 0,
        total_tokens: 0,
    });
    sessions.busy_ids.lock().unwrap().push(id.into());
}

#[tokio::test]
async fn cup_preempt_yields_slot_on_priority_drop() {
    // Канонический BDD: кап из busy low-активов, high в очереди → тик
    // гасит жертву (abort+delete), слот уходит high-кандидату
    let v1 = row_with_session(1, "ses_v1", Priority::Low);
    let v2 = row_with_session(2, "ses_v2", Priority::Low);
    let mut waiter = task(3, Status::InProgress);
    waiter.set_priority(Priority::High);
    let (u, board, sessions, _) = usecase(vec![v1, v2, waiter], PrState::Missing);
    live_busy(&sessions, "ses_v1");
    live_busy(&sessions, "ses_v2");
    let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out.preempts, 1, "одна жертва на отдачу слота");
    assert_eq!(out.spawns_done, 1, "high-кандидат получил слот");
    assert_eq!(out.held_cup, 0);
    let deleted = sessions.deleted.lock().unwrap();
    let victims: Vec<_> = deleted
        .iter()
        .filter(|d| d.starts_with("ses_v"))
        .cloned()
        .collect();
    assert_eq!(victims, ["ses_v1"], "гасится ровно одна low-жертва");
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("status#")),
        "жертве статус не трогается (возобновление — plain-спавном)"
    );
}

#[tokio::test]
async fn cup_no_preempt_of_medium_active() {
    // Гард: high/medium-активы не гасятся никогда — high-кандидат ждёт
    let v = row_with_session(1, "ses_v1", Priority::Medium);
    let mut waiter = task(2, Status::InProgress);
    waiter.set_priority(Priority::High);
    let (u, _, sessions, _) = usecase(vec![v, waiter], PrState::Missing);
    live_busy(&sessions, "ses_v1");
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.preempts, 0);
    assert_eq!(out.held_cup, 1, "medium-актив держит слот");
    assert_eq!(out.spawns_done, 0);
    assert!(sessions.deleted.lock().unwrap().is_empty());
    assert!(sessions.aborted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cup_preempt_skipped_when_budget_exhausted() {
    // Контракт порядка (круг 1): бюджетная проба ДО гашения — жертва
    // зря не гибнет, кандидат остаётся held до свободного бюджета
    let v = row_with_session(1, "ses_v1", Priority::Low);
    let mut waiter = task(2, Status::InProgress);
    waiter.set_priority(Priority::High);
    let (u, _, sessions, _) = usecase(vec![v, waiter], PrState::Missing);
    live_busy(&sessions, "ses_v1");
    sessions.spawn_probe.store(false, Ordering::SeqCst);
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.preempts, 0, "без готовности кандидата гашения нет");
    assert_eq!(out.held_cup, 1);
    assert!(sessions.deleted.lock().unwrap().is_empty());
    assert!(sessions.aborted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cup_preempt_abort_failure_holds_spawn() {
    // abort/delete не прошли — кандидат остаётся held, ретрай следующим
    // тиком; жертва не удалена (abort-first порядок)
    let v = row_with_session(1, "ses_v1", Priority::Low);
    let mut waiter = task(2, Status::InProgress);
    waiter.set_priority(Priority::High);
    let (u, _, sessions, _) = usecase(vec![v, waiter], PrState::Missing);
    live_busy(&sessions, "ses_v1");
    sessions.abort_fails.store(true, Ordering::SeqCst);
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.preempts, 0);
    assert_eq!(out.held_cup, 1);
    assert_eq!(out.actions_failed, 1, "отказ гашения — громкий");
    assert!(
        sessions.deleted.lock().unwrap().is_empty(),
        "abort не прошёл — delete не вызывается"
    );
}

#[tokio::test]
async fn cup_preempt_picks_freshest_lowest() {
    // Детерминизм: среди low-жертв — минимальный ранг, при равенстве
    // свежее заспавненная (контекст младше — дешевле потерять)
    let v1 = row_with_session(1, "ses_v1", Priority::Low);
    let v2 = row_with_session(2, "ses_v2", Priority::Low);
    let mut waiter = task(3, Status::InProgress);
    waiter.set_priority(Priority::High);
    let state = Arc::new(MemState(Mutex::default()));
    state.0.lock().unwrap().spawn_ms = vec![(1, Slot::Dev, 5000), (2, Slot::Dev, 1000)];
    let (u, _, sessions, _) = usecase_with_sessions(
        vec![v1, v2, waiter],
        PrState::Missing,
        FakeSessions::new(),
        state,
    );
    live_busy(&sessions, "ses_v1");
    live_busy(&sessions, "ses_v2");
    let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out.preempts, 1);
    let deleted = sessions.deleted.lock().unwrap();
    let victims: Vec<_> = deleted
        .iter()
        .filter(|d| d.starts_with("ses_v"))
        .cloned()
        .collect();
    assert_eq!(
        victims,
        ["ses_v1"],
        "свежее заспавненная жертва гасится первой"
    );
}

#[tokio::test]
async fn cup_nonrow_sessions_never_aborted() {
    // Структурный гард: не-строки (боты/кроны/ручные) вне множества
    // жертв — преемпшен трогает только сессии строк досок
    let v = row_with_session(1, "ses_v1", Priority::Low);
    let mut waiter = task(2, Status::InProgress);
    waiter.set_priority(Priority::High);
    let (u, _, sessions, _) = usecase(vec![v, waiter], PrState::Missing);
    live_busy(&sessions, "ses_v1");
    live_busy(&sessions, "ses_bot_busy");
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.preempts, 1);
    let deleted = sessions.deleted.lock().unwrap();
    let victims: Vec<_> = deleted
        .iter()
        .filter(|d| d.starts_with("ses_v"))
        .cloned()
        .collect();
    assert_eq!(victims, ["ses_v1"], "жертва — строка доски");
    assert!(sessions.aborted.lock().unwrap().as_slice() == ["ses_v1"]);
    assert!(!deleted.contains(&"ses_bot_busy".to_string()));
}

#[tokio::test]
async fn cup_preempted_victim_defers_while_cap_full() {
    // Пин resume-пути: погашенная жертва (dead-сессия) не респавнится,
    // пока кап полон — respawn Deferred, строка ждёт свободного слота
    // cap=1: тик 1 — кап держат v1+v2 (busy), high-кандидат получает
    // жертву, но слот по-прежнему занят (busy остался ≥ cap) → без спавна
    let v1 = row_with_session(1, "ses_v1", Priority::Low);
    let v2 = row_with_session(2, "ses_v2", Priority::Low);
    let mut waiter = task(3, Status::InProgress);
    waiter.set_priority(Priority::High);
    let (u, _, sessions, _) = usecase(vec![v1, v2, waiter], PrState::Missing);
    live_busy(&sessions, "ses_v1");
    live_busy(&sessions, "ses_v2");
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.preempts, 1, "жертва отдана кандидату");
    assert_eq!(out.spawns_done, 0, "слот всё ещё занят вторым busy-активом");
    // тик 2: v1 погашена (вне списка и 404 на пробе); кап держит v2 →
    // кандидат гасит v2 и занимает слот; v1 (dead) — Deferred: кап полон
    sessions.live.lock().unwrap().retain(|s| s.id != "ses_v1");
    sessions.dead_sessions.lock().unwrap().push("ses_v1".into());
    let out2 = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out2.preempts, 1, "вторая жертва — под спавн кандидата");
    assert_eq!(out2.spawns_done, 1, "кандидат занял отнятый слот");
    assert_eq!(
        out2.wakeups_deferred, 1,
        "жертва без слота ждёт — Deferred, не петля"
    );
}

#[tokio::test]
async fn cup_preempted_victim_respawns_when_slot_frees() {
    // Пин resume-пути: слот освободился (прочие активы ушли с исполнителя)
    // — жертва возобновляется штатным respawn'ом в тот же task-env
    let v1 = row_with_session(1, "ses_v1", Priority::Low);
    let v2 = row_with_session(2, "ses_v2", Priority::Low);
    let mut waiter = task(3, Status::InProgress);
    waiter.set_priority(Priority::High);
    let (u, board, sessions, _) = usecase(vec![v1, v2, waiter], PrState::Missing);
    live_busy(&sessions, "ses_v1");
    live_busy(&sessions, "ses_v2");
    let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out.preempts, 1);
    // слоты свободны: обе старые сессии вне списка исполнителя (v1 —
    // гашение, v2 — завершилась между тиками), busy=0
    sessions
        .live
        .lock()
        .unwrap()
        .retain(|s| s.id != "ses_v1" && s.id != "ses_v2");
    {
        let mut dead = sessions.dead_sessions.lock().unwrap();
        dead.push("ses_v1".into());
        dead.push("ses_v2".into());
    }
    let out2 = u.run(&plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(
        out2.spawns_done, 2,
        "жертвы возобновились respawn'ом при свободных слотах"
    );
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("session#1=")),
        "ячейка жертвы перезаписана новой сессией"
    );
}

#[tokio::test]
async fn cup_preempt_empty_victim_by_medium_waiter() {
    // Empty-приоритет ниже low в очереди — жертва для medium-кандидата
    let v = row_with_session(1, "ses_v1", Priority::Empty);
    let mut waiter = task(2, Status::InProgress);
    waiter.set_priority(Priority::Medium);
    let (u, _, sessions, _) = usecase(vec![v, waiter], PrState::Missing);
    live_busy(&sessions, "ses_v1");
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.preempts, 1, "Empty-актив гасится medium-кандидатом");
    assert_eq!(out.spawns_done, 1);
    let victims: Vec<_> = sessions
        .deleted
        .lock()
        .unwrap()
        .iter()
        .filter(|d| d.starts_with("ses_v"))
        .cloned()
        .collect();
    assert_eq!(victims, ["ses_v1"]);
}

// ---- преемпшен × луп-гвард: гашение не смерть (#1412) ----

/// Полная метка busy-хвоста исполнителя (union-скан тика).
fn busy_exactly(sessions: &FakeSessions, ids: &[String]) {
    *sessions.busy_ids.lock().unwrap() = ids.to_vec();
}

/// Погасить сессию в db-смысле: после delete исполнителя serve 404-ит
/// GET сессии (liveness Dead), сессия не в списке (second source чист).
fn mark_dead(sessions: &FakeSessions, id: &str) {
    sessions.dead_sessions.lock().unwrap().push(id.into());
}

/// Ячейка жертвы по доске (Mutex rows — фидельность hub).
fn victim_cell(board: &FakeBoard) -> Option<String> {
    let rows = board.rows.lock().unwrap();
    rows.iter()
        .find(|t| t.number() == 1)
        .and_then(|t| t.session_of(Slot::Dev))
        .map(|s| s.to_string())
}

/// Доска между тиками: гашение сняло ячейку, спавн вернул (как hub).
fn set_row_cell(board: &FakeBoard, n: u64, slot: Slot, s: Option<&str>) {
    let mut rows = board.rows.lock().unwrap();
    if let Some(t) = rows.iter_mut().find(|t| t.number() == n) {
        let mut f = TaskFields::default();
        if let Some(s) = s {
            f.set_session(slot, uwuwu_domain::SessionRef::parse(s).unwrap());
        } else {
            f.clear_session(slot);
        }
        t.apply_fields(f);
    }
}

fn set_victim_cell(board: &FakeBoard, s: Option<&str>) {
    set_row_cell(board, 1, Slot::Dev, s);
}

/// Движущиеся часы (#1412): now_ms растёт с каждым тиком (шаг 20 мин —
/// больше кулдауна спавна), utc_minute фиксирован вне пик-окна — кулдаун
/// не стопорит возвраты жертвы, пик-окно не вмешивается.
struct AdvancingClock(std::sync::Mutex<u64>);
impl crate::ports::Clock for AdvancingClock {
    fn utc_minute(&self) -> UtcMinute {
        UtcMinute {
            minute_of_day: 100,
            weekday: 3,
        }
    }
    fn now_ms(&self) -> u64 {
        *self.0.lock().unwrap()
    }
}

#[tokio::test]
async fn cup_preempt_wave_does_not_escalate_victim_loop() {
    // #1412, класс: волна medium-кандидатов × low-жертва — каждое
    // гашение сбрасывает луп-гвард и кулдаун слота жертвы, ячейка
    // снимается: строка возвращается в очередь plain-спавном. Пять
    // циклов «гашение → возврат жертвы → гашение» — ни одной эскалации.
    // До фикса пятый возврат жертвы эскалирует «Респавн-петля» (гашение
    // не отличалось от смерти, счётчик рос без прогресса статуса).
    let victim = row_with_session(1, "ses_a", Priority::Low);
    let mut rows = vec![victim];
    rows.push(row_with_session(7, "ses_x", Priority::Medium)); // busy-фон
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(rows),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(FakeSessions::new());
    let clock = Arc::new(AdvancingClock(std::sync::Mutex::new(2000)));
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: clock.clone(),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    live_busy(&sessions, "ses_a");
    live_busy(&sessions, "ses_x");

    // Кандидаты приходят волнами (как ночные волны конвейера 19.11):
    // тик волны — кап полон (фон + рабочая жертва), кандидат held гасит
    // жертву под свой спавн; тик затишья — ход кандидата доведён (сессия
    // ушла из busy), слот свободен, жертва возвращается. Часы движутся
    // на 20 мин/тик (кулдаун 15 мин не стопорит возвраты).
    fn advance_tick(clock: &AdvancingClock) {
        *clock.0.lock().unwrap() += 20 * 60_000;
    }
    let mut preempts_total = 0;
    for wave in 0..5 {
        // волна: новый кандидат при полном капе — гашение жертвы
        board.rows.lock().unwrap().push({
            let mut t = task(2 + wave, Status::InProgress);
            t.set_priority(Priority::Medium);
            t
        });
        let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
        assert_eq!(
            out.preempts,
            1,
            "волна {}: жертва гасится кандидатом",
            wave + 1
        );
        preempts_total += out.preempts;
        let victim_session = victim_cell(&board).expect("ячейка жертвы от спавна");
        // доска: кандидат получил свою ячейку (последний спавн исполнителя)
        let cand_session = sessions
            .spawned
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("спавн кандидата в тике волны");
        set_row_cell(&board, 2 + wave, Slot::Dev, Some(&cand_session));
        busy_exactly(&sessions, &["ses_x".to_string()]); // ход кандидата доведён
        mark_dead(&sessions, &victim_session);
        sessions
            .live
            .lock()
            .unwrap()
            .retain(|s| s.id != victim_session && s.id != "ses_a");
        // доска: гашение сняло ячейку жертвы (set_victim_cell(None))
        set_victim_cell(&board, None);
        advance_tick(&clock);

        // затишье: слот свободен — жертва возвращается в работу
        let out = u.run(&plan(Mode::Live, 2)).await.unwrap();
        assert_eq!(
            out.spawns_done,
            1,
            "волна {}: жертва возвращается штатным спавном",
            wave + 1
        );
        // доска: спавн вернул ячейку жертвы (последний спавн исполнителя)
        let returned = sessions
            .spawned
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("спавн жертвы в тике затишья");
        set_victim_cell(&board, Some(&returned));
        assert_eq!(
            victim_cell(&board).as_deref(),
            Some(returned.as_str()),
            "волна {}: ячейка жертвы указывает на свежую сессию",
            wave + 1
        );
        busy_exactly(&sessions, &["ses_x".to_string(), returned]);
        advance_tick(&clock);
    }
    assert_eq!(preempts_total, 5, "пять гашений жертвы волной кандидатов");
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter().any(|l| l.contains("status#")),
        "ни одной эскалации за пять циклов гашений: ни жертве, ни кандидатам"
    );
    assert_eq!(
        log.iter().filter(|l| l.starts_with("clear#1=Dev")).count(),
        5,
        "каждое из пяти гашений сняло ячейку жертвы"
    );
}

#[tokio::test]
async fn cup_preempt_resets_victim_loop_guard_and_cooldown() {
    // Контракт сброса на месте гашения: луп-гвард слота жертвы и
    // кулдаун спавна чистятся, ячейка снимается (clear_session) —
    // строка возвращается в очередь с нулевым счётчиком.
    let victim = row_with_session(1, "ses_a", Priority::Low);
    let mut waiter = task(2, Status::InProgress);
    waiter.set_priority(Priority::Medium);
    let state = Arc::new(MemState(Mutex::default()));
    {
        let mut g = state.0.lock().unwrap();
        // «прошлая жизнь» жертвы: счётчик петли и кулдаун от старых спавнов
        g.loops.push((1, Slot::Dev, Status::InProgress, 3));
        g.spawn_ms = vec![(1, Slot::Dev, 1000)];
    }
    let (u, board, sessions, _) =
        usecase_with_state(vec![victim, waiter], PrState::Missing, state.clone());
    live_busy(&sessions, "ses_a");
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.preempts, 1, "жертва погашена");
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|l| l == "clear#1=Dev"),
        "ячейка жертвы снята гашением (clear_session)"
    );
    let loops = state.0.lock().unwrap().loops.clone();
    assert!(
        !loops.iter().any(|(n, s, _, _)| *n == 1 && *s == Slot::Dev),
        "луп-гвард слота жертвы сброшен гашением"
    );
    let ms = state.0.lock().unwrap().spawn_ms.clone();
    assert!(
        !ms.iter().any(|(n, s, _)| *n == 1 && *s == Slot::Dev),
        "кулдаун спавна слота жертвы сброшен — возврат в очередь не ждёт 15 мин"
    );
}

#[tokio::test]
async fn cup_preempt_clear_session_failure_degrades_to_respawn() {
    // Краш-окно: гашение исполнителя удалось, запись на доску (снятие
    // ячейки) упала → гашение засчитано, строка остаётся с мёртвым id
    // и деградирует к прежнему поведению: dead-ветка → respawn с
    // луп-гвардом (не хуже status quo).
    let victim = row_with_session(1, "ses_a", Priority::Low);
    let mut waiter = task(2, Status::InProgress);
    waiter.set_priority(Priority::Medium);
    let (u, board, sessions, _) = usecase(vec![victim, waiter], PrState::Missing);
    board.clear_session_fails.store(true, Ordering::Relaxed);
    live_busy(&sessions, "ses_a");
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.preempts, 1, "гашение засчитано — слот освобождён");
    {
        let log = board.log.lock().unwrap();
        assert!(
            !log.iter().any(|l| l == "clear#1=Dev"),
            "снятие ячейки не удалось — путь деградации"
        );
    }
    // строка осталась с мёртвым id → следующий свободный тик забирает
    // её dead-веткой (respawn с луп-гвардом), как до фикса
    busy_exactly(&sessions, &[]);
    mark_dead(&sessions, "ses_a");
    sessions.live.lock().unwrap().retain(|s| s.id != "ses_a");
    let out2 = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(
        out2.spawns_done, 1,
        "мёртвая ячейка обслужена respawn'ом — прежняя механика"
    );
}

#[tokio::test]
async fn cup_no_preempt_in_peak_for_low_waiter() {
    // Пин порядка гейтов: DeferredPeak раньше HeldCup — не-high
    // кандидат в пик-окно жертв не гасит (его спавн всё равно заморожен)
    let v = row_with_session(1, "ses_v1", Priority::Low);
    let mut waiter = task(2, Status::InProgress);
    waiter.set_priority(Priority::Low);
    let (u, _, sessions, _) = peak_usecase(
        vec![v, waiter],
        Vec::default(),
        Arc::new(MemState(Mutex::default())),
    );
    live_busy(&sessions, "ses_v1");
    let out = u.run(&peak_plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.deferred_peak, 1, "low-кандидат заморожен окном");
    assert_eq!(out.preempts, 0, "замороженный кандидат жертв не гасит");
    assert!(sessions.aborted.lock().unwrap().is_empty());
    assert!(sessions.deleted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cup_no_preempt_in_fallback_scan_unavailable() {
    // Пин склейки #1259×#1263: пул жертв наполняется только при валидном
    // union-скане — fallback (list_sessions недоступен) жертв не гасит.
    // Слепое гашение = класс инцидента #1241: без зрения исполнителя
    // преемпшен невозможен, кандидат держится статусным подсчётом.
    let v = row_with_session(1, "ses_v1", Priority::Low);
    let mut waiter = task(2, Status::InProgress);
    waiter.set_priority(Priority::High);
    let mut sessions = FakeSessions::new();
    sessions.list_fails = true;
    let (u, _, sessions, _) = usecase_with_sessions(
        vec![v, waiter],
        PrState::Missing,
        sessions,
        Arc::new(MemState(Mutex::default())),
    );
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.held_blind, 1, "кандидат в слепом hold");
    assert_eq!(out.preempts, 0, "fallback жертв не гасит");
    assert!(sessions.aborted.lock().unwrap().is_empty());
    assert!(sessions.deleted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cup_idle_low_row_not_preempted_when_cap_held_by_others() {
    // Пин склейки #1259×#1231: busy в пуле жертв — из union-скана
    // (SessionInfo.busy). Idle low-строка (сессия жива на исполнителе,
    // ход завершён) капа не держит и жертвой не становится — даже когда
    // кап полон от busy-чужих. Сломанный busy-фильтр гасил бы спящую
    // сессию зря (abort+delete) вместо ожидания свободного слота.
    let idle = row_with_session(1, "ses_idle_low", Priority::Low);
    let busy = row_with_session(2, "ses_m", Priority::Medium);
    let mut waiter = task(3, Status::InProgress);
    waiter.set_priority(Priority::High);
    let (u, _, sessions, _) = usecase(vec![idle, busy, waiter], PrState::Missing);
    live_busy(&sessions, "ses_m");
    // idle-сайтинг: сессия жива в скане (alive), busy не несёт —
    // между ходами, капа не держит (#1231)
    sessions.live.lock().unwrap().push(LiveSighting {
        id: "ses_idle_low".into(),
        agent: String::new(),
        directory: String::new(),
        outcome: "succeeded".into(),
        updated_ms: 0,
        total_tokens: 0,
    });
    let out = u.run(&plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.preempts, 0, "idle low-строка — не жертва");
    assert_eq!(out.held_cup, 1, "кап полон от busy medium — кандидат ждёт");
    assert!(
        sessions.aborted.lock().unwrap().is_empty(),
        "спящая сессия не абортится"
    );
    assert!(
        sessions.deleted.lock().unwrap().is_empty(),
        "спящая сессия не удаляется"
    );
}

/// Транспортный отказ executor'а — факт тика поимённо (#1312): громкость
/// (порог/анти-повтор) и ТГ-сигнал живут в tick_loop (main.rs), тик несёт
/// имена отказавших — один источник факта, дубль-детекции нет.
#[tokio::test]
async fn executor_failure_lands_in_outcome_by_name() {
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![]),
        log: Mutex::default(),
        comments: Vec::default(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(FakeSessions {
        list_fails: true,
        ..FakeSessions::new()
    });
    let u = TickUseCase {
        board,
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.executors_failed,
        vec!["default".to_string()],
        "отказ union-скана — имя executor'а в outcome"
    );
}

/// Отказ named-исполнителя изолирован от здорового пира (#1263-карта,
/// #1312): в отказ-список попадает только реальный виновник — сигнал
/// громкости не назовёт здорового.
#[tokio::test]
async fn named_executor_failure_is_isolated_from_healthy_peer() {
    let default_gw = Arc::new(FakeSessions::new());
    let named_gw = Arc::new(FakeSessions {
        list_fails: true,
        ..FakeSessions::new()
    });
    let u = TickUseCase {
        board: Arc::new(FakeBoard {
            rows: std::sync::Mutex::new(vec![]),
            log: Mutex::default(),
            comments: Vec::default(),
            fields_fails: false,
            set_session_fails: false,
            clear_session_fails: AtomicBool::new(false),
            status_fails: AtomicBool::new(false),
        }),
        sessions: Arc::new(SessionRouter::new(
            "kisa",
            default_gw.clone(),
            vec![(
                "1xgames".to_string(),
                named_gw.clone() as Arc<dyn AgentSessionGateway>,
            )],
            std::collections::HashMap::from([(
                "1xgames".to_string(),
                named_gw.clone() as Arc<dyn AgentSessionGateway>,
            )]),
        )),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    let out = u.run(&routed_plan(Mode::Live)).await.unwrap();
    assert_eq!(
        out.executors_failed,
        vec!["1xgames".to_string()],
        "здоровый пир не попадает в отказ-список"
    );
}

// ===== Изоляция фейла чтения доски (#1503) =====

/// Тик-юзкейс на мультиборде (#1503): per-board read_board (fail_reads).
fn multi_board_usecase(board: Arc<FakeMultiBoard>) -> TickUseCase {
    TickUseCase {
        board,
        sessions: Arc::new(SessionRouter::uniform(Arc::new(FakeSessions::new()))),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FixedClock),
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    }
}

/// План на две доски (#1503): B1 — здоровая, B2 — падающая в сценарии.
fn two_board_plan(mode: Mode) -> TickPlan {
    TickPlan {
        mode,
        policy: SpawnPolicy {
            cup: 7,
            peak: PeakWindow {
                enabled: false,
                start_min: 0,
                end_min: 0,
            },
        },
        boards: vec![
            BoardPlan {
                project: "p1".into(),
                board: "B1".into(),
                repo: "a/c".into(),
                checkout: "/tmp/c1".into(),
                env_root: "/tmp/env".into(),
            },
            BoardPlan {
                project: "p2".into(),
                board: "B2".into(),
                repo: "a/c".into(),
                checkout: "/tmp/c2".into(),
                env_root: "/tmp/env".into(),
            },
        ],
    }
}

/// 502 одной доски не роняет тик (#1503): строки здоровой доски прочитаны
/// и обработаны (decide по прочитанным), упавшая — warn-событие
/// (board.read_failed) + имя в outcome; ретрай — следующий тик (poll 180 c).
#[tokio::test]
async fn one_board_read_failure_does_not_kill_tick() {
    let board = Arc::new(FakeMultiBoard {
        boards: vec![
            (
                "B1".into(),
                vec![task(1, Status::Todo), task(2, Status::Todo)],
            ),
            ("B2".into(), vec![task(3, Status::Todo)]),
        ],
        fail_reads: vec!["B2".into()],
        log: Mutex::default(),
    });
    let u = multi_board_usecase(board);
    let out = u
        .run(&two_board_plan(Mode::Live))
        .await
        .expect("фейл одной доски не роняет тик");
    assert_eq!(
        out.boards_failed,
        vec!["B2".to_string()],
        "фейл доски — имя в outcome"
    );
    assert_eq!(
        out.rows_total, 2,
        "строки здоровой доски посчитаны, упавшей — нет"
    );
    assert_eq!(out.observed, 2, "decide прошёл по строкам здоровой доски");
}

/// Полный отказ чтения — громкий tick.failed сохраняется (#1503):
/// изоляция не маскирует полный отказ «успешным» пустым тиком.
#[tokio::test]
async fn total_board_read_failure_is_loud_tick_failed() {
    let board = Arc::new(FakeMultiBoard {
        boards: vec![
            ("B1".into(), vec![task(1, Status::Todo)]),
            ("B2".into(), vec![task(2, Status::Todo)]),
        ],
        fail_reads: vec!["B1".into(), "B2".into()],
        log: Mutex::default(),
    });
    let u = multi_board_usecase(board);
    let err = u
        .run(&two_board_plan(Mode::Live))
        .await
        .expect_err("полный отказ — Err наверх (tick.failed)");
    let msg = err.to_string();
    assert!(
        msg.contains("B1") && msg.contains("B2"),
        "ошибка называет все упавшие доски: {msg}"
    );
}

/// Форма инцидента 22–23.11 (наблюдение прода): падает несколько досок,
/// но не все — тик выживает, outcome называет весь состав упавших
/// (#1503; ~50% запросов — 502, состав падающих досок меняется между
/// прогонами).
#[tokio::test]
async fn several_boards_read_failure_tick_survives_with_full_composition() {
    let board = Arc::new(FakeMultiBoard {
        boards: vec![
            (
                "B1".into(),
                vec![task(1, Status::Todo), task(2, Status::Todo)],
            ),
            ("B2".into(), vec![task(3, Status::Todo)]),
            (
                "B3".into(),
                vec![task(4, Status::Todo), task(5, Status::Todo)],
            ),
        ],
        fail_reads: vec!["B2".into(), "B3".into()],
        log: Mutex::default(),
    });
    let mut plan = two_board_plan(Mode::Live);
    plan.boards.push(BoardPlan {
        project: "p3".into(),
        board: "B3".into(),
        repo: "a/c".into(),
        checkout: "/tmp/c3".into(),
        env_root: "/tmp/env".into(),
    });
    let u = multi_board_usecase(board);
    let out = u
        .run(&plan)
        .await
        .expect("падение двух досок из трёх не роняет тик");
    assert_eq!(
        out.boards_failed,
        vec!["B2".to_string(), "B3".to_string()],
        "outcome называет весь состав упавших, порядок — порядок плана"
    );
    assert_eq!(out.rows_total, 2, "посчитаны строки только здоровой доски");
    assert_eq!(out.observed, 2, "decide прошёл по строкам здоровой доски");
}

/// Пустой план — легитимный пустой тик (#1503): гвард полного отказа
/// не триггерит на плане без досок («boards.is_empty()» сам по себе —
/// не отказ).
#[tokio::test]
async fn empty_plan_is_legit_empty_tick_without_guard_trip() {
    let board = Arc::new(FakeMultiBoard {
        boards: vec![],
        fail_reads: vec![],
        log: Mutex::default(),
    });
    let mut plan = two_board_plan(Mode::Live);
    plan.boards.clear();
    let u = multi_board_usecase(board);
    let out = u
        .run(&plan)
        .await
        .expect("план без досок — Ok, гвард не срабатывает");
    assert!(
        out.boards_failed.is_empty(),
        "упавших досок нет — изоляции здесь нет и быть не должно"
    );
    assert_eq!(out.rows_total, 0, "строк нет");
}

// ===== Dead-session доставка: маршрутизация потери сессии (#1288) =====

/// Часы с большим now_ms — тишина сессии ≥ сталл-порогов (сталл-ветка T5).
struct LostClock;
impl Clock for LostClock {
    fn utc_minute(&self) -> UtcMinute {
        UtcMinute {
            minute_of_day: 100,
            weekday: 3,
        }
    }
    fn now_ms(&self) -> u64 {
        20_000_000
    }
}

/// Харнесс потерянной сессии: строка In Progress с дев-сессией в ячейке,
/// настраиваемые комменты, мок исполнителя и часы.
fn lost_usecase(
    comments: Vec<CommentRef>,
    sessions: FakeSessions,
    clock: Arc<dyn Clock>,
) -> (TickUseCase, Arc<FakeBoard>, Arc<FakeSessions>) {
    let board = Arc::new(FakeBoard {
        rows: std::sync::Mutex::new(vec![dev_task(16, "ses_dev_16")]),
        log: Mutex::default(),
        comments,
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(sessions);
    let prs = Arc::new(FakePrs {
        state: PrState::Missing,
        merges: Mutex::default(),
        probes: Mutex::default(),
    });
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::uniform(sessions.clone())),
        prs,
        clock,
        state: Arc::new(MemState(Mutex::default())),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    };
    (u, board, sessions)
}

/// Слепок «сессия в списке исполнителя» (десинк-класс: доставка 404 при
/// живом списке). Пустые agent/directory — вне раскладки гарда спавна.
fn listed_sighting(sessions: &FakeSessions, id: &str) {
    sessions.live.lock().unwrap().push(LiveSighting {
        id: id.into(),
        agent: String::new(),
        directory: String::new(),
        outcome: String::new(),
        updated_ms: 0,
        total_tokens: 0,
    });
}

/// Свежий foreign-коммент: wakeup-путь пытается доставить его каждый тик
/// (seen не пишется при отклонённой доставке — ретрай бесконечен).
fn foreign_comment() -> Vec<CommentRef> {
    vec![CommentRef {
        id: "c1".into(),
        created_ms: 1999,
        body: "чужой коммент: wakeup доставит".into(),
    }]
}

fn parked(board: &FakeBoard) -> bool {
    board
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("NeedsAttention"))
}

#[tokio::test]
async fn lost_session_desync_counts_and_parks_after_threshold_via_wakeup() {
    // T1 (#1288): сессия в списке исполнителя (десинк «в списке, но
    // доставка 404»), wakeup-промпт отклонён — респавн запрещён анти-дублем;
    // порог 3 неудач подряд → Needs attention
    let sessions = FakeSessions::new();
    sessions
        .prompt_lost
        .lock()
        .unwrap()
        .push("ses_dev_16".into());
    listed_sighting(&sessions, "ses_dev_16");
    let (u, board, sess) = lost_usecase(foreign_comment(), sessions, Arc::new(FixedClock));
    // тики 1–2: счётчик растёт, статус не тронут, спавнов нет
    for _ in 0..2 {
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        assert_eq!(out.session_lost_flagged, 0, "парковка раньше порога");
        assert!(
            sess.spawned.lock().unwrap().is_empty(),
            "десинк: респавна нет"
        );
        assert!(!parked(&board), "до порога строка не паркуется");
    }
    // тик 3: порог достигнут — парковка с причиной
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.session_lost_flagged, 1, "парковка на пороге");
    assert!(parked(&board), "строка в Needs attention после порога");
    assert!(
        sess.spawned.lock().unwrap().is_empty(),
        "анти-дубль: сессия в списке — respawn запрещён"
    );
}

#[tokio::test]
async fn lost_session_resurrects_then_respawn_loop_guard_parks() {
    // T2 (#1288, красный тест BDD): mock-исполнитель create → ok,
    // prompt → 404. Сессии ячейки нет в списке (потеря подтверждена
    // исполнителем) → resurrection каждый тик; свежие сессии тоже 404-ит
    // (сирота-путь), попытки считает луп-гвард домена — на 5-м тике
    // «Респавн-петля» → Needs attention
    let mut sessions = FakeSessions::new();
    sessions.prompt_lost_all = true;
    let (u, board, sess) = lost_usecase(foreign_comment(), sessions, Arc::new(FixedClock));
    // тик 1: resurrection предпринята (create ok, свежая сессия 404-ит —
    // сирота-делит), завершённого спавна нет
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 0, "спавн не завершается: промпт 404");
    assert!(
        !sess.spawned.lock().unwrap().is_empty(),
        "resurrection предпринята: create прошёл"
    );
    assert!(
        !sess.deleted.lock().unwrap().is_empty(),
        "сирота-зачистка: недоставляемая свежая сессия гасится"
    );
    // тики 2–4: попытки продолжаются, парковки нет (луп-гвард ≤ 4)
    for _ in 0..3 {
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        assert_eq!(
            out.session_lost_flagged, 0,
            "lost-счётчик не считает сироту-путь"
        );
        assert!(!parked(&board), "луп-гвард ещё не на потолке");
    }
    // тик 5: луп-гвард >4 — «Респавн-петля» → Needs attention
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(parked(&board), "луп-гвард респавнов паркует на 5-м тике");
}

#[tokio::test]
async fn lost_session_desync_parks_via_stall_soft_ping() {
    // T5 (#1288, сталл-ветка): без свежих foreign-комментов (wakeup молчит),
    // idle-сессия, тишина ≥ soft-порога — мягкий сталл-пинг отклонён 404.
    // Та же маршрутизация: счётчик → порог 3 → Needs attention
    let mut sessions = FakeSessions::new();
    sessions
        .prompt_lost
        .lock()
        .unwrap()
        .push("ses_dev_16".into());
    sessions.activity_ms = Some(0);
    sessions.idle_reply = true;
    listed_sighting(&sessions, "ses_dev_16");
    let (u, board, sess) = lost_usecase(Vec::new(), sessions, Arc::new(LostClock));
    for _ in 0..2 {
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        assert_eq!(out.session_lost_flagged, 0, "парковка раньше порога");
        assert!(
            sess.spawned.lock().unwrap().is_empty(),
            "десинк: респавна нет"
        );
    }
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(parked(&board), "сталл-ветка паркует по тому же порогу");
}

#[tokio::test]
async fn transient_delivery_failure_still_retries_without_escalation() {
    // T3 (#1288): временный отказ доставки (не 404) — ретрай как раньше:
    // без эскалации и без respawn, серия попыток продолжается
    let mut sessions = FakeSessions::new();
    sessions.prompt_fails = true;
    let (u, board, sess) = lost_usecase(foreign_comment(), sessions, Arc::new(FixedClock));
    for _ in 0..4 {
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        assert_eq!(out.session_lost_flagged, 0, "временный отказ не паркует");
        assert!(
            sess.spawned.lock().unwrap().is_empty(),
            "временный отказ не респавнит"
        );
        assert!(!parked(&board));
    }
    assert_eq!(
        sess.switched.lock().unwrap().len(),
        4,
        "доставка ретраится каждым тиком как раньше (пин агента перед ходом)"
    );
}

#[tokio::test]
async fn busy_session_gets_no_delivery_and_is_never_counted_lost() {
    // T4 (#1288): busy-сессия (ход в полёте) доставок не получает —
    // свежая активность новее коммента, счётчик потери не заводится
    let mut sessions = FakeSessions::new();
    sessions.activity_ms = Some(5000);
    sessions.busy_ids.lock().unwrap().push("ses_dev_16".into());
    listed_sighting(&sessions, "ses_dev_16");
    let (u, board, sess) = lost_usecase(foreign_comment(), sessions, Arc::new(FixedClock));
    for _ in 0..3 {
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        assert_eq!(out.session_lost_flagged, 0);
        assert!(!parked(&board));
    }
    assert!(
        sess.prompts.lock().unwrap().is_empty(),
        "busy-сессии доставок нет"
    );
}

#[tokio::test]
async fn dead_probe_with_fresh_404s_resurrects_once_per_tick_and_parks_on_fifth() {
    // Ревью раунда 1 (#1288): проба Dead + 404 промптов свежих спавнов —
    // отказ respawn_dead_stage в Dead-ветке не возвращается в
    // маршрутизацию (анти-ре-энтри): одна resurrection-попытка за тик
    // (bootstrap+рабочая); луп-гвард считает respawn-попытки — парковка
    // после 5-й (тик 1 — kick_unfreeze, попытки с тика 2)
    let mut sessions = FakeSessions::new();
    sessions.prompt_lost_all = true;
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dev_16".into());
    let (u, board, sess) = lost_usecase(Vec::new(), sessions, Arc::new(FixedClock));
    // тик 1: kick_unfreeze — мёртвая дев-сессия снимается (#397), plain
    // spawn предпринят и отклонён 404
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.actions_failed, 1, "plain spawn тика 1 отклонён 404");
    // тики 2–5: dead-respawn-попытки (ровно одна за тик), луп-гвард ≤ 4
    for tick in 2..=5 {
        let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
        assert_eq!(
            out.session_lost_flagged, 0,
            "не десинк-класс: сессии нет в списке"
        );
        assert_eq!(
            sess.spawned.lock().unwrap().len(),
            2 * tick,
            "ровно одна попытка за тик (bootstrap+рабочая), без повторного входа"
        );
        assert!(!parked(&board), "луп-гвард ≤ 4 — парковки нет (тик {tick})");
    }
    // тик 6: пятая respawn-попытка — луп-гвард >4 → «Респавн-петля»
    u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(parked(&board), "луп-гвард паркует после 5 respawn-попыток");
}

// ——— Матрица e2e-семантики «peak × ребут × приоритеты» (#1310) ———
// Сценарий владельца (18.11): пик-окно, ребут ноды и приоритеты сходятся
// в одном контуре — конвейер переживает комбинацию без потери отложенных
// спавнов и без ложных сталлов. Якоря семантики: Design Spec UC-05,
// AGENTS peak-клауза, ADR-034/044/101/107, #1300, #1259. Факт кода
// (ревизия развилки V4): deferred-носителя нет — гейт пересчитывается
// временем каждого тика; персистентный TickStateStore deferred не держит.

/// Часы матрицы: фаза «пик / после окна» (тест переключает между тиками —
/// «время вышло из окна») + монотонный now_ms с шагом 16 мин: реальный
/// тик ≥ минута, шаг переступает кулдаун спавна 15 мин (`SPAWN_COOLDOWN_MS`).
struct MatrixClock {
    in_peak: AtomicBool,
    now_ms: Mutex<u64>,
}

impl MatrixClock {
    fn new() -> Self {
        Self {
            in_peak: AtomicBool::new(true),
            now_ms: Mutex::new(2000),
        }
    }
}

impl Clock for MatrixClock {
    fn utc_minute(&self) -> UtcMinute {
        if self.in_peak.load(Ordering::SeqCst) {
            // будни, внутри окна 360–600 (peak_plan)
            UtcMinute {
                minute_of_day: 400,
                weekday: 3,
            }
        } else {
            // будни, окно кончилось — гейт оценивается временем тика
            UtcMinute {
                minute_of_day: 700,
                weekday: 3,
            }
        }
    }
    fn now_ms(&self) -> u64 {
        let mut t = self.now_ms.lock().unwrap();
        let now = *t;
        *t += 16 * 60_000;
        now
    }
}

/// Юзкейс матрицы: фейки портов + MatrixClock (в пике). Часы возвращаются
/// тесту для переключения фазы между тиками.
fn matrix_usecase(
    rows: Vec<Task>,
    comments: Vec<CommentRef>,
    state: Arc<MemState>,
    idle_reply: bool,
) -> (
    TickUseCase,
    Arc<FakeBoard>,
    Arc<FakeSessions>,
    Arc<FakePrs>,
    Arc<MatrixClock>,
) {
    let clock = Arc::new(MatrixClock::new());
    let (u, board, sessions, prs) = fixture_usecase(
        rows,
        comments,
        state,
        clock.clone(),
        FakeSessions {
            idle_reply,
            ..FakeSessions::new()
        },
    );
    (u, board, sessions, prs, clock)
}

fn medium_design(n: u64) -> Task {
    let mut t = task(n, Status::Design);
    t.set_priority(Priority::Medium);
    t
}

fn high_design(n: u64) -> Task {
    let mut t = task(n, Status::Design);
    t.set_priority(Priority::High);
    t
}

/// V1 — гиркин сценария владельца (18.11): N medium в работе и high-задачи.
/// Пик: medium-ходы в полёте дорабатывают (пик не абортит), medium-спавны
/// отложены, high идут. После окна: medium выезжают штатно.
#[tokio::test]
async fn owner_scenario_peak_holds_medium_spawns_high_then_releases_medium_after_window() {
    let state = Arc::new(MemState(Mutex::default()));
    let busy = row_with_session(300, "ses_dev_busy", Priority::Medium);
    let (u, board, sessions, _, clock) = matrix_usecase(
        vec![
            busy,
            medium_design(301),
            medium_design(302),
            high_design(303),
            high_design(304),
        ],
        Vec::new(),
        state,
        false,
    );
    live_busy(&sessions, "ses_dev_busy");
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.deferred_peak, 2, "medium-спавны отложены пиком");
    assert_eq!(out.spawns_done, 2, "high идут сквозь пик штатно");
    assert!(
        sessions.aborted.lock().unwrap().is_empty(),
        "ходы в полёте пик не прерывает (гейтится только новый спавн)"
    );
    assert_eq!(out.stall_flagged, 0, "ложных эскалаций нет");

    // Пик кончился; high-строки двинулись стадией (агент перевёл сам,
    // ячейки живы), medium-кандидаты едут после окна.
    clock.in_peak.store(false, Ordering::SeqCst);
    *board.rows.lock().unwrap() = vec![
        row_with_session(300, "ses_dev_busy", Priority::Medium),
        row_with_session(303, "ses_dev_h303", Priority::High),
        row_with_session(304, "ses_dev_h304", Priority::High),
        medium_design(301),
        medium_design(302),
    ];
    let out2 = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out2.spawns_done, 2, "medium запущены штатно после окна");
    assert_eq!(out2.deferred_peak, 0);
    assert_eq!(out2.stall_flagged, 0);
    let log = board.log.lock().unwrap();
    assert!(
        log.iter().any(|l| l.starts_with("session#301="))
            && log.iter().any(|l| l.starts_with("session#302=")),
        "обе medium-строки заспавнены: {log:?}"
    );
}

/// V2: ребут ноды убил ход high-сессии в пике. Респавн high проходит пик
/// немедленно, роль слота не меняется (#1300: рестарт не подменяет
/// stage-агента — спавн той же роли).
#[tokio::test]
async fn node_reboot_mid_high_turn_respawns_same_developer_role_in_peak() {
    let state = Arc::new(MemState(Mutex::default()));
    // тот же раунд (статус не менялся) — путь чистого respawn
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((310, Status::InProgress));
    let (u, _, sessions, _, _) = matrix_usecase(
        vec![row_with_session(310, "ses_dev_dead", Priority::High)],
        Vec::new(),
        state,
        false,
    );
    sessions
        .dead_sessions
        .lock()
        .unwrap()
        .push("ses_dev_dead".into());
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.spawns_done, 1, "high-респавн в пике немедленный");
    assert_eq!(out.wakeups_deferred, 0, "пик high-респавн не держит");
    let specs = sessions.specs.lock().unwrap();
    let work = specs.last().expect("рабочая сессия респавна");
    assert!(
        matches!(work.agent, uwuwu_domain::AgentSlot::Role(Agent::Developer)),
        "#1300: спавн той же роли слота (developer), получено {:?}",
        work.agent
    );
}

/// V3: окно кончилось, пока нода была выключена. Гейт оценивается временем
/// тика, не временем постановки — deferred исполняется на первом тике
/// после окна (UC-05: отложено «до конца окна», не навсегда).
#[tokio::test]
async fn deferred_medium_spawn_executes_on_first_tick_after_window() {
    let state = Arc::new(MemState(Mutex::default()));
    let (u, _, sessions, _, clock) =
        matrix_usecase(vec![medium_design(320)], Vec::new(), state, false);
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.deferred_peak, 1);
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "в пике спавна нет"
    );
    clock.in_peak.store(false, Ordering::SeqCst);
    let out2 = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out2.spawns_done, 1,
        "отложенное исполняется на первом тике после окна"
    );
    assert_eq!(out2.deferred_peak, 0);
}

/// V4 (ревизия развилки — факт кода): deferred-носителя нет — откладка
/// пересчитывается гейтом от времени тика, `deferred_peak` — диагностический
/// счётчик Out, TickStateStore deferred не держит. Рестарт uwuwu-server
/// (пустой state) в пике ничего не теряет: после конца окна спавн
/// исполняется. Бюджет in-memory (ADR-107) не применим: очередь не
/// материализована вовсе.
#[tokio::test]
async fn server_restart_in_peak_loses_nothing_deferred_recomputed_from_clock() {
    let state = Arc::new(MemState(Mutex::default()));
    let (u, _, sessions, _, clock) =
        matrix_usecase(vec![medium_design(330)], Vec::new(), state.clone(), false);
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.deferred_peak, 1, "в пике medium отложен");
    // рестарт: state-носитель пуст (JSON пережил бы, но deferred в нём
    // и не бывал), время идёт
    *state.0.lock().unwrap() = MemStateInner::default();
    clock.in_peak.store(false, Ordering::SeqCst);
    let out2 = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out2.spawns_done, 1,
        "отложенный спавн не потерян рестартом — носителя и не требовалось"
    );
    assert!(
        !sessions.spawned.lock().unwrap().is_empty(),
        "сессия создана после рестарта"
    );
}

/// V5: high завершается в пике — следующий спавн high-цепочки (ревьювер
/// плана) немедленный (AGENTS: «high продолжают работать»).
#[tokio::test]
async fn high_chain_next_stage_spawn_is_immediate_in_peak() {
    let state = Arc::new(MemState(Mutex::default()));
    let mut t = task(340, Status::ReviewPlan);
    t.set_priority(Priority::High);
    let (u, _, sessions, _, _) = matrix_usecase(vec![t], Vec::new(), state, false);
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.spawns_done, 1,
        "следующий спавн high-цепочки не ждёт конца окна"
    );
    assert_eq!(out.deferred_peak, 0);
    let specs = sessions.specs.lock().unwrap();
    let work = specs.last().expect("сессия ревьювера");
    assert!(
        matches!(
            work.agent,
            uwuwu_domain::AgentSlot::Role(Agent::PlanReviewer)
        ),
        "стадия ревью-плана — агент plan-reviewer, получено {:?}",
        work.agent
    );
}

/// V6: кап полон и пик идёт — HeldCup (high) и DeferredPeak (medium)
/// сосуществуют (пик проверяется раньше капа); после окна первым выезжает
/// high из held (очередь rank, sort_by_key), medium ждёт следующий слот.
#[tokio::test]
async fn cup_full_and_peak_coexist_high_exits_held_first_after_window() {
    let state = Arc::new(MemState(Mutex::default()));
    let busy = row_with_session(350, "ses_dev_busy", Priority::Medium);
    let (u, board, sessions, _, clock) = matrix_usecase(
        vec![busy, high_design(351), medium_design(352)],
        Vec::new(),
        state,
        false,
    );
    live_busy(&sessions, "ses_dev_busy"); // active = 1 = cup
    let out = u.run(&peak_plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out.held_cup, 1, "high держит полный кап");
    assert_eq!(
        out.deferred_peak, 1,
        "medium держит пик — пик проверяется раньше капа"
    );
    assert_eq!(out.spawns_done, 0, "в окне спавнов нет");
    clock.in_peak.store(false, Ordering::SeqCst);
    sessions.busy_ids.lock().unwrap().clear(); // ход доработался, слот свободен
    let out2 = u.run(&peak_plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out2.spawns_done, 1, "за тик ровно кап спавнов");
    assert_eq!(out2.held_cup, 1, "medium ждёт следующий свободный слот");
    {
        let log = board.log.lock().unwrap();
        assert!(
            log.iter().any(|l| l.starts_with("session#351=")),
            "первым из held выехал high: {log:?}"
        );
        assert!(
            !log.iter().any(|l| l.starts_with("session#352=")),
            "medium не перепрыгнул очередь капа"
        );
    }
    // тик записал ячейку high на носитель — строка двинулась стадией
    *board.rows.lock().unwrap() = vec![
        row_with_session(350, "ses_dev_busy", Priority::Medium),
        row_with_session(351, "ses_dev_h351", Priority::High),
        medium_design(352),
    ];
    let out3 = u.run(&peak_plan(Mode::Live, 1)).await.unwrap();
    assert_eq!(out3.spawns_done, 1, "medium выехал на следующем тике");
    assert!(board
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("session#352=")));
}

/// V7: накопленный deferred-backlog выезжает после окна батчами по капу,
/// без потерь, в порядке очереди (ADR-107: пауза — не отмена).
#[tokio::test]
async fn deferred_backlog_drains_after_window_in_rank_order_within_cup() {
    let state = Arc::new(MemState(Mutex::default()));
    let (u, board, _, _, clock) = matrix_usecase(
        vec![medium_design(360), medium_design(361), medium_design(362)],
        Vec::new(),
        state,
        false,
    );
    let out = u.run(&peak_plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out.deferred_peak, 3, "весь backlog отложен окном");
    clock.in_peak.store(false, Ordering::SeqCst);
    let out2 = u.run(&peak_plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out2.spawns_done, 2, "первый батч — по капу");
    assert_eq!(out2.held_cup, 1, "хвост backlog ждёт следующий тик");
    // тик записал ячейки первой пары на носитель — строки в работе
    *board.rows.lock().unwrap() = vec![
        row_with_session(360, "ses_dev_h360", Priority::Medium),
        row_with_session(361, "ses_dev_h361", Priority::Medium),
        medium_design(362),
    ];
    let out3 = u.run(&peak_plan(Mode::Live, 2)).await.unwrap();
    assert_eq!(out3.spawns_done, 1, "хвост выехал — потерь нет");
    let log = board.log.lock().unwrap();
    for n in [360, 361, 362] {
        assert!(
            log.iter().any(|l| l.starts_with(&format!("session#{n}="))),
            "строка {n} исполнена (весь backlog довезён): {log:?}"
        );
    }
}

/// V8 (ревизия 2 — два вида deferred): только medium в пике.
/// (а) орфан без сессии: спавн-ветка → DeferredPeak, до сталл-пасса не
/// доходит; (б) живая idle-сессия (ход завершён до окна) с зависшим
/// будильником: шорт-кат WakeAct::Deferred минует stall_pass. Ни одна
/// строка не эскалируется (ADR-044/101: сталл — про живую сессию, не про
/// отложенный спавн; страховка idle-сессии в окне).
#[tokio::test]
async fn peak_defers_medium_without_false_stalls_orphans_and_pending_wakeups() {
    let state = Arc::new(MemState(Mutex::default()));
    let idle = row_with_session(371, "ses_dev_idle", Priority::Medium);
    let (u, board, sessions, _, _) = matrix_usecase(
        vec![medium_design(370), idle],
        vec![CommentRef {
            id: "c1".into(),
            created_ms: 3000,
            body: "свежий коммент — будильник pendинг до конца окна".into(),
        }],
        state,
        true, // idle_reply: ход завершён (финал) — до сталл-пинга далеко
    );
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.deferred_peak, 1, "орфан-спавн отложен пиком");
    assert_eq!(
        out.wakeups_deferred, 1,
        "будильник живой сессии отложен пиком (шорт-кат мимо сталл-пасса)"
    );
    assert_eq!(out.stall_pings, 0, "deferred не пингается сталлом");
    assert_eq!(out.stall_flagged, 0, "deferred не эскалируется сталлом");
    assert_eq!(out.soft_pings, 0);
    assert!(
        sessions.prompts.lock().unwrap().is_empty(),
        "доставка будильника не ушла в пике"
    );
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter().any(|l| l.starts_with("status#")),
        "статусы строк нетронуты (никаких Needs attention): {log:?}"
    );
}

/// Ревизия 2 (находка Low ревью плана): гейт деферрит весь не-high
/// (`priority != High`) — низкие ранги пинятся явно, дрейф рангов пойман.
#[tokio::test]
async fn low_and_empty_priorities_defer_in_peak_same_gate() {
    let state = Arc::new(MemState(Mutex::default()));
    let mut low = task(380, Status::Design);
    low.set_priority(Priority::Low);
    let empty = task(381, Status::Design); // дефолт Empty — ниже low
    let (u, board, sessions, _, _) = matrix_usecase(vec![low, empty], Vec::new(), state, false);
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(
        out.deferred_peak, 2,
        "low и empty отложены тем же гейтом пика"
    );
    assert!(sessions.spawned.lock().unwrap().is_empty());
    assert_eq!(out.stall_flagged, 0, "эскалаций нет");
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("status#")),
        "статусы нетронуты"
    );
}

/// Тест-раунд #1310 (допокрытие тестировщика): отложенный пиком будильник
/// доставляется после окна — деферр не пишет seen-маркер, коммент не
/// теряется и не дублируется (UC-05 «откладывается до конца окна»,
/// семейство V4/V8 «не потерять отложенное»). Разрыв = молчаливая потеря
/// коммента после окна при записи маркера на деферре — счётчиками
/// (wakeups_deferred) этот класс не виден.
#[tokio::test]
async fn deferred_wakeup_delivered_after_window_without_loss_or_dup() {
    let state = Arc::new(MemState(Mutex::default()));
    let idle = row_with_session(390, "ses_dev_idle", Priority::Medium);
    let (u, _, sessions, _, clock) = matrix_usecase(
        vec![idle],
        vec![CommentRef {
            id: "c1".into(),
            created_ms: 3000,
            body: "свежий коммент владельца".into(),
        }],
        state.clone(),
        true, // idle_reply: ход завершён до окна — коммент свежее ответа
    );
    let out = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.wakeups_deferred, 1, "в пике будильник отложен");
    assert!(
        sessions.prompts.lock().unwrap().is_empty(),
        "доставки в окне нет"
    );
    assert!(
        state.0.lock().unwrap().comments.is_empty(),
        "seen-маркер на деферре не пишется — иначе коммент потеряется после окна"
    );

    // Окно кончилось: тот же коммент доставляется, маркер поглощает его
    clock.in_peak.store(false, Ordering::SeqCst);
    let out2 = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out2.wakeups, 1, "отложенный будильник доставлен после окна");
    {
        let prompts = sessions.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1, "ровно одна доставка: {prompts:?}");
        assert!(
            prompts[0].starts_with("ses_dev_idle|Wakeup"),
            "промпт уходит в живую idle-сессию: {prompts:?}"
        );
    }
    assert_eq!(
        state.0.lock().unwrap().comments.as_slice(),
        ["c1"],
        "коммент поглощён маркером — ретрая не будет"
    );

    // Следующий тик не дублирует доставку (маркер уже стоит)
    let out3 = u.run(&peak_plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out3.wakeups, 0, "повторной доставки нет");
    let wake_prompts = sessions
        .prompts
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.contains("Wakeup:"))
        .count();
    assert_eq!(wake_prompts, 1, "дублей wake-доставки нет");
}

// —— Контур судьи (#1364): verdict-тесты на фейковом судье ——

struct FakeJudge {
    verdict: Mutex<Option<uwuwu_domain::JudgeVerdict>>,
    fail: AtomicBool,
    calls: Mutex<Vec<crate::ports::JudgeContext>>,
}

impl FakeJudge {
    fn says(status: uwuwu_domain::JudgeStatus) -> Arc<Self> {
        Arc::new(Self {
            verdict: Mutex::new(Some(uwuwu_domain::JudgeVerdict {
                status,
                owner_needed: 0.9,
                confidence: 0.7,
            })),
            fail: AtomicBool::new(false),
            calls: Mutex::default(),
        })
    }
    fn down() -> Arc<Self> {
        Arc::new(Self {
            verdict: Mutex::new(None),
            fail: AtomicBool::new(true),
            calls: Mutex::default(),
        })
    }
}

#[async_trait]
impl DecisionJudge for FakeJudge {
    async fn decide(
        &self,
        ctx: &crate::ports::JudgeContext,
    ) -> UsecaseResult<uwuwu_domain::JudgeVerdict> {
        self.calls.lock().unwrap().push(ctx.clone());
        if self.fail.load(Ordering::Relaxed) {
            return Err(UsecaseError::Session("judge недоступна (мок)".into()));
        }
        self.verdict
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| UsecaseError::Session("вердики кончились (мок)".into()))
    }
}

/// Судья с hook'ом — хореография внутри decide(): симуляция агентской
/// транзакции, завершившейся за время сбора контекста и вызова модели
/// (окно race-чека паркингов, ревью раунда 4 Low).
struct HookJudge {
    hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    verdict: uwuwu_domain::JudgeStatus,
}

#[async_trait]
impl DecisionJudge for HookJudge {
    async fn decide(
        &self,
        _ctx: &crate::ports::JudgeContext,
    ) -> UsecaseResult<uwuwu_domain::JudgeVerdict> {
        if let Some(h) = self.hook.lock().unwrap().as_ref() {
            h();
        }
        Ok(uwuwu_domain::JudgeVerdict {
            status: self.verdict,
            owner_needed: 0.9,
            confidence: 0.7,
        })
    }
}

/// Юзкейс с судьёй: rows на доске, judge-конфиг включён, тишина сессии
/// управляется activity_ms (ms), idle-фаза — idle_reply.
fn judge_usecase(
    rows: Vec<Task>,
    activity_ms: Option<u64>,
    idle_reply: bool,
    state: Arc<MemState>,
    judge_cfg: (Option<Arc<dyn DecisionJudge>>, JudgeCfg),
) -> (
    TickUseCase,
    Arc<FakeBoard>,
    Arc<FakeSessions>,
    Arc<MemState>,
) {
    let (judge, cfg) = judge_cfg;
    let board = Arc::new(FakeBoard {
        rows: rows.into(),
        log: Mutex::default(),
        comments: Vec::new(),
        fields_fails: false,
        set_session_fails: false,
        clear_session_fails: AtomicBool::new(false),
        status_fails: AtomicBool::new(false),
    });
    let sessions = Arc::new(FakeSessions {
        activity_ms,
        idle_reply,
        ..FakeSessions::new()
    });
    struct FreezeClock;
    impl Clock for FreezeClock {
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
    let u = TickUseCase {
        board: board.clone(),
        sessions: Arc::new(SessionRouter::new(
            "kisa",
            sessions.clone(),
            Vec::new(),
            std::collections::HashMap::new(),
        )),
        prs: Arc::new(FakePrs {
            state: PrState::Missing,
            merges: Mutex::default(),
            probes: Mutex::default(),
        }),
        clock: Arc::new(FreezeClock),
        state: state.clone(),
        models: Arc::new(crate::spawn_models::SpawnModels::canonical()),
        boot_ms: 0,
        judge,
        judge_cfg: cfg,
        quota: None,
    };
    (u, board, sessions, state)
}

/// In Progress с дев-сессией + сессия-stage-slot = dev (сталл-точка).
fn judge_row(n: u64, status: Status, session: &str, slot: Slot) -> Task {
    let mut t = task(n, status);
    let mut f = TaskFields::default();
    f.set_session(slot, uwuwu_domain::SessionRef::parse(session).unwrap());
    t.apply_fields(f);
    t
}

#[tokio::test]
async fn judge_noise_state_wakes_without_transition() {
    // Спайковый кейс C2 (#1364): масс-аномалия/тишина ниже эскалационной —
    // вердикт «текущий статус» = пробуждение без переноса: владельца нет,
    // пингов старой механики нет.
    const NOW: u64 = 100_000_000;
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::InProgress);
    let cfg = JudgeCfg {
        enabled: true,
        ..JudgeCfg::default()
    };
    let (u, board, sessions, _) = judge_usecase(
        vec![judge_row(41, Status::InProgress, "ses_dev", Slot::Dev)],
        Some(NOW - 40 * 60_000), // тишина 40 мин, idle
        true,
        Arc::new(MemState(Mutex::default())),
        (Some(judge.clone() as Arc<dyn DecisionJudge>), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.judge_calls, 1, "вызов судьи был");
    assert_eq!(out.judge_wakeups, 1, "пробуждение без переноса");
    assert_eq!(out.judge_transitions, 0, "переноса нет");
    assert_eq!(out.stall_flagged, 0, "паркинга нет");
    assert!(
        sessions
            .prompts
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("Pipeline judge")),
        "агент разбужен промптом судьи"
    );
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("status#41=")),
        "статус не менялся"
    );
    // контекст судьи: тишина и статус на месте
    let ctx = judge.calls.lock().unwrap().last().unwrap().clone();
    assert_eq!(ctx.silence_min, 40);
    assert!(ctx.idle, "ход завершён (idle)");
}

#[tokio::test]
async fn judge_verdict_transitions_row_with_race_check_passing() {
    // Вердикт Тестирование на ReviewResult-строке: перенос + дайджест +
    // промпт агенту; носитель сталла сброшен (статус сменился).
    const NOW: u64 = 100_000_000;
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::Testing);
    let cfg = JudgeCfg {
        enabled: true,
        ..JudgeCfg::default()
    };
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((42, Status::ReviewResult));
    state
        .0
        .lock()
        .unwrap()
        .stalls
        .push((42, Status::ReviewResult, NOW - 3 * 60 * 60_000));
    let (u, board, sessions, state) = judge_usecase(
        vec![judge_row(
            42,
            Status::ReviewResult,
            "ses_rev",
            Slot::ResultReview,
        )],
        Some(NOW - 90 * 60_000),
        true,
        state,
        (Some(judge), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.judge_transitions, 1);
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "status#42=Testing"),
        "перенос в Тестирование: {log:?}",
        log = board.log.lock().unwrap()
    );
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("comment#42=⚖︎ Judge:")),
        "дайджест вердикта в ленте"
    );
    assert!(
        state.0.lock().unwrap().stalls.is_empty(),
        "носитель сталла сброшен"
    );
    let _ = sessions;
}

/// Перенос вердиктом не будит старую сессию — архитектура владельца
/// «перенос = пинг» (C2 плана #1364): адресат промпта — агент прошлой
/// стадии, контракту новой стадии он не отвечает; спавн новой стадии —
/// штатный тик, не ветка судьи. Регрессия (возврат промпта в ветку
/// переноса) = дубль-будж агенту, которого ни один тест не ловил.
#[tokio::test]
async fn judge_transition_prompts_nobody_verdict_move_is_the_ping() {
    const NOW: u64 = 100_000_000;
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::Testing);
    let cfg = JudgeCfg {
        enabled: true,
        ..JudgeCfg::default()
    };
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((44, Status::ReviewResult));
    state
        .0
        .lock()
        .unwrap()
        .stalls
        .push((44, Status::ReviewResult, NOW - 3 * 60 * 60_000));
    let (u, board, sessions, _) = judge_usecase(
        vec![judge_row(
            44,
            Status::ReviewResult,
            "ses_rev44",
            Slot::ResultReview,
        )],
        Some(NOW - 90 * 60_000),
        true,
        state,
        (Some(judge), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.judge_transitions, 1, "перенос состоялся");
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "status#44=Testing"),
        "строка перенесена в Тестирование"
    );
    assert!(
        sessions.prompts.lock().unwrap().is_empty(),
        "перенос не промптит старую сессию — перенос и есть пинг (C2)"
    );
    assert!(
        sessions.spawned.lock().unwrap().is_empty(),
        "спавн новой стадии делает штатный тик, не ветка судьи"
    );
}

#[tokio::test]
async fn judge_rejects_illegal_verdict_as_hold() {
    // Вердикт Тестирование на Design-строке: прыжок через стадии канона —
    // гард отклоняет, hold-эквивалент (мутаций нет, счётчик rejected).
    const NOW: u64 = 100_000_000;
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::Testing);
    let cfg = JudgeCfg {
        enabled: true,
        ..JudgeCfg::default()
    };
    let (u, board, sessions, _) = judge_usecase(
        vec![judge_row(43, Status::Design, "ses_design", Slot::Design)],
        Some(NOW - 20 * 60_000),
        true,
        Arc::new(MemState(Mutex::default())),
        (Some(judge), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.judge_rejected, 1, "гард отклонил вердикт");
    assert_eq!(out.judge_transitions, 0);
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("status#43=")),
        "мутаций нет"
    );
    assert!(
        sessions.prompts.lock().unwrap().is_empty(),
        "hold-эквивалент не промптит"
    );
}

#[tokio::test]
async fn judge_err_falls_back_to_classic_soft_ping() {
    // Судья недоступна (Err) — старая механика: мягкий пинг idle-сессии.
    const NOW: u64 = 100_000_000;
    let cfg = JudgeCfg {
        enabled: true,
        ..JudgeCfg::default()
    };
    let (u, _, sessions, _) = judge_usecase(
        vec![judge_row(44, Status::InProgress, "ses_dev", Slot::Dev)],
        Some(NOW - 16 * 60_000),
        true,
        Arc::new(MemState(Mutex::default())),
        (Some(FakeJudge::down()), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.judge_calls, 0, "вызова судьи не было");
    assert_eq!(out.soft_pings, 1, "fallback: мягкий пинг старой механики");
    assert!(
        sessions
            .prompts
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("Soft ping")),
        "старый текст мягкого пинга (English-канон #1383)"
    );
}

#[tokio::test]
async fn judge_guard_forces_park_without_call() {
    // Гвард сверху: 3 вызова без смены статуса — 4-я сталл-точка паркует
    // строку БЕЗ нового вызова модели (слово владельца «порог 3»).
    const NOW: u64 = 100_000_000;
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::Hold);
    let cfg = JudgeCfg {
        enabled: true,
        every_ms: 0, // интервал повторов не держит — все точки за один тик-путь не нужны,
        // сталл-проход один за тик: count сеем носителем
        ..JudgeCfg::default()
    };
    let state = Arc::new(MemState(Mutex::default()));
    // 3 вызова без смены статуса: три записи одного раунда
    state
        .0
        .lock()
        .unwrap()
        .stalls
        .push((45, Status::InProgress, NOW - 3 * 60 * 60_000));
    state
        .0
        .lock()
        .unwrap()
        .stalls
        .push((45, Status::InProgress, NOW - 3 * 60 * 60_000));
    state
        .0
        .lock()
        .unwrap()
        .stalls
        .push((45, Status::InProgress, NOW - 3 * 60 * 60_000));
    let (u, board, sessions, _) = judge_usecase(
        vec![judge_row(45, Status::InProgress, "ses_dev", Slot::Dev)],
        Some(NOW - 61 * 60_000),
        false, // не idle: классический гейт 60 мин
        state,
        (Some(judge.clone() as Arc<dyn DecisionJudge>), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.judge_forced, 1, "паркинг гвардом");
    assert_eq!(judge.calls.lock().unwrap().len(), 0, "модель не вызывалась");
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "status#45=NeedsAttention"),
        "паркинг выполнен"
    );
    let _ = sessions;
}

#[tokio::test]
async fn judge_cap_verdict_parks_row_on_needs_attention() {
    // Кап-точка с судьёй: 4-й подряд rc — вердикт Needs attention →
    // паркинг с дайджестом, счётчик раундов сброшен.
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::NeedsAttention);
    let cfg = JudgeCfg {
        enabled: true,
        ..JudgeCfg::default()
    };
    let state = Arc::new(MemState(Mutex::default()));
    // кап-точка: тик видит смену ReviewResult → InProgress (4-й подряд rc)
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((46, Status::ReviewResult));
    state.0.lock().unwrap().review_rounds.push((46, 3));
    let (u, board, _, _) = judge_usecase(
        vec![judge_row(46, Status::InProgress, "ses_dev", Slot::Dev)],
        Some(1000),
        true,
        state,
        (Some(judge), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.judge_calls, 1, "судья вызван на кап-точке");
    assert_eq!(out.judge_transitions, 1, "паркинг вердиктом — перенос");
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "status#46=NeedsAttention"),
        "строка запаркована вердиктом"
    );
    assert_eq!(out.judge_forced, 0, "гвард не срабатывал (раунды 4 < 7)");
}

#[tokio::test]
async fn judge_stage_guard_stall_on_review_testing_uses_legacy_pings() {
    // Гард стадии, сталл-точка (#1413, раунд 3): ReviewTesting — вердикты
    // тест-ревьювера, не судьи; любой вердикт там гард отклоняет, вызов
    // сжёг бы API без легального ответа. Сталл тест-ревью-сессии при
    // включённом судье идёт по пингам старой механики.
    const NOW: u64 = 100_000_000;
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::NeedsAttention);
    let cfg = JudgeCfg {
        enabled: true,
        ..JudgeCfg::default()
    };
    let state = Arc::new(MemState(Mutex::default()));
    // статус уже виден тику (смены нет): иначе первое видение = вход в
    // стадию — «свежие глаза» чистят тест-ревью-слот до сталл-контура
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((52, Status::ReviewTesting));
    let (u, board, _, _) = judge_usecase(
        vec![judge_row(
            52,
            Status::ReviewTesting,
            "ses_tr",
            Slot::TestReview,
        )],
        Some(NOW - 90 * 60_000), // тишина 90 мин, ход идёт (idle=false)
        false,
        state,
        (Some(judge.clone() as Arc<dyn DecisionJudge>), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        judge.calls.lock().unwrap().is_empty(),
        "модель не вызывается на стадии вне полномочий судьи"
    );
    assert_eq!(out.judge_calls, 0, "счётчик вызовов судьи пуст");
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("comment#52=")),
        "пинг старой механики доставлен комментом"
    );
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("status#52=")),
        "статус не менялся (пинг, не паркинг)"
    );
}

#[tokio::test]
async fn judge_stage_guard_test_cap_uses_legacy_force_park() {
    // Гард стадии, кап-точка (#1413, раунд 3): цикл капа — стадия prev.
    // Пара ReviewTesting→Testing — fail-возвраты тест-ревьювера, не
    // request_changes; строка к тику уже в Testing (в матрице судьи),
    // поэтому гард — по prev: кап тест-цикла паркует старая механика
    // без вызова модели.
    const NOW: u64 = 100_000_000;
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::NeedsAttention);
    let cfg = JudgeCfg {
        enabled: true,
        ..JudgeCfg::default()
    };
    let state = Arc::new(MemState(Mutex::default()));
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((53, Status::ReviewTesting));
    state.0.lock().unwrap().review_rounds.push((53, 4));
    let (u, board, _, _) = judge_usecase(
        vec![judge_row(53, Status::Testing, "ses_test", Slot::Test)],
        Some(NOW), // активность свежая — сталл-контур молчит
        true,
        state,
        (Some(judge.clone() as Arc<dyn DecisionJudge>), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert!(
        judge.calls.lock().unwrap().is_empty(),
        "модель не вызывается на тест-капе"
    );
    assert_eq!(out.judge_calls, 0, "счётчик вызовов судьи пуст");
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "status#53=NeedsAttention"),
        "кап тест-цикла паркует старая механика"
    );
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("comment#53=⚠︎ Review-round cap")),
        "дайджест старой механики (не «⚖︎ Judge»)"
    );
    assert_eq!(
        out.review_flagged, 1,
        "эскалация посчитана после состоявшегося паркинга (Low раунда 3)"
    );
}

#[tokio::test]
async fn judge_cap_guard_forces_park_without_model_call() {
    // Гвард кап-точки: раунды CAP + escalate_after → паркинг БЕЗ вызова
    // модели (set_status + дайджест, счётчик сброс). Тик-уровень форс-ветки
    // (ревью раунда 2, L: домен judge_step был покрыт, механика тика — нет).
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::InProgress);
    let cfg = JudgeCfg {
        enabled: true,
        escalate_after: 3,
        ..JudgeCfg::default()
    };
    let state = Arc::new(MemState(Mutex::default()));
    // кап-переход: тик видит смену ReviewResult → InProgress при раундах 7
    // (CAP 4 + escalate_after 3 — форс-зона)
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((48, Status::ReviewResult));
    state.0.lock().unwrap().review_rounds.push((48, 7));
    let (u, board, sessions, _) = judge_usecase(
        vec![judge_row(48, Status::InProgress, "ses_dev", Slot::Dev)],
        Some(100_000_000), // активность свежая — сталл-контур молчит
        true,
        state,
        (Some(judge.clone() as Arc<dyn DecisionJudge>), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.judge_forced, 1, "форс-паркинг гвардом кап-точки");
    assert_eq!(
        judge.calls.lock().unwrap().len(),
        0,
        "модель не вызывалась — паркинг без вызова"
    );
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "status#48=NeedsAttention"),
        "строка запаркована"
    );
    assert!(
        board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("comment#48=⚖︎ Judge: cap-point")),
        "дайджест гварда в ленте"
    );
    assert!(
        sessions.prompts.lock().unwrap().is_empty(),
        "осиротевшая ревью-сессия не будится (ревью C3)"
    );
}

#[tokio::test]
async fn judge_cap_park_stale_race_check_aborts_mutations() {
    // Race-чек кап-паркинга (ревью раунда 4, Low): агентская транзакция
    // сменила статус строки за время контекста+вызова — вердикт-паркинг
    // не перетирает свежий переход: мутаций нет, счётчики паркинга не
    // насчитываются, раунды не сброшены (ретрай следующим count-переходом).
    let judge = Arc::new(HookJudge {
        hook: Mutex::new(None),
        verdict: uwuwu_domain::JudgeStatus::NeedsAttention,
    });
    let cfg = JudgeCfg {
        enabled: true,
        ..JudgeCfg::default()
    };
    let state = Arc::new(MemState(Mutex::default()));
    // кап-точка: тик видит смену ReviewResult → InProgress при раундах 3
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((46, Status::ReviewResult));
    state.0.lock().unwrap().review_rounds.push((46, 3));
    let (u, board, sessions, state) = judge_usecase(
        vec![judge_row(46, Status::InProgress, "ses_dev", Slot::Dev)],
        Some(100_000_000), // активность свежая — сталл-контур молчит
        true,
        state,
        (Some(judge.clone() as Arc<dyn DecisionJudge>), cfg),
    );
    // внутри decide(): статус строки уехал (агент успел перевести в Design)
    let board2 = Arc::clone(&board);
    *judge.hook.lock().unwrap() = Some(Box::new(move || {
        let mut rows = board2.rows.lock().unwrap();
        for t in rows.iter_mut() {
            if t.number() == 46 {
                *t = judge_row(46, Status::Design, "ses_dev", Slot::Dev);
            }
        }
    }));
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.judge_calls, 1, "вызов судьи состоялся");
    assert_eq!(
        out.judge_transitions, 0,
        "паркинг не состоялся — race-чек поймал stale"
    );
    assert_eq!(out.judge_forced, 0);
    let log = board.log.lock().unwrap();
    assert!(
        !log.iter().any(|l| l.starts_with("status#46=")),
        "статус не перетёрт: {log:?}"
    );
    assert!(
        !log.iter().any(|l| l.starts_with("comment#46=")),
        "дайджест паркинга не доставлен"
    );
    assert_eq!(
        state.0.lock().unwrap().review_rounds.first().map(|r| r.1),
        Some(4),
        "счётчик стоит на капе: инкремент count-перехода сохранён, сброс — только у состоявшегося паркинга"
    );
    assert!(
        sessions.prompts.lock().unwrap().is_empty(),
        "промптов при stale нет"
    );
}

#[tokio::test]
async fn judge_cap_verdict_continues_row_without_park() {
    // Кап-точка: вердикт «текущий статус» (пробуждение) — строка
    // продолжает работу, паркинга нет, раунды считаются дальше.
    let judge = FakeJudge::says(uwuwu_domain::JudgeStatus::InProgress);
    let cfg = JudgeCfg {
        enabled: true,
        ..JudgeCfg::default()
    };
    let state = Arc::new(MemState(Mutex::default()));
    // кап-точка: смена ReviewResult → InProgress при раундах 3 → 4-й подряд
    state
        .0
        .lock()
        .unwrap()
        .statuses
        .push((47, Status::ReviewResult));
    state.0.lock().unwrap().review_rounds.push((47, 3));
    let (u, board, sessions, _) = judge_usecase(
        vec![judge_row(47, Status::InProgress, "ses_dev", Slot::Dev)],
        Some(100_000_000), // активность свежая — сталл-контур молчит, тестируем кап
        true,
        state,
        (Some(judge.clone() as Arc<dyn DecisionJudge>), cfg),
    );
    let out = u.run(&plan(Mode::Live, 7)).await.unwrap();
    assert_eq!(out.judge_calls, 1, "судья вызван на кап-точке");
    assert_eq!(out.judge_forced, 0);
    assert!(
        !board
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "status#47=NeedsAttention"),
        "паркинга нет — судья сказал «ещё раунд»"
    );
    assert!(
        sessions.prompts.lock().unwrap().is_empty(),
        "кап-продолжение без промптов: review-слот очищен до вызова, будить нечего (ревью C3)"
    );
}
