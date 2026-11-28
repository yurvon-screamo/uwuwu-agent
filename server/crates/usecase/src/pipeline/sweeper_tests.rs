//! Тесты клина холодного ящика (#1293): отбор, кап, бюджет-гайд,
//! observe-гейт, отказ одной строки не роняет волну.

use super::sweeper::{budget_floor, sweep_batch_cap, sweep_board, SweepPolicy};

/// Политика тест-прохода: NOW и порог из аргументов.
fn policy(now_ms: u64, age_days: u32, observe: bool) -> SweepPolicy {
    SweepPolicy {
        now_ms,
        age_days,
        observe,
    }
}
use crate::error::UsecaseResult;
use crate::ports::TaskBoardRepo;
use async_trait::async_trait;
use std::sync::Mutex;
use uwuwu_domain::{BranchName, Slot, Status, Task, TaskFields};

const DAY_MS: u64 = 86_400_000;
const NOW: u64 = 1_800_000_000_000;

/// Мок доски: журнал архиваций + настраиваемые бюджет/отказы.
struct SweepBoard {
    archived: Mutex<Vec<u64>>,
    budget: Mutex<Option<u64>>,
    fail_numbers: Vec<u64>,
    invalidations: Mutex<usize>,
}

impl SweepBoard {
    fn new(budget: Option<u64>) -> Self {
        Self {
            archived: Mutex::new(Vec::new()),
            budget: Mutex::new(budget),
            fail_numbers: Vec::new(),
            invalidations: Mutex::new(0),
        }
    }

    fn archived(&self) -> Vec<u64> {
        self.archived.lock().unwrap().clone()
    }

    fn invalidations(&self) -> usize {
        *self.invalidations.lock().unwrap()
    }
}

