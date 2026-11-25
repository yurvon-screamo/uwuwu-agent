//! Тесты env-gc (#1431, батч #1526): матрица гардов прохода, одна
//! команда на проход, сбор вердиктов из батч-сообщения, observe. Фейки
//! изолированы на тест (rules-test-rule); вердикт-канал отвечает
//! мгновенно — темп ожидания инфра-забота адаптера.

use super::env_gc::{
    sweep_envs, EnvGcCtx, EnvGcOutcome, EnvGcPolicy, ENV_GC_BATCH_CAP, ENV_GC_VERDICT_DEADLINE_MS,
    TASK_ENV_GC_SCRIPT,
};
use crate::error::{UsecaseError, UsecaseResult};
use crate::ports::{
    AgentSessionGateway, AssistantReply, CommentRef, IssueView, Liveness, SessionInfo, SessionSpec,
    TaskBoardRepo,
};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uwuwu_domain::{BranchName, Slot, Status, Task, TaskFields};

const ENV_ROOT: &str = "/env";

#[derive(Default)]
struct FakeGcSessions {
    list: Vec<SessionInfo>,
    list_fails: bool,
    created: Mutex<Vec<String>>,
    deleted: Mutex<Vec<String>>,
    /// Полные args принятых команд («<row> <row> …», порядок диспатча).
    commands: Mutex<Vec<String>>,
    /// Последний budget_ms команды — юзкейс обязан носить бюджет прохода.
    last_budget_ms: Mutex<u64>,
    /// Команда не принята исполнителем (транспортный отказ).
    command_fails: Mutex<bool>,
    /// Ключ → вердикт-глагол; ключ матчится к токену args; «*» — дефолт.
    /// Отсутствие записи для строки = вердикта в батч-сообщении нет
    /// (parse-all: строка failed).
    verdicts: Mutex<HashMap<String, String>>,
    /// Переопределение текста вывода целиком (кейс «вывод без нужных
    /// вердикт-строк»); None — вывод собирается по verdicts.
    feed_override: Mutex<Option<String>>,
    /// Ok(None) — дедлайн shell (вывода нет).
    wait_none: Mutex<bool>,
}

fn fake(list: Vec<SessionInfo>) -> Arc<FakeGcSessions> {
    Arc::new(FakeGcSessions {
        list,
        ..FakeGcSessions::default()
    })
}

fn fake_failing_list() -> Arc<FakeGcSessions> {
    Arc::new(FakeGcSessions {
        list_fails: true,
        ..FakeGcSessions::default()
    })
}

/// Полная shell-команда батча (путь скрипта + sweep + строки).
fn sweep_command(rows: &[u64]) -> String {
    let list: Vec<String> = rows.iter().map(|r| r.to_string()).collect();
    format!("{TASK_ENV_GC_SCRIPT} sweep {}", list.join(" "))
}

fn verdict_on(gw: &FakeGcSessions, row: &str, verdict: &str) {
    gw.verdicts
        .lock()
        .unwrap()
        .insert(row.to_string(), verdict.to_string());
}

fn verdict_default(gw: &FakeGcSessions, verdict: &str) {
    verdict_on(gw, "*", verdict);
}

