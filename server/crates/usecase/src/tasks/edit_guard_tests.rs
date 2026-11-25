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
    /// Счётчик fresh-ридов: пин квоты «один точечный GraphQL-рид
    /// на вызов edit» (Low дельта-раунда #1615 — двойной рид на
    /// идемпотентной отказной ветке).
    fresh_reads: Mutex<usize>,
    /// Транспортный отказ fresh-рида: пин fail-closed на отказной
    /// ветке канон-гарда (#1615 — fresh-сверка перед отказом).
    fresh_err: bool,
}

impl StaleBoard {
    fn task(number: u64, status: Status) -> Task {
        Task::new(number, format!("строка {number}"), status)
    }

    fn writes(&self) -> Vec<(u64, Status)> {
        self.writes.lock().unwrap().clone()
    }

    fn fresh_reads(&self) -> usize {
        *self.fresh_reads.lock().unwrap()
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
        *self.fresh_reads.lock().unwrap() += 1;
        if self.fresh_err {
            return Err(crate::error::UsecaseError::Board(format!(
                "row #{n}: fresh carrier read failed (fixture transport)"
            )));
        }
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
    async fn set_pr(&self, _: &str, _: u64, _: &uwuwu_domain::PrUrl) -> UsecaseResult<()> {
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

/// fact_track-строка (#1631): носитель несёт FactTrack=on — гард
/// открывает ребро ReviewResult → Review и закрывает входы в
/// тест-контур только этой строке.
struct FactTrackFields;

#[async_trait]
impl IssueFieldStore for FactTrackFields {
    async fn load(&self, _: u64) -> UsecaseResult<std::collections::HashMap<String, String>> {
        Ok(std::collections::HashMap::from([
            (crate::naming::F_PRIORITY.to_string(), "medium".to_string()),
            (crate::naming::F_AUTOMERGE.to_string(), "on".to_string()),
            (crate::naming::F_FACT_TRACK.to_string(), "on".to_string()),
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

/// Носитель без триаж-полей (Приоритет и Automerge пусты): пин гейта
/// полноты #1280 на свежем current — «волна выпустила строку с пустым
/// триажем» ловится, только если гейт видит свежий носитель, а не
/// кеш-вид (Common ревью тестов #1615).
struct EmptyFields;

#[async_trait]
impl IssueFieldStore for EmptyFields {
    async fn load(&self, _: u64) -> UsecaseResult<std::collections::HashMap<String, String>> {
        Ok(std::collections::HashMap::new())
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
    fn round_reentry_pending(&self, _: &str, _: u64) -> bool {
        false
    }
    fn set_round_reentry_pending(&self, _: &str, _: u64) {}
    fn clear_round_reentry_pending(&self, _: &str, _: u64) {}

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

    fn master_binding(&self, _: &str, _: &str) -> Option<(String, String, String)> {
        None
    }

    fn set_master_binding(&self, _: &str, _: &str, _: &str, _: &str, _: &str) {}

    fn master_compact_ms(&self, _: &str, _: &str) -> u64 {
        0
    }

    fn set_master_compact_ms(&self, _: &str, _: &str, _: u64) {}

    fn prune_master_role(&self, _: &str, _: &std::collections::HashSet<String>) -> bool {
        false
    }

    fn save(&self) -> UsecaseResult<()> {
        Ok(())
    }
}

fn writer(board: Arc<StaleBoard>) -> TaskWriteUseCase {
    writer_with(board, Arc::new(FilledFields))
}

/// Транспортный отказ fresh-рида — громкий fail-closed (контракт
/// #1235/#1236): ошибка транспорта доходит до вызывающего, не винит
/// носитель «строкой нет на доске», мутации нет, рид один. Возвращает
/// текст отказа — веткам с особой маской (канон-отказ на отказной
/// ветке #1615) добавить свой ассерт. Общий хелпер трёх транспортных
/// пинов (qlty similar-code дельты #1615).
async fn assert_fresh_transport_fail_closed(
    board: &Arc<StaleBoard>,
    number: u64,
    status: Status,
    context: &str,
) -> String {
    let err = edit_status(board, number, status).await.expect_err(context);
    let text = err.to_string();
    assert!(
        text.contains("fresh carrier read failed"),
        "транспортная ошибка дошла до агента, не проглочена: {text}"
    );
    assert!(
        !text.contains("is not on board"),
        "сбой транспорта не винит носитель «строкой нет на доске»: {text}"
    );
    assert!(
        board.writes().is_empty(),
        "мутации не должно быть: {:?}",
        board.writes()
    );
    assert_eq!(board.fresh_reads(), 1);
    text
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

/// Фикстура строки 11 с расщеплённой правдой (qlty similar-code дельты
/// #1615): (снапшот, носитель, кеш-вид — None = строки нет в лагающем
/// REST-листе), флаг транспортного отказа.
fn board(
    snapshot: Status,
    carrier: Status,
    cached: Option<Status>,
    fresh_err: bool,
) -> Arc<StaleBoard> {
    Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, snapshot)],
        carrier: vec![StaleBoard::task(11, carrier)],
        cached: cached
            .map(|s| vec![StaleBoard::task(11, s)])
            .unwrap_or_default(),
        writes: Mutex::new(vec![]),
        fresh_reads: Mutex::new(0),
        fresh_err,
    })
}

/// edit(status) с произвольным носителем полей — direct/Empty-пины
/// дельты #1615.
async fn edit_status_with(
    board: &Arc<StaleBoard>,
    fields: Arc<dyn IssueFieldStore>,
    status: Status,
) -> UsecaseResult<()> {
    writer_with(board.clone(), fields)
        .edit(
            "uwuwu",
            11,
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
        fresh_reads: Mutex::new(0),
        fresh_err: false,
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
        fresh_reads: Mutex::new(0),
        fresh_err: false,
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
        fresh_reads: Mutex::new(0),
        fresh_err: false,
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
        fresh_reads: Mutex::new(0),
        fresh_err: false,
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
        fresh_reads: Mutex::new(0),
        fresh_err: false,
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
async fn fact_track_row_finishes_to_review() {
    // #1631: fact_track-строка после код-ревью идёт сразу на приёмку —
    // ReviewResult → Review с флагом FactTrack=on на носителе.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::ReviewResult)],
        carrier: vec![StaleBoard::task(11, Status::ReviewResult)],
        cached: vec![StaleBoard::task(11, Status::ReviewResult)],
        writes: Mutex::new(vec![]),
        fresh_reads: Mutex::new(0),
        fresh_err: false,
    });
    writer_with(board.clone(), Arc::new(FactTrackFields))
        .edit(
            "uwuwu",
            11,
            TaskPatch {
                status: Some(Status::UserReview),
                ..Default::default()
            },
        )
        .await
        .expect("fact_track-ребро ReviewResult → Review открыто флагом");
    assert_eq!(board.writes(), vec![(11, Status::UserReview)]);
}

#[tokio::test]
async fn fact_track_row_cannot_enter_testing() {
    // #1631, негативы: тест-контур fact_track-строке закрыт — гард
    // режет и прямую отправку, и вердиктный вход; мутации нет.
    for from in [Status::InProgress, Status::ReviewResult] {
        let board = Arc::new(StaleBoard {
            snapshot: vec![StaleBoard::task(11, from)],
            carrier: vec![StaleBoard::task(11, from)],
            cached: vec![StaleBoard::task(11, from)],
            writes: Mutex::new(vec![]),
            fresh_reads: Mutex::new(0),
            fresh_err: false,
        });
        let res = writer_with(board.clone(), Arc::new(FactTrackFields))
            .edit(
                "uwuwu",
                11,
                TaskPatch {
                    status: Some(Status::Testing),
                    ..Default::default()
                },
            )
            .await;
        assert!(res.is_err(), "{from:?} → Testing закрыто флагом");
        assert!(
            board.writes().is_empty(),
            "мутации не должно быть: {:?}",
            board.writes()
        );
    }
}

#[tokio::test]
async fn ordinary_row_still_enters_testing() {
    // Без флага запреты #1631 не действуют — канон UC-01 побайтово:
    // обычная строка из ReviewResult уходит в Testing.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::ReviewResult)],
        carrier: vec![StaleBoard::task(11, Status::ReviewResult)],
        cached: vec![StaleBoard::task(11, Status::ReviewResult)],
        writes: Mutex::new(vec![]),
        fresh_reads: Mutex::new(0),
        fresh_err: false,
    });
    writer_with(board.clone(), Arc::new(FilledFields))
        .edit(
            "uwuwu",
            11,
            TaskPatch {
                status: Some(Status::Testing),
                ..Default::default()
            },
        )
        .await
        .expect("без флага ReviewResult → Testing легально");
}

#[tokio::test]
async fn row_status_absent_is_fail_closed() {
    // Строку сняли с доски: снапшот ещё знает её, носитель — нет.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Analys)],
        carrier: vec![],
        cached: vec![],
        writes: Mutex::new(vec![]),
        fresh_reads: Mutex::new(0),
        fresh_err: false,
    });
    let err = edit_status(&board, 11, Status::InProgress)
        .await
        .expect_err("None — fail-closed отказ");
    assert!(
        err.to_string().contains("is not on board"),
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
        fresh_reads: Mutex::new(0),
        fresh_err: false,
    });
    let err = edit_status(&board, 11, Status::ReviewAnalys)
        .await
        .expect_err("непарсящийся статус носителя — fail-closed отказ");
    assert!(
        err.to_string().contains("is unreadable"),
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
        fresh_reads: Mutex::new(0),
        fresh_err: false,
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
        fresh_reads: Mutex::new(0),
        fresh_err: false,
    });
    let err = edit_status(&board, 11, Status::Design)
        .await
        .expect_err("стейл-писец отвергнут свежим чтением носителя");
    assert!(
        err.to_string().contains("outside the pipeline canon"),
        "текст отказа: {err}"
    );
    assert!(
        board.writes().is_empty(),
        "мутации не должно быть: {:?}",
        board.writes()
    );
    // Один fresh-рид на вызов (Low дельта-раунда #1615): идемпотентная
    // ветка уже вернула свежий носитель — отказ по нему без второго рида.
    assert_eq!(board.fresh_reads(), 1);
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
        fresh_reads: Mutex::new(0),
        fresh_err: false,
    });
    edit_status(&board, 11, Status::ReviewResult)
        .await
        .expect("ребро In Progress → Ревью результата легально по свежему носителю");
    assert_eq!(board.writes(), vec![(11, Status::ReviewResult)]);
}