#[async_trait]
impl TaskBoardRepo for SweepBoard {
    async fn read_board(&self, _b: &str) -> UsecaseResult<Vec<Task>> {
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
    async fn row_status(&self, _b: &str, _n: u64) -> UsecaseResult<Option<Status>> {
        Ok(None)
    }
    async fn row_exists(&self, _b: &str, _n: u64) -> UsecaseResult<bool> {
        Ok(false)
    }
    async fn set_session(&self, _b: &str, _n: u64, _s: Slot, _ses: &str) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_branch(&self, _b: &str, _n: u64, _branch: &BranchName) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_pr(&self, _b: &str, _n: u64, _pr_url: &uwuwu_domain::PrUrl) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_board_cells(
        &self,
        _b: &str,
        _n: u64,
        _p: Option<&str>,
        _a: Option<bool>,
        _d: Option<bool>,
        _f: Option<bool>,
        _c: Option<&str>,
    ) -> UsecaseResult<()> {
        Ok(())
    }
    async fn clear_session(&self, _b: &str, _n: u64, _s: Slot) -> UsecaseResult<()> {
        Ok(())
    }
    async fn post_comment(&self, _n: u64, _body: &str) -> UsecaseResult<String> {
        Ok(String::new())
    }
    async fn close_issue(&self, _n: u64) -> UsecaseResult<()> {
        Ok(())
    }
    async fn latest_comments(&self, _n: u64) -> UsecaseResult<Vec<crate::ports::CommentRef>> {
        Ok(Vec::new())
    }
    async fn read_issue(&self, _b: &str, _n: u64) -> UsecaseResult<crate::ports::IssueView> {
        Err(crate::error::UsecaseError::Board("мок".into()))
    }
    async fn issue_fields(&self, _n: u64) -> UsecaseResult<TaskFields> {
        Ok(TaskFields::default())
    }
    async fn create_issue(
        &self,
        _b: &str,
        _p: &str,
        _t: &str,
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
    async fn patch_cells(&self, _n: u64, _mutate: crate::ports::FmMutator) -> UsecaseResult<()> {
        Ok(())
    }
    async fn close_issue_ext(&self, _n: u64, _np: bool) -> UsecaseResult<()> {
        Ok(())
    }
    async fn archive_row(&self, _b: &str, n: u64) -> UsecaseResult<()> {
        if self.fail_numbers.contains(&n) {
            return Err(crate::error::UsecaseError::Board(format!(
                "archive #{n} отказ (мок)"
            )));
        }
        self.archived.lock().unwrap().push(n);
        Ok(())
    }
    fn gql_budget_remaining(&self) -> Option<u64> {
        *self.budget.lock().unwrap()
    }
    async fn issues_with_title(&self, _t: &str) -> UsecaseResult<Vec<u64>> {
        Ok(Vec::new())
    }
    async fn replace_body(&self, _n: u64, _body: &str) -> UsecaseResult<()> {
        Ok(())
    }
    fn invalidate_snapshot(&self, _b: &str) {
        *self.invalidations.lock().unwrap() += 1;
    }
}

/// Строка-фабрика: терминальная Done, закрытая `days_ago` суток назад.
fn closed_done(number: u64, days_ago: u64) -> Task {
    let mut t = Task::new(number, format!("строка {number}"), Status::Done);
    t.set_closed_at(NOW - days_ago * DAY_MS);
    t
}

#[tokio::test]
async fn observe_logs_without_mutations() {
    // observe-гейт клина внутренний: кандидаты видны (счётчик), мутаций нет
    let board = SweepBoard::new(None);
    let tasks = vec![closed_done(1, 10), closed_done(2, 8), closed_done(3, 3)];
    let out = sweep_board(&board, "B", &tasks, policy(NOW, 7, true))
        .await
        .expect("observe проход");
    assert_eq!(out.candidates, 2, "строки 1 и 2 старше 7 суток");
    assert_eq!(out.archived, 0, "observe не мутирует");
    assert!(board.archived().is_empty());
}

#[tokio::test]
async fn live_archives_oldest_first_within_cap() {
    let board = SweepBoard::new(None);
    let mut tasks = vec![closed_done(100, 3)]; // свежая — не кандидат
    for i in 0..sweep_batch_cap() + 20 {
        tasks.push(closed_done(i as u64, 10 + i as u64 % 5));
    }
    let out = sweep_board(&board, "B", &tasks, policy(NOW, 7, false))
        .await
        .expect("live проход");
    assert_eq!(out.candidates, sweep_batch_cap() + 20);
    assert_eq!(out.archived, sweep_batch_cap(), "кап на проход");
    let archived = board.archived();
    assert_eq!(archived.len(), sweep_batch_cap());
    // uwuwu#1726: порядок «старейшие первыми» без привязки к значению капа:
    // возраст строки — 10 + i%5, самая старая — i%5==4, самая свежая — i%5==0.
    let total = sweep_batch_cap() + 20;
    let oldest = (0..total).find(|i| i % 5 == 4).expect("старая есть");
    let freshest = (0..total).rev().find(|i| i % 5 == 0).expect("свежая есть");
    assert!(
        archived.contains(&(oldest as u64)),
        "самая старая строка среди архивированных"
    );
    assert!(
        !archived.contains(&(freshest as u64)),
        "самая свежая строка не попала в первый батч"
    );
    assert!(
        !archived.contains(&0),
        "самая молодая из кандидатов — в конце"
    );
    // High-1 ревью #1293: свежий срез — ровно один на волну, не на строку
    assert_eq!(board.invalidations(), 1);
    assert_eq!(out.deferred_cap, 20, "не влезшие в кап учтены отдельно");
}

#[tokio::test]
async fn observe_does_not_invalidate_snapshot() {
    let board = SweepBoard::new(None);
    let tasks = vec![closed_done(1, 10)];
    sweep_board(&board, "B", &tasks, policy(NOW, 7, true))
        .await
        .expect("observe проход");
    assert_eq!(board.invalidations(), 0, "observe не мутирует кеш");
}

#[tokio::test]
async fn wave_without_archived_does_not_invalidate() {
    // бюджет-гайд сработал до первой мутации: кеш никто не портил
    let board = SweepBoard::new(Some(budget_floor() - 1));
    let tasks = vec![closed_done(1, 10)];
    sweep_board(&board, "B", &tasks, policy(NOW, 7, false))
        .await
        .expect("проход");
    assert_eq!(board.invalidations(), 0);
}

#[tokio::test]
async fn budget_floor_defers_rest_of_wave() {
    let board = SweepBoard::new(Some(budget_floor() - 1));
    let tasks = vec![closed_done(1, 10), closed_done(2, 12)];
    let out = sweep_board(&board, "B", &tasks, policy(NOW, 7, false))
        .await
        .expect("проход с низким бюджетом");
    assert_eq!(out.archived, 0);
    assert_eq!(out.deferred_budget, 2, "вся волна переезжает");
    assert!(board.archived().is_empty());
}

#[tokio::test]
async fn budget_none_passes_gate() {
    // носитель не сообщает бюджет — гард пропускает
    let board = SweepBoard::new(None);
    let tasks = vec![closed_done(1, 10)];
    let out = sweep_board(&board, "B", &tasks, policy(NOW, 7, false))
        .await
        .expect("проход без бюджета");
    assert_eq!(out.archived, 1);
}

#[tokio::test]
async fn single_row_failure_does_not_break_wave() {
    let mut board = SweepBoard::new(None);
    board.fail_numbers = vec![1];
    let tasks = vec![closed_done(1, 10), closed_done(2, 12)];
    let out = sweep_board(&board, "B", &tasks, policy(NOW, 7, false))
        .await
        .expect("отказ строки не роняет проход");
    assert_eq!(out.archived, 1);
    assert_eq!(out.failed, 1);
    assert_eq!(board.archived(), vec![2]);
}

#[tokio::test]
async fn empty_when_no_candidates() {
    let board = SweepBoard::new(None);
    let tasks = vec![
        closed_done(1, 3),
        Task::new(2, "актив".into(), Status::InProgress),
    ];
    let out = sweep_board(&board, "B", &tasks, policy(NOW, 7, false))
        .await
        .expect("пустой проход");
    assert_eq!(out, Default::default());
}
