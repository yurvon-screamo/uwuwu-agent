//! Тесты env-gc (#1431): матрица гардов прохода, вердикт-канал, батч,
//! observe. Фейки изолированы на тест (rules-test-rule); вердикт-канал
//! отвечает мгновенно — темп ожидания инфра-забота адаптера.

use super::env_gc::{sweep_envs, EnvGcCtx, EnvGcOutcome, EnvGcPolicy, ENV_GC_BATCH_CAP};
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
    commands: Mutex<Vec<String>>,
    /// $1 последней команды — первый токен args (резолв шаблона opencode:
    /// argsRegex сплитит arguments по пробелам, позиционный $1 = args[0]).
    /// Вердикт в ленте вычисляется по $1, не по маркеру сервера — так
    /// юниты ловят расхождение args-контракта (bug #1506: args
    /// «sweep <row>» делали $1 = "sweep", вердикт уходил с чужим row).
    arg1: Mutex<String>,
    /// Ключ → вердикт-глагол; ключ матчится к $1 args; «*» — дефолт.
    verdicts: Mutex<HashMap<String, String>>,
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
    async fn prompt(&self, _s: &str, _t: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn run_command(&self, _s: &str, _n: &str, args: &str) -> UsecaseResult<()> {
        self.commands.lock().unwrap().push(args.into());
        *self.arg1.lock().unwrap() = args
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        Ok(())
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
    async fn wait_user_message(
        &self,
        _s: &str,
        _marker: &str,
        _deadline_ms: u64,
    ) -> UsecaseResult<Option<String>> {
        // Лента содержит вердикт, отрезолвленный opencode'ом по $1 args
        // (вывод shell-блока шаблона); маркер сервера сюда не входит.
        // Пустая карта — None (путь дедлайна).
        let row = self.arg1.lock().unwrap().clone();
        let map = self.verdicts.lock().unwrap();
        Ok(map
            .get(&row)
            .or_else(|| map.get("*"))
            .filter(|_| !row.is_empty())
            .map(|v| format!("ENV-GC {row} {v}")))
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
        agent: String::new(),
        directory: directory.into(),
        updated_ms,
        outcome: outcome.into(),
        total_tokens: 0,
        busy: false,
    }
}

/// Носитель доски: row_status настраивается построчно (ревалидация
/// ревайва #1431); дефолт — Some(Done) для любого номера. Неиспользуемые
/// методы трейта — todo!(): проход их не зовёт, вызов = честная паника.
struct FakeBoard {
    statuses: Mutex<HashMap<u64, Status>>,
    row_status_fails: bool,
}

impl FakeBoard {
    fn all_done() -> Arc<Self> {
        Arc::new(Self {
            statuses: Mutex::new(HashMap::new()),
            row_status_fails: false,
        })
    }
    fn with_status(row: u64, status: Status) -> Arc<Self> {
        Arc::new(Self {
            statuses: Mutex::new(HashMap::from([(row, status)])),
            row_status_fails: false,
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
    async fn set_pr(
        &self,
        _board: &str,
        _number: u64,
        _pr_url: &uwuwu_domain::PrUrl,
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
    // Красный тест плана #1431: Done 3 суток, env жив, сессий нет →
    // проход диспатчит sweep, вердикт SWEPT, bootstrap гасится
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(gw.commands.lock().unwrap().as_slice(), ["1431"]);
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
async fn batch_cap_bounds_dispatch_per_pass() {
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks: Vec<Task> = (0..25)
        .map(|i| done_closed(2000 + i, THREE_DAYS + i, NOW))
        .collect();
    let out = run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(out.candidates, 25);
    assert_eq!(out.dispatched, ENV_GC_BATCH_CAP);
    assert_eq!(gw.commands.lock().unwrap().len(), ENV_GC_BATCH_CAP);
}

#[tokio::test]
async fn oldest_candidates_are_dispatched_first() {
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks = [
        done_closed(1432, THREE_DAYS, NOW),
        done_closed(1433, 10 * 86_400_000, NOW),
    ];
    run(&tasks, &gw, &FakeBoard::all_done(), NOW, false).await;
    assert_eq!(
        gw.commands.lock().unwrap().as_slice(),
        ["1433", "1432"],
        "старейшие первыми (путь клина)"
    );
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
async fn verdict_timeout_counts_the_row_failed() {
    // Фейк без вердиктов: канал отвечает None (дедлайн), строка — failed
    // (переедет следующим проходом)
    let gw = fake(Vec::new());
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
async fn foreign_and_absent_verdicts_are_counted() {
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
async fn status_probe_failure_skips_the_row_fail_safe() {
    // «Не знаю» ≠ «можно сносить» (ADR-058): недоступная проба статуса —
    // строка переезжает, диспатча нет
    let board = Arc::new(FakeBoard {
        statuses: Mutex::new(HashMap::new()),
        row_status_fails: true,
    });
    let gw = fake(Vec::new());
    verdict_default(&gw, "SWEPT");
    let tasks = [done_closed(1431, THREE_DAYS, NOW)];
    let out = run(&tasks, &gw, &board, NOW, false).await;
    assert_eq!(out.failed, 1);
    assert_eq!(out.dispatched, 0);
    assert!(gw.commands.lock().unwrap().is_empty());
}

// ---------- пин контракта «шаблон ↔ args» (#1506) ----------

/// Реальные шаблоны slash-команд. include_str! мёртвым путём —
/// compile-error, не тишина (канон mock_route_coverage).
const TASK_ENV_GC_TEMPLATE: &str = include_str!("../../../../../commands/task-env-gc.md");
const TASK_ENV_TEMPLATE: &str = include_str!("../../../../../commands/task-env.md");

/// Фейк выше эмулирует резолв opencode ($1 = первый токен args, слово
/// sweep литерально в шаблоне), но ни один юнит не читает реальный файл
/// шаблона — его правка мимо args-контракта (слово выкинуто, $1
/// подвинут) прошла бы все тесты и сломала канал вердиктов тем же
/// классом бага #1506, только на стороне шаблона.
#[test]
fn sweep_template_carries_subcommand_literally_and_row_via_arg1() {
    // shell-блок: скрипт получает sweep литералом, row — $1 (первый
    // токен args); контракт единый с эталонным task-env (args = row)
    assert!(
        TASK_ENV_GC_TEMPLATE.contains("task-env-gc.sh sweep $1"),
        "шаблон task-env-gc обязан нести «sweep $1»: подкоманда литерально в шаблоне, args = row (#1506)"
    );
    assert!(
        TASK_ENV_TEMPLATE.contains("task-env.sh $1"),
        "эталонный контракт task-env: args = row, без подкоманды в args"
    );
}