#[async_trait]
impl AgentSessionGateway for FakeGcSessions {
    async fn create_session(&self, _spec: &SessionSpec) -> UsecaseResult<String> {
        let mut created = self.created.lock().unwrap();
        let id = format!("ses_gc_{}", created.len() + 1);
        created.push(id.clone());
        Ok(id)
    }
    /// Slash-команда больше не используется каналом env-gc (прямой shell
    /// вместо task-env-gc #1526): заглушка, чтобы заметить случайный
    /// возврат к старому транспорту.
    async fn run_command(&self, _s: &str, _n: &str, _a: &str, _b: u64) -> UsecaseResult<()> {
        panic!("env-gc больше не диспатчит slash-команды — только run_shell")
    }
    async fn prompt(&self, _s: &str, _t: &str) -> UsecaseResult<()> {
        Ok(())
    }
    /// Прямой shell без LLM (#1526): POST /session/:id/shell, вывод —
    /// из shell-сообщения ленты; user-сообщений и assistant-поворотов
    /// нет. Токены args после литерала sweep — строки батча.
    async fn run_shell(
        &self,
        _s: &str,
        command: &str,
        budget_ms: u64,
    ) -> UsecaseResult<Option<String>> {
        *self.last_budget_ms.lock().unwrap() = budget_ms;
        self.commands.lock().unwrap().push(command.into());
        if *self.command_fails.lock().unwrap() {
            return Err(UsecaseError::Session("shell не исполнен (мок)".into()));
        }
        if *self.wait_none.lock().unwrap() {
            return Ok(None);
        }
        if let Some(feed) = self.feed_override.lock().unwrap().clone() {
            return Ok(Some(feed));
        }
        let map = self.verdicts.lock().unwrap();
        // Строки батча — токены команды после литерала sweep
        let rows: Vec<String> = command
            .split_whitespace()
            .skip_while(|t| *t != "sweep")
            .skip(1)
            .map(str::to_string)
            .collect();
        let mut lines: Vec<String> = Vec::new();
        for row in rows {
            if let Some(v) = map.get(&row).or_else(|| map.get("*")) {
                lines.push(format!("ENV-GC {row} {v}"));
            }
        }
        if lines.is_empty() {
            return Ok(None);
        }
        Ok(Some(lines.join("\n")))
    }
    async fn liveness(&self, _s: &str) -> Liveness {
        Liveness::Alive
    }
    async fn delete_session(&self, s: &str) -> UsecaseResult<()> {
        self.deleted.lock().unwrap().push(s.into());
        Ok(())
    }
    async fn list_sessions(&self) -> UsecaseResult<Vec<SessionInfo>> {
        if self.list_fails {
            return Err(UsecaseError::Session("исполнитель недоступен (мок)".into()));
        }
        Ok(self.list.clone())
    }
    async fn last_activity_ms(&self, _s: &str) -> UsecaseResult<Option<u64>> {
        Ok(None)
    }
    async fn last_assistant_reply(&self, _s: &str) -> UsecaseResult<Option<AssistantReply>> {
        Ok(None)
    }
    async fn export_session(&self, _s: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
}

fn session(id: &str, directory: &str, outcome: &str, updated_ms: u64) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        title: String::new(),
        agent: String::new(),
        directory: directory.into(),
        updated_ms,
        outcome: outcome.into(),
        total_tokens: 0,
        busy: false,
    }
}

/// Сессия листинга с титулом — для рипера bootstrap-сессий env-gc.
fn titled_session(id: &str, title: &str, busy: bool) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        title: title.into(),
        agent: String::new(),
        directory: String::new(),
        updated_ms: 0,
        outcome: String::new(),
        total_tokens: 0,
        busy,
    }
}

/// Носитель доски: row_status настраивается построчно; «счётчик ревайва»
/// — строка, чей второй вызов возвращает In Progress (ревайв между
/// pre-flight и перепробой). Неиспользуемые методы трейта — todo!():
/// проход их не зовёт, вызов = честная паника.
struct FakeBoard {
    statuses: Mutex<HashMap<u64, Status>>,
    row_status_fails: bool,
    /// Номер → на n-м вызове строки fresh_row_status возвращает In
    /// Progress (ревайв виден ТОЛЬКО живому чтению — пин L3/L-1:
    /// откат перепробы на row_status красит
    /// row_revived_between_cycles_is_dropped_from_batch).
    revive_on_call: Mutex<HashMap<u64, usize>>,
    /// Номер → с какого вызова (в обоих методах) статус-чтение возвращает
    /// Err — отказ жив и на pre-flight, и на перепробе.
    fail_from_call: Mutex<HashMap<u64, usize>>,
    calls: Mutex<HashMap<u64, usize>>,
}

