//! E2E носитель→тик на MemoryHub (#1616): direct-флаг доезжает через
//! enrichment (carrier-запись state.fields мока), decide не спавнит
//! direct-строку; контрольная untracked-строка спавнится как раньше —
//! канон fast-track не задет. Пин синхронизации мока: без записи
//! state.fields в set_board_cells проекция read_board молча сбрасывает
//! флаг, и тик спавнит дубль (механизм инстанса true-washing #1611).

use std::sync::Arc;

use uwuwu_domain::{PeakWindow, Slot, SpawnPolicy, Status, UtcMinute};
use uwuwu_usecase::{
    BoardPlan, Clock, Mode, PrGateway, PrProbe, PrState, SessionRouter, SpawnModels, TaskBoardRepo,
    TickPlan, TickUseCase, UsecaseResult,
};

use crate::api::test_support::{StubSessions, StubState};
use crate::mockhub::MemoryHub;

struct FixedClock;

impl Clock for FixedClock {
    fn utc_minute(&self) -> UtcMinute {
        UtcMinute {
            minute_of_day: 0,
            weekday: 1,
        }
    }

    fn now_ms(&self) -> u64 {
        0
    }
}

struct StubPrs;

#[async_trait::async_trait]
impl PrGateway for StubPrs {
    async fn probe(
        &self,
        _project_repo: &str,
        _pr_url: Option<&str>,
        _branch: Option<&str>,
    ) -> UsecaseResult<PrProbe> {
        Ok(PrProbe {
            state: PrState::Missing,
            url: None,
        })
    }

    async fn merge(&self, _project_repo: &str, _pr_url: &str) -> UsecaseResult<()> {
        Ok(())
    }
}

fn plan() -> TickPlan {
    TickPlan {
        mode: Mode::Live,
        policy: SpawnPolicy {
            cup: 7,
            peak: PeakWindow::default(),
        },
        boards: vec![BoardPlan {
            project: "sandbox".into(),
            board: "Задачи sandbox".into(),
            repo: "o/r".into(),
            checkout: "/tmp/checkout".into(),
            env_root: "/tmp/env".into(),
        }],
    }
}

fn usecase(board: Arc<MemoryHub>) -> TickUseCase {
    TickUseCase {
        board,
        sessions: Arc::new(SessionRouter::uniform(Arc::new(StubSessions))),
        prs: Arc::new(StubPrs),
        clock: Arc::new(FixedClock),
        state: Arc::new(StubState),
        models: Arc::new(SpawnModels::canonical()),
        boot_ms: 0,
        judge: None,
        judge_cfg: Default::default(),
        quota: None,
    }
}

#[tokio::test]
async fn tick_never_spawns_direct_row_but_spawns_untracked_control() {
    let memory = Arc::new(MemoryHub::with_seed(Some((1, Status::InProgress))));
    // флаг direct через set_board_cells — как прод-носитель (REST-поле);
    // мутация материализует сид (ensure_seed ленив), next_number = 2
    memory
        .set_board_cells("Задачи sandbox", 1, None, None, Some(true), None)
        .await
        .expect("direct cell");
    let control = memory
        .create_issue(
            "Задачи sandbox",
            "sandbox",
            "контрольная untracked In Progress",
            "",
            None,
        )
        .await
        .expect("control row");
    assert_eq!(control, 2, "контрольная строка — №2, сид не задет");
    memory
        .set_status("Задачи sandbox", control, Status::InProgress)
        .await
        .expect("control In Progress");

    let out = usecase(memory.clone()).run(&plan()).await.expect("tick");

    // direct-строка: проекция носит флаг, дев-ячейка пуста — спавна нет
    let board = memory.read_board("Задачи sandbox").await.expect("board");
    let row1 = board.iter().find(|t| t.number() == 1).expect("row 1");
    assert!(row1.direct(), "проекция read_board несёт direct=true");
    let fields1 = memory.issue_fields(1).await.expect("fields 1");
    assert!(fields1.direct(), "enrichment видит флаг (state.fields)");
    assert!(
        fields1.session(Slot::Dev).is_none(),
        "дев-ячейка direct-строки пуста — спавна не было"
    );

    // контрольная untracked In Progress — канон fast-track жив
    assert_eq!(
        out.spawns_done, 1,
        "ровно один спавн — контрольная строка; direct не спавнится"
    );
    let fields2 = memory.issue_fields(control).await.expect("fields 2");
    assert!(
        fields2.session(Slot::Dev).is_some(),
        "контрольная строка получила исполнителя"
    );
}