#[tokio::test]
async fn stale_cache_rejection_verifies_fresh_carrier_passes_legal_edge() {
    // #1615 (инстанс #1612, 25.11): wt-окно щита #1346 истекло, кеш-вид
    // (REST items-индекс, read-лаг листа минутного масштаба) всё ещё
    // отдаёт до-записный «Analys», носитель живёт «Review Analys».
    // Легальное ребро Review Analys → Todo гард отвергал по кеш-виду —
    // «Analys → Todo вне канона» три попытки подряд, вердикт ревьювера
    // висел в воздухе. Отказная ветка обязана сверить current со свежим
    // носителем (точечный GraphQL-рид #1437) и провести ребро по
    // свежему значению — инвариант #1235 на канон-пути: чтение current
    // гарда = носителю при любом возрасте последней статусной записи.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Analys)],
        carrier: vec![StaleBoard::task(11, Status::ReviewAnalys)],
        cached: vec![StaleBoard::task(11, Status::Analys)],
        writes: Mutex::new(vec![]),
        fresh_reads: Mutex::new(0),
        fresh_err: false,
    });
    edit_status(&board, 11, Status::Todo).await.expect(
        "ребро Review Analys → Todo легально по свежему носителю — кеш-стейл не отказывает (#1615)",
    );
    assert_eq!(board.writes(), vec![(11, Status::Todo)]);
    // Один fresh-рид: отказная сверка, кеш-вид Analys расходится с целью
    assert_eq!(board.fresh_reads(), 1);
}