impl FakeBoard {
    fn all_done() -> Arc<Self> {
        Arc::new(Self {
            statuses: Mutex::new(HashMap::new()),
            row_status_fails: false,
            revive_on_call: Mutex::new(HashMap::new()),
            fail_from_call: Mutex::new(HashMap::new()),
            calls: Mutex::new(HashMap::new()),
        })
    }
    fn with_status(row: u64, status: Status) -> Arc<Self> {
        Arc::new(Self {
            statuses: Mutex::new(HashMap::from([(row, status)])),
            row_status_fails: false,
            revive_on_call: Mutex::new(HashMap::new()),
            fail_from_call: Mutex::new(HashMap::new()),
            calls: Mutex::new(HashMap::new()),
        })
    }
    /// Строка жива на pre-flight (1-й вызов) и возрождена к перепробе
    /// (2-й и далее) — владельческий ревайв между циклами.
    fn with_revive_between_cycles(row: u64) -> Arc<Self> {
        Arc::new(Self {
            statuses: Mutex::new(HashMap::new()),
            row_status_fails: false,
            revive_on_call: Mutex::new(HashMap::from([(row, 2)])),
            fail_from_call: Mutex::new(HashMap::new()),
            calls: Mutex::new(HashMap::new()),
        })
    }
    /// Строка доступна на pre-flight (1-й вызов), перепроба отказывает
    /// (2-й и далее) — «не знаю» ≠ «можно сносить» на перепробе.
    fn with_fail_from_call(row: u64, from: usize) -> Arc<Self> {
        Arc::new(Self {
            statuses: Mutex::new(HashMap::new()),
            row_status_fails: false,
            revive_on_call: Mutex::new(HashMap::new()),
            fail_from_call: Mutex::new(HashMap::from([(row, from)])),
            calls: Mutex::new(HashMap::new()),
        })
    }
}

#[async_trait]
impl TaskBoardRepo for FakeBoard {
    async fn read_board(&self, _board: &str) -> UsecaseResult<Vec<Task>> {
        todo!("не используется проходом env-gc")
    }
    async fn set_status(
        &self,
        _board: &str,
        _number: u64,
        _status: Status,
    ) -> UsecaseResult<crate::ports::StatusDelivery> {
        todo!("не используется проходом env-gc")
    }
    async fn row_status(&self, _board: &str, number: u64) -> UsecaseResult<Option<Status>> {
        if self.row_status_fails {
            return Err(UsecaseError::Session("носитель недоступен (мок)".into()));
        }
        let mut calls = self.calls.lock().unwrap();
        let call = calls.entry(number).or_insert(0);
        *call += 1;
        let nth = *call;
        drop(calls);
        if let Some(&from) = self.fail_from_call.lock().unwrap().get(&number) {
            if nth >= from {
                return Err(UsecaseError::Session(
                    "перепроба статуса недоступна (мок)".into(),
                ));
            }
        }
        Ok(Some(
            *self
                .statuses
                .lock()
                .unwrap()
                .get(&number)
                .unwrap_or(&Status::Done),
        ))
    }
    /// Живое чтение с семантикой перепробы: ревайв-поле обрабатывается
    /// ТОЛЬКО здесь (L3/L-1 ревью кода — пин с критерием «откат
    /// env_gc/row.rs на row_status красит row_revived_between_cycles»).
    /// row_status выше ревайв не видит намеренно: кеш-проба его не
    /// возвращает.
    async fn fresh_row_status(&self, _board: &str, number: u64) -> UsecaseResult<Option<Status>> {
        let mut calls = self.calls.lock().unwrap();
        let call = calls.entry(number).or_insert(0);
        *call += 1;
        let nth = *call;
        drop(calls);
        if let Some(&from) = self.fail_from_call.lock().unwrap().get(&number) {
            if nth >= from {
                return Err(UsecaseError::Session(
                    "перепроба статуса недоступна (мок)".into(),
                ));
            }
        }
        if let Some(&from) = self.revive_on_call.lock().unwrap().get(&number) {
            if nth >= from {
                return Ok(Some(Status::InProgress));
            }
        }
        Ok(Some(
            *self
                .statuses
                .lock()
                .unwrap()
                .get(&number)
                .unwrap_or(&Status::Done),
        ))
    }
    async fn row_exists(&self, _board: &str, _number: u64) -> UsecaseResult<bool> {
        todo!("не используется проходом env-gc")
    }
    async fn set_session(
        &self,
        _board: &str,
        _number: u64,
        _slot: Slot,
        _session: &str,
    ) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
    async fn set_branch(
        &self,
        _board: &str,
        _number: u64,
        _branch: &BranchName,
    ) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
    async fn set_board_cells(
        &self,
        _board: &str,
        _number: u64,
        _priority: Option<&str>,
        _automerge: Option<bool>,
        _direct: Option<bool>,
    ) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
    async fn clear_session(&self, _board: &str, _number: u64, _slot: Slot) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
    async fn post_comment(&self, _number: u64, _body: &str) -> UsecaseResult<String> {
        todo!("не используется проходом env-gc")
    }
    async fn close_issue(&self, _number: u64) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
    async fn latest_comments(&self, _number: u64) -> UsecaseResult<Vec<CommentRef>> {
        todo!("не используется проходом env-gc")
    }
    async fn read_issue(&self, _board: &str, _number: u64) -> UsecaseResult<IssueView> {
        todo!("не используется проходом env-gc")
    }
    async fn issue_fields(&self, _number: u64) -> UsecaseResult<TaskFields> {
        todo!("не используется проходом env-gc")
    }
    async fn create_issue(
        &self,
        _board: &str,
        _project: &str,
        _title: &str,
        _body: &str,
        _kind_name: Option<&str>,
    ) -> UsecaseResult<u64> {
        todo!("не используется проходом env-gc")
    }
    async fn set_issue_type(&self, _number: u64, _kind_name: &str) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
    async fn rename_issue(&self, _number: u64, _title: &str) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
    async fn patch_cells(
        &self,
        _number: u64,
        _mutate: crate::ports::FmMutator,
    ) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
    async fn close_issue_ext(&self, _number: u64, _not_planned: bool) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
    async fn archive_row(&self, _board: &str, _number: u64) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
    async fn issues_with_title(&self, _title: &str) -> UsecaseResult<Vec<u64>> {
        todo!("не используется проходом env-gc")
    }
    async fn replace_body(&self, _number: u64, _body_md: &str) -> UsecaseResult<()> {
        todo!("не используется проходом env-gc")
    }
}

