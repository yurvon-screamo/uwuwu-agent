//! Ролевые пины деградации списка сессий (#1609): отказ одного исполнителя
//! не роняет волну analyst / analyst_review / dreamer — спавн идёт через
//! живого, событие `<role>.list_degraded` пишется. Уровень роутера уже
//! покрыт router_tests::reachable_union_degrades_when_executor_down — здесь
//! проверяется реакция самого юзкейса, а не слияние списков.
//! dreamer-тест дополнительно пинит отложенный prune при частичном зрении.

use super::degraded_list_support::{
    assert_degraded, dreamer_target, router, trap_events, DreamClock, RecordingGw,
};
use crate::pipeline::analyst::{AnalystBoard, AnalystUseCase};
use crate::pipeline::analyst_review::AnalystReviewUseCase;
use crate::pipeline::dreamer::DreamerUseCase;
use crate::pipeline::tick_tests::{board_with_rows, empty_tick_state, FixedClock};
use crate::ports::{DreamerMark, TickStateStore};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use std::sync::Arc;
use uwuwu_domain::{Status, Task};

fn boards() -> Vec<AnalystBoard> {
    vec![AnalystBoard {
        project: "uwuwu".into(),
        board: "Задачи uwuwu".into(),
        checkout: "/tmp/checkout".into(),
    }]
}

fn analyst(sessions: Arc<SessionRouter>) -> AnalystUseCase {
    AnalystUseCase {
        board: board_with_rows(vec![Task::new(
            3500,
            "волна аналитики".into(),
            Status::Analys,
        )]),
        boards: boards(),
        sessions,
        state: empty_tick_state(),
        clock: Arc::new(FixedClock),
        models: Arc::new(SpawnModels::canonical()),
        cup: 7,
    }
}

fn analyst_review(sessions: Arc<SessionRouter>) -> AnalystReviewUseCase {
    AnalystReviewUseCase {
        board: board_with_rows(vec![Task::new(
            3501,
            "волна ревью".into(),
            Status::ReviewAnalys,
        )]),
        boards: boards(),
        sessions,
        state: empty_tick_state(),
        clock: Arc::new(FixedClock),
        models: Arc::new(SpawnModels::canonical()),
        cup: 7,
        compact: None,
    }
}

fn dreamer(sessions: Arc<SessionRouter>, state: Arc<dyn TickStateStore>) -> DreamerUseCase {
    DreamerUseCase {
        sessions,
        state,
        clock: Arc::new(DreamClock),
        models: Arc::new(SpawnModels::canonical()),
        checkout: "/tmp/checkout".into(),
        cup: 7,
    }
}

#[tokio::test]
async fn analyst_wave_spawns_on_live_union_and_logs_degradation() {
    let live = RecordingGw::new(false, Vec::new());
    let (guard, records) = trap_events();
    let spawned = analyst(router(live.clone(), RecordingGw::new(true, Vec::new())))
        .tick()
        .await
        .expect("отказ чужого исполнителя не роняет волну");
    drop(guard);
    assert_eq!(spawned, 1, "волна ушла живому исполнителю");
    assert_eq!(live.created.lock().unwrap().len(), 1);
    assert_degraded(&records.lock().unwrap(), "analys.list_degraded");
}

#[tokio::test]
async fn analyst_review_wave_delivers_on_live_union_and_logs_degradation() {
    let live = RecordingGw::new(false, Vec::new());
    let (guard, records) = trap_events();
    let delivered = analyst_review(router(live.clone(), RecordingGw::new(true, Vec::new())))
        .tick()
        .await
        .expect("отказ чужого исполнителя не роняет ревью-волну");
    drop(guard);
    assert_eq!(delivered, 1, "ревью-волна ушла живому исполнителю");
    assert_eq!(live.prompted.lock().unwrap().len(), 1);
    assert_degraded(&records.lock().unwrap(), "analys_review.list_degraded");
}

#[tokio::test]
async fn dreamer_spawns_on_live_union_defers_prune_and_logs_degradation() {
    let live = RecordingGw::new(false, vec![dreamer_target()]);
    let state = empty_tick_state();
    state.mark_dreamer("ses_dead", DreamerMark::Forever);
    let (guard, records) = trap_events();
    let spawned = dreamer(
        router(live.clone(), RecordingGw::new(true, Vec::new())),
        state.clone(),
    )
    .tick()
    .await
    .expect("отказ чужого исполнителя не роняет дримера");
    drop(guard);
    assert_eq!(spawned, 1, "дример спавнится через живого");
    assert_eq!(live.created.lock().unwrap().len(), 1);
    assert_degraded(&records.lock().unwrap(), "dreamer.list_degraded");
    assert!(
        state.dreamer_mark("ses_dead").is_some(),
        "частичный скан не чистит записи — prune отложен"
    );
    // Контроль: при полном зрении та же запись уходит гигиеной.
    let healthy = empty_tick_state();
    healthy.mark_dreamer("ses_dead", DreamerMark::Forever);
    dreamer(
        router(live, RecordingGw::new(false, Vec::new())),
        healthy.clone(),
    )
    .tick()
    .await
    .unwrap();
    assert!(healthy.dreamer_mark("ses_dead").is_none());
}
