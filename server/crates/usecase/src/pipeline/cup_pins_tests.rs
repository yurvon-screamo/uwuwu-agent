//! Пины CUP-гвардов сайдкаров (#1313, маршрут-матрица «кто входит в
//! кап»): dreamer/analyst/analyst_review держат запуск хода наполненным
//! капом. Пины не вакуумные: у каждого есть контроль-ран на свободном
//! капе, где маршрут реально спавнит (Ok(1)) — при удалении CUP-гейта
//! held-ран краснеет. Отказ-семантика reachable (#1450) здесь не пинится
//! — её закрывают router/crons/tick-тесты #1450.

use crate::pipeline::tick_tests::FixedClock;
use crate::pipeline::tick_tests::{board_with_rows, empty_tick_state, gateway_with_busy_sessions};
use crate::pipeline::{
    analyst::AnalystUseCase, analyst_review::AnalystReviewUseCase, dreamer::DreamerUseCase,
    AnalystBoard,
};
use crate::ports::Clock;
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use std::sync::Arc;
use uwuwu_domain::{PeakWindow, Status, Task, UtcMinute};

/// Часы с большим now_ms — филлеры updated_ms=0 дозревают до idle-кандидатов
/// дримера (IDLE_MS 30 мин) при фиксированном времени.
struct DreamerClock;
impl Clock for DreamerClock {
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

/// Кандидат волны: строка нужного статуса с непустым тайтлом
/// (фильтры волн analyst.rs/analyst_review.rs; ⏰-семейство мимо).
fn wave_row(n: u64, status: Status) -> Task {
    Task::new(n, format!("волна #{n}"), status)
}

fn analyst_boards() -> Vec<AnalystBoard> {
    vec![AnalystBoard {
        project: "uwuwu".into(),
        board: "Задачи uwuwu".into(),
        checkout: "/tmp/checkout".into(),
    }]
}

fn dreamer(cup: usize) -> DreamerUseCase {
    DreamerUseCase {
        sessions: Arc::new(SessionRouter::uniform(gateway_with_busy_sessions(
            2,
            "succeeded",
        ))),
        state: empty_tick_state(),
        clock: Arc::new(DreamerClock),
        models: Arc::new(SpawnModels::canonical()),
        checkout: "/tmp/checkout".into(),
        cup,
    }
}

fn analyst_wave(cup: usize) -> AnalystUseCase {
    AnalystUseCase {
        board: board_with_rows(vec![wave_row(3500, Status::Analys)]),
        boards: analyst_boards(),
        sessions: Arc::new(SessionRouter::uniform(gateway_with_busy_sessions(
            if cup == 1 { 2 } else { 0 },
            "",
        ))),
        state: empty_tick_state(),
        clock: Arc::new(FixedClock),
        models: Arc::new(SpawnModels::canonical()),
        cup,
        peak: PeakWindow {
            enabled: false,
            start_min: 0,
            end_min: 0,
        },
        compact: None,
    }
}

fn analyst_review_wave(cup: usize) -> AnalystReviewUseCase {
    AnalystReviewUseCase {
        board: board_with_rows(vec![wave_row(3500, Status::ReviewAnalys)]),
        boards: analyst_boards(),
        sessions: Arc::new(SessionRouter::uniform(gateway_with_busy_sessions(
            if cup == 1 { 2 } else { 0 },
            "",
        ))),
        state: empty_tick_state(),
        clock: Arc::new(FixedClock),
        models: Arc::new(SpawnModels::canonical()),
        cup,
    }
}

/// Дример-спавн при полном капе откладывается (dreamer.held_cup);
/// контроль: свободный кап — дример реально дримит (не вакуум).
#[tokio::test]
async fn dreamer_spawn_held_when_cup_full() {
    // филлеры «succeeded»+токены проходят is_fresh_candidate — без гейта
    // дример бы задримил один из них, потому холд — заслуга гейта
    assert_eq!(
        dreamer(1).tick().await.unwrap(),
        0,
        "кап полон — дример отложен"
    );
    assert_eq!(
        dreamer(7).tick().await.unwrap(),
        1,
        "контроль: дример спавнит"
    );
}

/// Волна аналитики при полном капе откладывается (analys.held_cup).
#[tokio::test]
async fn analyst_wave_held_when_cup_full() {
    assert_eq!(
        analyst_wave(1).tick().await.unwrap(),
        0,
        "кап полон — волна аналитики отложена"
    );
    assert_eq!(
        analyst_wave(7).tick().await.unwrap(),
        1,
        "контроль: волна спавнит"
    );
}

/// Ревью-волна при полном капе откладывается (analys_review.held_cup).
#[tokio::test]
async fn analyst_review_wave_held_when_cup_full() {
    assert_eq!(
        analyst_review_wave(1).tick().await.unwrap(),
        0,
        "кап полон — ревью-волна отложена"
    );
    assert_eq!(
        analyst_review_wave(7).tick().await.unwrap(),
        1,
        "контроль: ревью-волна спавнит"
    );
}
