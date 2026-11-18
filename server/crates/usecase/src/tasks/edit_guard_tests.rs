//! Гард канона edit(): свежий current с носителя, не со снапшотом (#1235).
//! Составной баг инцидента #1211: гард, сверяющийся со стэйл-снапшотом,
//! пропускал нелегальную перезапись как легальную — волна брала строку из
//! стэйл-пачки и переписывала реальный In Progress ребром Analys → Review
//! Analys. Здесь гард обязан читать row_status (носитель).

use crate::error::UsecaseResult;
use crate::ports::{
    Clock, CommentRef, FmMutator, IssueFieldStore, IssueView, TaskBoardRepo, TickStateStore,
};
use crate::tasks::write::{TaskPatch, TaskWriteUseCase, TaskWriter};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use uwuwu_domain::{BranchName, Slot, Status, Task, TaskFields, UtcMinute};

/// Фейк с расщеплённой правдой: снапшот (read_board) против носителя
/// (row_status/set_status) — воспроизведение составного бага #1235.
struct StaleBoard {
    snapshot: Vec<Task>,
    carrier: Vec<Task>,
    /// Журнал записей set_status: (number, status) — «мутации нет»-пины.
    writes: Mutex<Vec<(u64, Status)>>,
}

impl StaleBoard {
    fn task(number: u64, status: Status) -> Task {
        Task::new(number, format!("строка {number}"), status)
    }

    fn writes(&self) -> Vec<(u64, Status)> {
        self.writes.lock().unwrap().clone()
    }
}

#[async_trait]
impl TaskBoardRepo for StaleBoard {
    async fn read_board(&self, _board: &str) -> UsecaseResult<Vec<Task>> {
        Ok(self.snapshot.clone())
    }

    async fn set_status(&self, _b: &str, n: u64, s: Status) -> UsecaseResult<()> {
        self.writes.lock().unwrap().push((n, s));
        Ok(())
    }

    async fn row_status(&self, _b: &str, n: u64) -> UsecaseResult<Option<Status>> {
        Ok(self
            .carrier
            .iter()
            .find(|t| t.number() == n)
            .map(|t| t.status()))
    }
    async fn row_exists(&self, _b: &str, n: u64) -> UsecaseResult<bool> {
        Ok(self.carrier.iter().any(|t| t.number() == n))
    }

    async fn set_session(&self, _: &str, _: u64, _: Slot, _: &str) -> UsecaseResult<()> {
        Ok(())
    }

    async fn set_branch(&self, _: &str, _: u64, _: &BranchName) -> UsecaseResult<()> {
        Ok(())
    }
    async fn set_board_cells(
        &self,
        _b: &str,
        _n: u64,
        _p: Option<&str>,
        _a: Option<bool>,
    ) -> UsecaseResult<()> {
        Ok(())
    }

    async fn clear_session(&self, _: &str, _: u64, _: Slot) -> UsecaseResult<()> {
        Ok(())
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

    async fn read_issue(&self, _b: &str, n: u64) -> UsecaseResult<IssueView> {
        Ok(IssueView {
            task: Self::task(n, Status::Todo),
            body: String::new(),
            comments: vec![],
        })
    }

    async fn issue_fields(&self, _: u64) -> UsecaseResult<TaskFields> {
        Ok(TaskFields::default())
    }

    async fn create_issue(&self, _: &str, _: &str, _: &str, _: &str) -> UsecaseResult<u64> {
        Ok(1)
    }

    async fn rename_issue(&self, _: u64, _: &str) -> UsecaseResult<()> {
        Ok(())
    }

    async fn patch_cells(&self, _: u64, _: FmMutator) -> UsecaseResult<()> {
        Ok(())
    }

    async fn close_issue_ext(&self, _: u64, _: bool) -> UsecaseResult<()> {
        Ok(())
    }

    async fn replace_body(&self, _: u64, _: &str) -> UsecaseResult<()> {
        Ok(())
    }
}

/// Носитель полон (Приоритет + Automerge): гейт полноты #1280 не мешает
/// этим тестам — они про гарда канона переходов (#1235). Поведение самого
/// гейта (пустые/мусорные поля) — матрица analys_exit_gate_tests.
struct FilledFields;

#[async_trait]
impl IssueFieldStore for FilledFields {
    async fn load(&self, _: u64) -> UsecaseResult<std::collections::HashMap<String, String>> {
        Ok(std::collections::HashMap::from([
            (crate::naming::F_PRIORITY.to_string(), "medium".to_string()),
            (crate::naming::F_AUTOMERGE.to_string(), "on".to_string()),
        ]))
    }

    async fn store(&self, _: u64, _: &[(&str, Option<String>)]) -> UsecaseResult<()> {
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
        0
    }
}

/// edit() state/clock не касается — мёртвые заглушки под конструкцию.
struct NopState;

impl TickStateStore for NopState {
    fn last_comment_id(&self, _: &str, _: u64) -> Option<String> {
        None
    }

