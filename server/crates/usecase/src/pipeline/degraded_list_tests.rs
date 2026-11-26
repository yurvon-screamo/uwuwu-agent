//! Ролевые пины деградации списка сессий (#1609): отказ одного исполнителя
//! не роняет волну analyst / analyst_review / dreamer — спавн идёт через
//! живого, событие `<role>.list_degraded` пишется. Сверка с существующим
//! покрытием (ревью #1609): слияние списков — router_tests::
//! reachable_union_degrades_when_executor_down, поведение волны аналитика —
//! tick_tests::analyst_wave_degrades_on_partial_outage. Поэтому analyst-тест
//! здесь пинит только событие роли, не дублируя поведенческие ассерты.
//! dreamer-тест дополнительно пинит отложенный prune при частичном зрении.

#[cfg(target_os = "linux")]
use super::degraded_list_support::LAZY_INSTALL;
use super::degraded_list_support::{
    assert_degraded, dreamer_target, has_exact_field, is_exact_degraded, router, trap_events,
    RecordingGw,
};
use crate::pipeline::analyst::AnalystUseCase;
use crate::pipeline::analyst_review::AnalystReviewUseCase;
use crate::pipeline::cup_pins_tests::{analyst_boards, DreamerClock};
use crate::pipeline::dreamer::DreamerUseCase;
use crate::pipeline::tick_tests::{board_with_rows, empty_tick_state, FixedClock};
use crate::ports::{DreamerMark, TickStateStore};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use std::sync::Arc;
use uwuwu_domain::{PeakWindow, Status, Task};

fn analyst(sessions: Arc<SessionRouter>) -> AnalystUseCase {
    AnalystUseCase {
        board: board_with_rows(vec![Task::new(
            3500,
            "волна аналитики".into(),
            Status::Analys,
        )]),
        boards: analyst_boards(),
        sessions,
        state: empty_tick_state(),
        clock: Arc::new(FixedClock),
        models: Arc::new(SpawnModels::canonical()),
        cup: 7,
        // Ревью #1609 раунд 2: контракт #1607 — peak/compact обязательны;
        // безопасные дефолты (механика off) не влияют на деградацию list.
        peak: PeakWindow {
            enabled: false,
            start_min: 0,
            end_min: 0,
        },
        compact: None,
    }
}

fn analyst_review(sessions: Arc<SessionRouter>) -> AnalystReviewUseCase {
    AnalystReviewUseCase {
        board: board_with_rows(vec![Task::new(
            3501,
            "волна ревью".into(),
            Status::ReviewAnalys,
        )]),
        boards: analyst_boards(),
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
        clock: Arc::new(DreamerClock),
        models: Arc::new(SpawnModels::canonical()),
        checkout: "/tmp/checkout".into(),
        cup: 7,
    }
}

/// Поведение волны при частичном отказе уже пинится в
/// tick_tests::analyst_wave_degrades_on_partial_outage; здесь — только событие роли.
#[tokio::test]
async fn analyst_wave_logs_degradation() {
    let (guard, records) = trap_events();
    analyst(router(
        RecordingGw::new(false, Vec::new()),
        RecordingGw::new(true, Vec::new()),
    ))
    .tick()
    .await
    .expect("отказ чужого исполнителя не роняет волну");
    drop(guard);
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

/// Пинит точность сопоставления имени события: префиксный вариант
/// (`...list_degraded_v2`) и подстрока в чужом поле не удовлетворяют ассерт —
/// сдвиг имени события теперь ловится каждым ролевым пином.
#[test]
fn degraded_assert_requires_exact_event_name() {
    let good = ["event=dreamer.list_degraded | executors=[\"down\"]".to_string()];
    let quoted = ["event=\"dreamer.list_degraded\" | executors=[\"down\"]".to_string()];
    let suffix = ["event=dreamer.list_degraded_v2 | executors=[\"down\"]".to_string()];
    let superset = ["event=dreamer.list_degraded | executors=[\"down\", \"live\"]".to_string()];
    let decoy =
        ["event=other | msg=event=dreamer.list_degraded | executors=[\"down\"]".to_string()];
    assert!(is_exact_degraded(&good, "dreamer.list_degraded"));
    assert!(is_exact_degraded(&quoted, "dreamer.list_degraded"));
    assert!(!is_exact_degraded(&suffix, "dreamer.list_degraded"));
    assert!(!is_exact_degraded(&superset, "dreamer.list_degraded"));
    assert!(!is_exact_degraded(&decoy, "dreamer.list_degraded"));
}

/// Гонка ловушки (#1661): первый заход на callsite с чужого потока без
/// диспетчера в tracing-core 0.1.36 кэширует `Interest::never()`, после чего
/// то же событие на потоке ловушки молча теряется (tokio-rs/tracing#3611).
/// Свой callsite нужен, чтобы полный прогон не зарегистрировал его заранее.
/// В изоляции на старом `trap_events` (thread-local dispatcher) тест красный
/// всегда; в полном прогоне он красный только без соседней живой ловушки
/// (при двух диспетчерах интерес считается по реестру и гонки нет) — как
/// страж в полном прогоне слабый. На процесс-глобальном роутере с
/// `Interest::always()` зелёный везде; страж порядка — тест ниже.
fn trap_race_probe_emit() {
    tracing::info!(target: "trap_race", event = "trap.race_probe", "пин гонки ловушки");
}

#[test]
fn trap_survives_first_hit_on_other_thread() {
    let (guard, recs) = trap_events();
    std::thread::spawn(trap_race_probe_emit).join().unwrap();
    trap_race_probe_emit();
    drop(guard);
    let recs = recs.lock().unwrap();
    assert!(
        has_exact_field(&recs, "event", "trap.race_probe"),
        "ловушка потеряла своё событие: {recs:?}"
    );
}

/// Роутер должен стоять до старта тестовых потоков: ленивая установка открывает
/// окно «heal-pass → публикация», где первый заход соседа кэширует `never`.
/// Первый же тест с ловушкой ставит флаг — пин красный при любом порядке тестов.
#[cfg(target_os = "linux")]
#[test]
fn router_is_installed_before_test_threads() {
    let (_guard, _recs) = trap_events();
    assert!(
        !LAZY_INSTALL.load(std::sync::atomic::Ordering::SeqCst),
        "роутер поставлен лениво — окно установки открыто"
    );
}