fn done_closed(number: u64, age_ms: u64, now: u64) -> Task {
    let mut t = Task::new(number, format!("t{number}"), Status::Done);
    t.set_closed_at(now.saturating_sub(age_ms));
    t
}

fn policy(now: u64, observe: bool) -> EnvGcPolicy {
    EnvGcPolicy {
        now_ms: now,
        age_days: 2,
        observe,
    }
}

async fn run(
    tasks: &[Task],
    gw: &Arc<FakeGcSessions>,
    board: &Arc<FakeBoard>,
    now: u64,
    observe: bool,
) -> EnvGcOutcome {
    let router = SessionRouter::uniform(gw.clone());
    let sessions = router.project("p");
    let ctx = EnvGcCtx {
        board_name: "Задачи uwuwu",
        checkout: "/checkout",
        env_root: ENV_ROOT,
        tasks,
        sessions,
        models: &SpawnModels::canonical(),
        board: board.as_ref(),
    };
    sweep_envs(&ctx, policy(now, observe))
        .await
        .expect("проход не падает")
}

const NOW: u64 = 1_800_000_000_000;
const THREE_DAYS: u64 = 3 * 86_400_000;

#[tokio::test]
async fn mature_done_row_dispatches_sweep_and_is_swept() {
    // Красный тест плана #1431 (батч #1526: одна строка = одна команда
    // с одним токеном): проход диспатчит sweep, вердикт SWEPT, bootstrap
    // гасится
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(
        gw.commands.lock().unwrap().as_slice(),
        [sweep_command(&[1431])],
        "прямой shell скрипта с батчем строк, без slash-команды"
    );
    assert_eq!(
        *gw.last_budget_ms.lock().unwrap(),
        ENV_GC_VERDICT_DEADLINE_MS,
        "юзкейс носит бюджет прохода в команде"
    );
    assert_eq!(out.swept, 1);
    assert_eq!(out.dispatched, 1);
    assert_eq!(
        gw.deleted.lock().unwrap().as_slice(),
        ["ses_gc_1"],
        "bootstrap-сессия гасится в конце прохода"
    );
}

