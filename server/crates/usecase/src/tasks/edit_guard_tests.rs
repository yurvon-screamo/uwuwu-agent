//! Гард канона edit(): свежий current с носителя, не со снапшотом (#1235).
//! Составной баг инцидента #1211: гард, сверяющийся со стэйл-снапшотом,
//! пропускал нелегальную перезапись как легальную — волна брала строку из
//! стэйл-пачки и переписывала реальный In Progress ребром Analys → Review
//! Analys. Здесь гард обязан читать row_status (носитель).

use crate::error::UsecaseResult;
use crate::ports::{
    Clock, CommentRef, DreamerMark, FmMutator, IssueFieldStore, IssueView, TaskBoardRepo,
    TickStateStore,
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
    /// Кеш-вид статуса (wt/индексный, может отставать от носителя):
    /// row_status читает его, fresh_row_status (#1437) — носитель.
    cached: Vec<Task>,
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

    async fn set_status(
        &self,
        _b: &str,
        n: u64,
        s: Status,
    ) -> UsecaseResult<crate::ports::StatusDelivery> {
        self.writes.lock().unwrap().push((n, s));
        Ok(crate::ports::StatusDelivery::Confirmed {
            actual: crate::naming::status_name(s).to_string(),
        })
    }

    async fn row_status(&self, _b: &str, n: u64) -> UsecaseResult<Option<Status>> {
        Ok(self
            .cached
            .iter()
            .find(|t| t.number() == n)
            .map(|t| t.status()))
    }

    async fn fresh_row_status(&self, _b: &str, n: u64) -> UsecaseResult<Option<Status>> {
        // #1437: свежее чтение носителя мимо кеш-вида — материал
        // идемпотентного гарда edit().
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
        _d: Option<bool>,
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

    async fn patch_cells(&self, _: u64, _: FmMutator) -> UsecaseResult<()> {
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

/// direct-строка (#1357): носитель несёт Direct=on — гард открывает
/// ребро In Progress → Review только этой строке.
struct DirectFields;

#[async_trait]
impl IssueFieldStore for DirectFields {
    async fn load(&self, _: u64) -> UsecaseResult<std::collections::HashMap<String, String>> {
        Ok(std::collections::HashMap::from([
            (crate::naming::F_PRIORITY.to_string(), "medium".to_string()),
            (crate::naming::F_AUTOMERGE.to_string(), "off".to_string()),
            (crate::naming::F_DIRECT.to_string(), "on".to_string()),
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

fn writer(board: Arc<StaleBoard>) -> TaskWriteUseCase {
    writer_with(board, Arc::new(FilledFields))
}

fn writer_with(board: Arc<StaleBoard>, fields: Arc<dyn IssueFieldStore>) -> TaskWriteUseCase {
    TaskWriteUseCase {
        board,
        fields,
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
        .map(|_| ())
}

#[tokio::test]
async fn guard_reads_fresh_carrier_not_stale_snapshot() {
    // Снапшот отстаёт: строка реально в In Progress (approve ревьювера),
    // стэйл-вид доски всё ещё показывает Analys — как в #1211.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Analys)],
        carrier: vec![StaleBoard::task(11, Status::InProgress)],
        cached: vec![StaleBoard::task(11, Status::InProgress)],
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
        cached: vec![StaleBoard::task(11, Status::Analys)],
        writes: Mutex::new(vec![]),
    });
    edit_status(&board, 11, Status::ReviewAnalys)
        .await
        .expect("ребро Analys → Review Analys легально");
    assert_eq!(board.writes(), vec![(11, Status::ReviewAnalys)]);
}

#[tokio::test]
async fn dev_sends_to_testing_from_in_progress() {
    // Прямая отправка на тест (#1349): дев-агент из In Progress ставит
    // «Тестирование» — легальное ребро канона, гард пропускает, запись
    // происходит; до #1349 гард резал ребро отказом «вне канона».
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::InProgress)],
        carrier: vec![StaleBoard::task(11, Status::InProgress)],
        cached: vec![StaleBoard::task(11, Status::InProgress)],
        writes: Mutex::new(vec![]),
    });
    edit_status(&board, 11, Status::Testing)
        .await
        .expect("ребро In Progress → Testing легально (прямая отправка на тест)");
    assert_eq!(board.writes(), vec![(11, Status::Testing)]);
}

#[tokio::test]
async fn direct_row_finishes_to_review() {
    // #1357: direct-строка финализируется агентом сам — In Progress →
    // Review с флагом Direct=on на носителе; мутация происходит.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::InProgress)],
        carrier: vec![StaleBoard::task(11, Status::InProgress)],
        cached: vec![StaleBoard::task(11, Status::InProgress)],
        writes: Mutex::new(vec![]),
    });
    writer_with(board.clone(), Arc::new(DirectFields))
        .edit(
            "uwuwu",
            11,
            TaskPatch {
                status: Some(Status::UserReview),
                ..Default::default()
            },
        )
        .await
        .expect("direct-ребро In Progress → Review открыто флагом");
    assert_eq!(board.writes(), vec![(11, Status::UserReview)]);
}

#[tokio::test]
async fn ordinary_row_cannot_skip_to_review() {
    // Без флага direct ребро In Progress → Review вне канона — «Ревью
    // результата»/«Тестирование» обязательны (регресс-пин канона).
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::InProgress)],
        carrier: vec![StaleBoard::task(11, Status::InProgress)],
        cached: vec![StaleBoard::task(11, Status::InProgress)],
        writes: Mutex::new(vec![]),
    });
    let res = writer_with(board.clone(), Arc::new(FilledFields))
        .edit(
            "uwuwu",
            11,
            TaskPatch {
                status: Some(Status::UserReview),
                ..Default::default()
            },
        )
        .await;
    assert!(res.is_err(), "без direct-флага ребро закрыто");
    assert!(
        board.writes().is_empty(),
        "мутации не должно быть: {:?}",
        board.writes()
    );
}