#[tokio::test]
async fn unreadable_cache_view_falls_back_to_fresh_carrier() {
    // Нечитаемый кеш-вид (Low дельта-раунда #1615): свежесозданной строки
    // нет в лагающем REST-листе — row_status отвечает None, носитель
    // (GraphQL) живёт Review Analys. Прежний отказ «row #N is not on
    // board (no status on the carrier)» винил честный носитель за кеш-
    // миссу; гард различает fresh-ридом и ведёт ребро по носителю.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Analys)],
        carrier: vec![StaleBoard::task(11, Status::ReviewAnalys)],
        cached: vec![],
        writes: Mutex::new(vec![]),
        fresh_reads: Mutex::new(0),
        fresh_err: false,
    });
    edit_status(&board, 11, Status::Todo).await.expect(
        "ребро Review Analys → Todo проходит при нечитаемом кеш-виде — fresh решает (#1615)",
    );
    assert_eq!(board.writes(), vec![(11, Status::Todo)]);
    assert_eq!(board.fresh_reads(), 1);
}

#[tokio::test]
async fn divergent_rejection_text_names_fresh_carrier_not_cache_view() {
    // Квадрант «кеш-вид и носитель расходятся, носитель тоже запрещает»
    // (Low дельта-раунда #1615): текст отказа обязан называть свежий
    // носитель (NeedsAttention), не кеш-вид (Draft); расхождение видно
    // в событии board.guard_rejected, мутации нет.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Draft)],
        carrier: vec![StaleBoard::task(11, Status::NeedsAttention)],
        cached: vec![StaleBoard::task(11, Status::Draft)],
        writes: Mutex::new(vec![]),
        fresh_reads: Mutex::new(0),
        fresh_err: false,
    });
    let err = edit_status(&board, 11, Status::Design)
        .await
        .expect_err("NeedsAttention → Design запрещён носителем и кеш-видом");
    let text = err.to_string();
    assert!(
        text.contains("NeedsAttention"),
        "текст отказа по свежему носителю: {text}"
    );
    assert!(
        !text.contains("Draft"),
        "кеш-видимое значение не попадает в текст отказа: {text}"
    );
    assert!(
        board.writes().is_empty(),
        "мутации не должно быть: {:?}",
        board.writes()
    );
    assert_eq!(board.fresh_reads(), 1);
}