#[tokio::test]
async fn row_closed_within_48h_is_never_a_candidate() {
    // Граница 47 ч против боевого порога 2 суток (#1431)
    let gw = fake(Vec::new());
    let tasks = [done_closed(1431, 47 * 3_600_000, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.candidates, 0);
    assert!(gw.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn non_terminal_row_is_not_a_candidate_even_with_old_closed_at() {
    let gw = fake(Vec::new());
    let mut active = Task::new(1431, "t1431".into(), Status::UserReview);
    active.set_closed_at(NOW - THREE_DAYS);
    let out = run(
        std::slice::from_ref(&active),
        &gw,
        &FakeBoard::all_done(),
        NOW,
        false,
    )
    .await;
    assert_eq!(out.candidates, 0);
    assert!(gw.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn live_session_in_env_holds_the_row() {
    // Гард 2: ход в полёте в env строки — строка переедет следующим тиком
    let gw = fake(vec![session("ses_live", "/env/1431", "", NOW - 60_000)]);
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.held_live, 1);
    assert_eq!(out.dispatched, 0);
    assert!(gw.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn live_session_in_repo_subdir_holds_the_row() {
    // Сессии стартуют в env-корне, агенты могут жить в repo — гард
    // закрывает оба пути
    let gw = fake(vec![session(
        "ses_live",
        "/env/1431/repo",
        "",
        NOW - 60_000,
    )]);
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.held_live, 1);
    assert_eq!(out.dispatched, 0);
}

#[tokio::test]
async fn finished_session_is_terminated_then_row_is_swept() {
    // Гард 3: незакрытая запись сессии с финалом гасится delete_session
    let gw = fake(vec![session(
        "ses_done",
        "/env/1431",
        "succeeded",
        NOW - 3_600_000,
    )]);
    verdict_default(&gw, "SWEPT");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert!(gw.deleted.lock().unwrap().contains(&"ses_done".to_string()));
    assert_eq!(out.swept, 1);
}

#[tokio::test]
async fn executor_scan_failure_makes_the_pass_a_noop() {
    // Гард 2/ADR-058: «не знаю» ≠ «можно сносить» — проход no-op
    let gw = fake_failing_list();
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.candidates, 1);
    assert_eq!(out.dispatched, 0);
    assert!(gw.created.lock().unwrap().is_empty());
    assert!(gw.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn observe_mode_counts_candidates_without_commands() {
    let gw = fake(Vec::new());
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, true).await;
    assert_eq!(out.candidates, 1);
    assert!(gw.created.lock().unwrap().is_empty());
    assert!(gw.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn batch_cap_bounds_dispatch_per_pass_in_one_command() {
    // 25 кандидатов, кап 20: одна команда со всеми 20 строками,
    // остаток переезжает следующим проходом (#1526)
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks: Vec<Task> = (0..25)
        .map(|i| done_closed(2000 + i, THREE_DAYS + i, NOW))
        .collect();
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.candidates, 25);
    assert_eq!(out.dispatched, ENV_GC_BATCH_CAP);
    let commands = gw.commands.lock().unwrap();
    assert_eq!(commands.len(), 1, "один shell на проход");
    let rows: Vec<&str> = commands[0]
        .split_whitespace()
        .skip_while(|t| *t != "sweep")
        .skip(1)
        .collect();
    assert_eq!(rows.len(), ENV_GC_BATCH_CAP);
}

#[tokio::test]
async fn oldest_candidates_are_dispatched_first_in_args() {
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks = [
        done_closed(1432, THREE_DAYS, NOW),
        done_closed(1433, 10 * 86_400_000, NOW),
    ];
    run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(
        gw.commands.lock().unwrap().as_slice(),
        [sweep_command(&[1433, 1432])],
        "старейшие первыми внутри shell-команды (путь клина)"
    );
}

#[tokio::test]
async fn prefix_row_ids_in_one_batch_do_not_cross_match() {
    // Префиксные row-id одного батча: «ENV-GC 143» не матчится в
    // «ENV-GC 1431 …» — точный матч по токенам
    let gw = fake(Vec::new());
    verdict_on(&gw, "143", "SWEPT");
    verdict_on(&gw, "1431", "ABSENT");
    let tasks = [
        done_closed(143, THREE_DAYS, NOW),
        done_closed(1431, THREE_DAYS + 1, NOW),
    ];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.swept, 1);
    assert_eq!(out.absent, 1);
    assert_eq!(out.failed, 0);
}

#[tokio::test]
async fn dirty_verdict_is_counted_without_sweep() {
    let gw = fake(Vec::new());
    verdict_on(&gw, "1431", "DIRTY");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.dirty, 1);
    assert_eq!(out.swept, 0);
}

#[tokio::test]
async fn norepo_verdict_is_counted_without_sweep() {
    let gw = fake(Vec::new());
    verdict_on(&gw, "1431", "NOREPO");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.norepo, 1);
    assert_eq!(out.swept, 0);
}

#[tokio::test]
async fn foreign_and_absent_verdicts_are_counted_in_one_batch() {
    let gw = fake(Vec::new());
    verdict_on(&gw, "1431", "FOREIGN");
    verdict_on(&gw, "1432", "ABSENT");
    let tasks = [
        done_closed(1432, THREE_DAYS, NOW),
        done_closed(1431, 10 * 86_400_000, NOW),
    ];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.foreign, 1);
    assert_eq!(out.absent, 1);
}

#[tokio::test]
async fn verdict_feed_without_first_row_verdict_counts_it_failed_others_ok() {
    // Сообщение село без вердикта первой строки: недостающая — failed,
    // остальные учтены (пин Design #1526, развилка 2)
    let gw = fake(Vec::new());
    verdict_on(&gw, "1432", "SWEPT");
    let tasks = [
        done_closed(1431, THREE_DAYS, NOW),
        done_closed(1432, THREE_DAYS + 1, NOW),
    ];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.dispatched, 2);
    assert_eq!(out.swept, 1);
    assert_eq!(out.failed, 1);
}

#[tokio::test]
async fn feed_without_any_needed_verdict_counts_all_failed() {
    // Ok(Some) без нужных вердикт-строк (чужой row): все failed — после
    // посадки сообщения новых не будет
    let gw = fake(Vec::new());
    *gw.feed_override.lock().unwrap() = Some("ENV-GC 999999 SWEPT".into());
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.dispatched, 1);
    assert_eq!(out.swept, 0);
    assert_eq!(out.failed, 1);
}