#[tokio::test]
async fn row_status_absent_is_fail_closed() {
    // Строку сняли с доски: снапшот ещё знает её, носитель — нет.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Analys)],
        carrier: vec![],
        cached: vec![],
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
        cached: vec![StaleBoard::task(11, Status::Empty)],
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
        cached: vec![StaleBoard::task(11, Status::InProgress)],
        writes: Mutex::new(vec![]),
    });
    edit_status(&board, 11, Status::InProgress)
        .await
        .expect("идемпотентная переустановка легальна");
    assert_eq!(board.writes(), vec![(11, Status::InProgress)]);
}

#[tokio::test]
async fn stale_writer_idempotent_reinstall_is_rejected_by_fresh_carrier() {
    // #1437, симптом 2 (фантомный Design): кеш-вид (wt/индекс) показывает
    // «Design», носитель живёт «Needs attention». Стейл-писец (повтор
    // старого вердикта пробуждённой сессией) зовёт task_edit(Design) —
    // гард обязан свериться со свежим носителем и отвергнуть ребро
    // Needs attention → Design, а не пропустить «переустановку»
    // Design → Design.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Design)],
        carrier: vec![StaleBoard::task(11, Status::NeedsAttention)],
        cached: vec![StaleBoard::task(11, Status::Design)],
        writes: Mutex::new(vec![]),
    });
    let err = edit_status(&board, 11, Status::Design)
        .await
        .expect_err("стейл-писец отвергнут свежим чтением носителя");
    assert!(
        err.to_string().contains("вне канона"),
        "текст отказа: {err}"
    );
    assert!(
        board.writes().is_empty(),
        "мутации не должно быть: {:?}",
        board.writes()
    );
}

#[tokio::test]
async fn stale_idempotent_lag_read_passes_as_legal_edge() {
    // Легальный retry после своего write-through: кеш показывает цель,
    // свежий носитель (в read-лаге) — прежний статус. Ребро
    // прежний → цель легально, повтор применяется — retry-путь
    // идемпотентных вызовов сохранён.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::InProgress)],
        carrier: vec![StaleBoard::task(11, Status::InProgress)],
        cached: vec![StaleBoard::task(11, Status::ReviewResult)],
        writes: Mutex::new(vec![]),
    });
    edit_status(&board, 11, Status::ReviewResult)
        .await
        .expect("ребро In Progress → Ревью результата легально по свежему носителю");
    assert_eq!(board.writes(), vec![(11, Status::ReviewResult)]);
}