#[tokio::test]
async fn rejection_stands_when_fresh_carrier_confirms_stale_cache() {
    // Обратная сторона #1615: fresh-сверка подтвердила кеш-вид — носитель
    // правда в Analys, ребро Analys → Todo вне канона, отказ честный
    // (текст по свежему значению), мутации нет. Пин против обратного
    // перегиба: отказная сверка не открывает незаконные рёбра.
    let board = Arc::new(StaleBoard {
        snapshot: vec![StaleBoard::task(11, Status::Analys)],
        carrier: vec![StaleBoard::task(11, Status::Analys)],
        cached: vec![StaleBoard::task(11, Status::Analys)],
        writes: Mutex::new(vec![]),
        fresh_reads: Mutex::new(0),
        fresh_err: false,
    });
    let err = edit_status(&board, 11, Status::Todo)
        .await
        .expect_err("Analys → Todo вне канона — отказ по согласию кеш-вида и свежего носителя");
    assert!(
        err.to_string().contains("outside the pipeline canon"),
        "текст отказа: {err}"
    );
    assert!(
        board.writes().is_empty(),
        "мутации не должно быть: {:?}",
        board.writes()
    );
}

#[tokio::test]
async fn direct_edge_on_stale_cache_reads_flag_from_fresh_carrier() {
    // Common ревью тестов #1615: на ветке «кеш-вид отстал, свежий носитель
    // разрешает» direct-флаг добывается по свежему current, не по
    // кеш-виду. Кеш показывает Todo (ребро Todo → Review закрыто),
    // носитель живёт In Progress, строка direct (Direct=on): мутант
    // «флаг по кеш-виду» отвечает ложным отказом «In Progress → Review
    // вне канона» — регресс класса инстанса #1612 на ребре финализации
    // direct-задачи. Один fresh-рид на вызов.
    let board = board(Status::Todo, Status::InProgress, Some(Status::Todo), false);
    edit_status_with(&board, Arc::new(DirectFields), Status::UserReview)
        .await
        .expect(
            "direct-ребро In Progress → Review открыто по свежему носителю — кеш-стейл не отказывает",
        );
    assert_eq!(board.writes(), vec![(11, Status::UserReview)]);
    assert_eq!(board.fresh_reads(), 1);
}