#[tokio::test]
async fn wait_deadline_counts_all_rows_failed() {
    // Ok(None) — страх-дедлайн: все failed (переедут следующим проходом)
    let gw = fake(Vec::new());
    *gw.wait_none.lock().unwrap() = true;
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.dispatched, 1);
    assert_eq!(out.failed, 1);
    assert_eq!(out.swept, 0);
    assert_eq!(
        gw.deleted.lock().unwrap().as_slice(),
        ["ses_gc_1"],
        "bootstrap гасится и после дедлайна"
    );
}

#[tokio::test]
async fn shell_transport_failure_fails_all_rows_without_dispatch() {
    // Транспортный отказ shell: скрипт не исполнен — все failed,
    // dispatched = 0, bootstrap гашен
    let gw = fake(Vec::new());
    *gw.command_fails.lock().unwrap() = true;
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.dispatched, 0);
    assert_eq!(out.failed, 1);
    assert_eq!(
        gw.deleted.lock().unwrap().as_slice(),
        ["ses_gc_1"],
        "bootstrap гасится и после отказа shell"
    );
}

#[tokio::test]
async fn revived_row_is_revalidated_and_not_swept() {
    // TOCTOU ревайва (#1431 ревью C1): строка возрождена владельцем
    // ПОСЛЕ снапшота прохода — свежая проба носителя отсекает снос
    let board = FakeBoard::with_status(1431, Status::InProgress);
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &board, NOW, false).await;
    assert_eq!(out.revoked, 1);
    assert_eq!(out.dispatched, 0);
    assert_eq!(out.swept, 0);
    assert!(gw.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn row_revived_between_cycles_is_dropped_from_batch() {
    // Ревайв между pre-flight и перепробой (ревью раунд 3): pre-flight
    // видел Done, перепроба видит In Progress — строка не входит в args
    let board = FakeBoard::with_revive_between_cycles(1431);
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &board, NOW, false).await;
    assert_eq!(out.revoked, 1);
    assert_eq!(out.dispatched, 0);
    assert!(gw.commands.lock().unwrap().is_empty());
    assert!(
        gw.created.lock().unwrap().is_empty(),
        "bootstrap не создаётся"
    );
}