    fn last_seen_status(&self, _: &str, _: u64) -> Option<Status> {
        None
    }

    fn set_last_seen_status(&self, _: &str, _: u64, _: Status) {}

    fn set_last_comment_id(&self, _: &str, _: u64, _: &str) {}

    fn dreamer_done(&self, _: &str) -> bool {
        false
    }

    fn mark_dreamer_done(&self, _: &str) {}

    fn dreamer_day_spawns(&self, _: &str) -> u32 {
        0
    }

    fn mark_dreamer_spawn(&self, _: &str) {}

    fn last_spawn_ms(&self, _: &str, _: u64, _: Slot) -> Option<u64> {
        None
    }

    fn mark_spawn(&self, _: &str, _: u64, _: Slot, _: u64) {}

    fn stall_ping(&self, _: &str, _: u64) -> Option<(Status, u64, u32)> {
        None
    }

    fn mark_stall_ping(&self, _: &str, _: u64, _: Status, _: u64) {}

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

    fn spawn_loop(&self, _: &str, _: u64, _: Slot) -> Option<(Status, u32)> {
        None
    }

    fn set_spawn_loop(&self, _: &str, _: u64, _: Slot, _: Status, _: u32) {}

    fn reset_stale_spawn_loops(&self, _: &str, _: u64, _: Status) {}

    fn own_comment(&self, _: &str) -> bool {
        false
    }

    fn mark_own_comment(&self, _: &str, _: u64) {}

    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}

fn writer(board: Arc<StaleBoard>) -> TaskWriteUseCase {
    TaskWriteUseCase {
        board,
        fields: Arc::new(FilledFields),
        boards: vec![("uwuwu".into(), "B".into())],
        clock: Arc::new(FixedClock),
        state: Arc::new(NopState),
    }
}

async fn edit_status(board: &Arc<StaleBoard>, number: u64, status: Status) -> UsecaseResult<()> {
    writer(board.clone())
        .edit(
            "uwuwu",
            number,
            TaskPatch {
                status: Some(status),
                ..Default::default()
            },
        )
        .await
}

#[tokio::test]
async fn guard_reads_fresh_carrier_not_stale_snapshot() {
    // Снапшот отстаёт: строка реально в In Progress (approve ревьювера),
    // стэйл-вид доски всё ещё показывает Analys — как в #1211.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Analys)],
        carrier: vec![StaleBoard::task(11, Status::InProgress)],
        writes: Mutex::new(vec![]),
    });
    // Агент волны: task_edit(Review Analys). На свежем носителе ребро
    // In Progress → Review Analys вне канона — громкий отказ, мутации нет.
    let res = edit_status(&board, 11, Status::ReviewAnalys).await;
    assert!(
        res.is_err(),
        "нелегальная перезапись по стэйл-снапшоту должна быть громким отказом"
    );
    assert!(
        board.writes().is_empty(),
        "мутации не должно быть: {:?}",
        board.writes()
    );
}

#[tokio::test]
async fn legal_transition_on_fresh_status_writes() {
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Analys)],
        carrier: vec![StaleBoard::task(11, Status::Analys)],
        writes: Mutex::new(vec![]),
    });
    edit_status(&board, 11, Status::ReviewAnalys)
        .await
        .expect("ребро Analys → Review Analys легально");
    assert_eq!(board.writes(), vec![(11, Status::ReviewAnalys)]);
}

#[tokio::test]
async fn row_status_absent_is_fail_closed() {
    // Строку сняли с доски: снапшот ещё знает её, носитель — нет.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Analys)],
        carrier: vec![],
        writes: Mutex::new(vec![]),
    });
    let err = edit_status(&board, 11, Status::InProgress)
        .await
        .expect_err("None — fail-closed отказ");
    assert!(
        err.to_string().contains("не на доске"),
        "текст отказа: {err}"
    );
    assert!(board.writes().is_empty());
}

#[tokio::test]
async fn unparseable_carrier_status_is_fail_closed() {
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Analys)],
        carrier: vec![StaleBoard::task(11, Status::Empty)],
        writes: Mutex::new(vec![]),
    });
    let err = edit_status(&board, 11, Status::ReviewAnalys)
        .await
        .expect_err("непарсящийся статус носителя — fail-closed отказ");
    assert!(
        err.to_string().contains("не читается"),
        "текст отказа: {err}"
    );
    assert!(board.writes().is_empty());
}

#[tokio::test]
async fn idempotent_same_status_still_writes() {
    // from == to — легальная идемпотентная переустановка, поведение сохранено.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::InProgress)],
        carrier: vec![StaleBoard::task(11, Status::InProgress)],
        writes: Mutex::new(vec![]),
    });
    edit_status(&board, 11, Status::InProgress)
        .await
        .expect("идемпотентная переустановка легальна");
    assert_eq!(board.writes(), vec![(11, Status::InProgress)]);
}