#[tokio::test]
async fn fresh_current_feeds_analys_exit_gate_not_cache_view() {
    // Common ревью тестов #1615: edit() получает fresh current из
    // guard_canon — гейт полноты #1280 решает по нему. Кеш-вид отстал
    // (показывает Analys), носитель — Review Analys, триаж-поля на
    // носителе пусты: ребро Review Analys → Todo проходит, гейт «выход
    // из Analys» не срабатывает — строка на носителе уже не в Analys.
    // Мутант «вернуть кеш-вид» запускает гейт по кеш-виду и валит
    // легальный переход требованием «fill them in».
    let board = board(
        Status::Analys,
        Status::ReviewAnalys,
        Some(Status::Analys),
        false,
    );
    edit_status_with(&board, Arc::new(EmptyFields), Status::Todo)
        .await
        .expect("ребро Review Analys → Todo проходит — гейт полноты #1280 не смотрит в кеш-вид");
    assert_eq!(board.writes(), vec![(11, Status::Todo)]);
    assert_eq!(board.fresh_reads(), 1);
}

#[tokio::test]
async fn fresh_transport_failure_on_rejection_branch_fails_closed() {
    // Отказная ветка #1615 с упавшим транспортом: кеш-вид зовёт отказ,
    // fresh-сверка падает — edit обязан отказать ГРОМКО транспортной
    // ошибкой (контракт #1235/#1236: мутация либо применяется и видна
    // следующим чтением, либо громкая ошибка), не тихим пропуском и не
    // канон-отказом по непроверенному кеш-виду. Мутации нет.
    let board = board(
        Status::Analys,
        Status::ReviewAnalys,
        Some(Status::Analys),
        true,
    );
    let text = assert_fresh_transport_fail_closed(
        &board,
        11,
        Status::Todo,
        "транспортный отказ fresh-рида — громкий fail-closed отказ edit",
    )
    .await;
    assert!(
        !text.contains("outside the pipeline canon"),
        "канон-отказ по непроверенному кеш-виду маскирует сбой носителя: {text}"
    );
}

#[tokio::test]
async fn fresh_transport_failure_on_idempotent_branch_fails_closed() {
    // Low ревью тестов #1615: идемпотентная ветка guard_current
    // (cached == target) тоже платит fresh-рид (#1437) — транспортный
    // отказ доходит до вызывающего громкой ошибкой (fail-closed
    // #1235/#1236), не превращается в «row is not on board» (вина
    // честного носителя за сбой транспорта) и не маскируется кеш-видом.
    // Мутации нет.
    let board = board(
        Status::InProgress,
        Status::InProgress,
        Some(Status::InProgress),
        true,
    );
    assert_fresh_transport_fail_closed(
        &board,
        11,
        Status::InProgress,
        "транспортный отказ fresh-рида на идемпотентной ветке — громкий fail-closed",
    )
    .await;
}

#[tokio::test]
async fn fresh_transport_failure_on_unreadable_cache_branch_fails_closed() {
    // Low ревью тестов #1615: нечитаемый кеш-вид (строки нет в лагающем
    // REST-листе) уходит в fresh-рид — транспортный отказ и здесь громкий
    // (fail-closed #1235/#1236), не «row is not on board» по кеш-миссе.
    // Мутации нет.
    let board = board(Status::Analys, Status::ReviewAnalys, None, true);
    assert_fresh_transport_fail_closed(
        &board,
        11,
        Status::Todo,
        "транспортный отказ fresh-рида на нечитаемом кеш-виде — громкий fail-closed",
    )
    .await;
}