#[tokio::test]
async fn no_ready_rows_means_no_bootstrap_and_no_command() {
    // Все строки revoked на pre-flight: bootstrap не создаётся, команды
    // нет (Low-6 ревью-4)
    let board = FakeBoard::with_status(1431, Status::InProgress);
    let gw = fake(Vec::new());
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &board, NOW, false).await;
    assert_eq!(out.revoked, 1);
    assert!(gw.created.lock().unwrap().is_empty());
    assert!(gw.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reaper_deletes_stale_bootstrap_of_same_board() {
    // Рипер протухших bootstrap (#1526): idle-сессия с титулом прохода
    // сносится, проход идёт дальше штатно
    let stale = titled_session("ses_stale_1", "env-gc Задачи uwuwu", false);
    let gw = fake(vec![stale]);
    verdict_default(&gw, "SWEPT");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.swept, 1);
    let deleted = gw.deleted.lock().unwrap();
    assert!(
        deleted.contains(&"ses_stale_1".to_string()),
        "протухший bootstrap снесён: {deleted:?}"
    );
    assert!(
        deleted.contains(&"ses_gc_1".to_string()),
        "bootstrap текущего прохода погашен штатно: {deleted:?}"
    );
}

#[tokio::test]
async fn reaper_keeps_busy_and_foreign_sessions() {
    // Рипер трогает только idle-сессии со своим титулом: занятая (чужой
    // живой проход) и чужая (другая доска/другой префикс) — целы
    let gw = fake(vec![
        titled_session("ses_busy_1", "env-gc Задачи uwuwu", true),
        titled_session("ses_other_1", "env-gc Задачи origa", false),
        titled_session("ses_manual_1", "ручная работа", false),
    ]);
    verdict_default(&gw, "SWEPT");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.swept, 1);
    let deleted = gw.deleted.lock().unwrap();
    assert!(
        !deleted.contains(&"ses_busy_1".to_string()),
        "занятая сессия не тронута: {deleted:?}"
    );
    assert!(
        !deleted.contains(&"ses_other_1".to_string()),
        "сессия другой доски не тронута: {deleted:?}"
    );
    assert!(
        !deleted.contains(&"ses_manual_1".to_string()),
        "чужая сессия не тронута: {deleted:?}"
    );
}

#[tokio::test]
async fn revalidate_failure_skips_the_row_fail_safe() {
    // Перепроба отказала при живом pre-flight (Low-5 ревью кода):
    // «не знаю» на перепробе — строка не входит в батч, команды нет
    let board = FakeBoard::with_fail_from_call(1431, 2);
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &board, NOW, false).await;
    assert_eq!(out.failed, 1);
    assert_eq!(out.dispatched, 0);
    assert!(gw.commands.lock().unwrap().is_empty());
    assert!(gw.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn status_probe_failure_skips_the_row_fail_safe() {
    // «Не знаю» ≠ «можно сносить» (ADR-058): недоступная проба статуса —
    // строка переезжает, диспатча нет
    let board = Arc::new(FakeBoard {
        statuses: Mutex::new(HashMap::new()),
        row_status_fails: true,
        revive_on_call: Mutex::new(HashMap::new()),
        fail_from_call: Mutex::new(HashMap::new()),
        calls: Mutex::new(HashMap::new()),
    });
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &board, NOW, false).await;
    assert_eq!(out.failed, 1);
    assert_eq!(out.dispatched, 0);
    assert!(gw.commands.lock().unwrap().is_empty());
}

// ---------- пин контракта вызова скрипта (без slash-команды) ----------

#[test]
fn sweep_shell_command_carries_literal_sweep_prefix() {
    // Прямой shell вместо slash-команды task-env-gc (вырезана #1526 —
    // её user-сообщение плодило мусорные сессии): путь скрипта +
    // литерал sweep + строки батча; путь не зависит от доски.
    let cmd = sweep_command(&[1431, 1432]);
    assert!(
        cmd.starts_with(super::env_gc::TASK_ENV_GC_SCRIPT),
        "команда начинается с пути скрипта сноса"
    );
    assert!(
        cmd.ends_with(" sweep 1431 1432"),
        "строки батча идут после литерала sweep в порядке диспатча, через пробел: {cmd}"
    );
}
