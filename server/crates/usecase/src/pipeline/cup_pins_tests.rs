//! Пины CUP-гвардов сайдкаров (#1313, маршрут-матрица «кто входит в
//! кап»): dreamer/analyst/analyst_review держат запуск хода наполненным
//! капом. Гард срабатывает до чтения досок/дневных капов — доска и
//! state не трогаются. Отказ-семантика reachable-скана (#1450) здесь
//! не пинится — её закрывают router/crons/tick-тесты #1450.

use crate::pipeline::tick_tests::FixedClock;
use crate::pipeline::tick_tests::{empty_board, empty_tick_state, gateway_with_busy_sessions};
use crate::pipeline::{
    analyst::AnalystUseCase, analyst_review::AnalystReviewUseCase, dreamer::DreamerUseCase,
};
use crate::router::SessionRouter;
use crate::spawn_models::SpawnModels;
use std::sync::Arc;

/// Дример-спавн при полном капе откладывается (dreamer.held_cup).
#[tokio::test]
async fn dreamer_spawn_held_when_cup_full() {
    let dreamer = DreamerUseCase {
        sessions: Arc::new(SessionRouter::uniform(gateway_with_busy_sessions(2))),
        state: empty_tick_state(),
        clock: Arc::new(FixedClock),
        models: Arc::new(SpawnModels::canonical()),
        checkout: "/tmp/checkout".into(),
        cup: 1,
    };
    let spawned = dreamer.tick().await.unwrap();
    assert_eq!(spawned, 0, "кап полон — дример-спавн отложен");
}

/// Волна аналитики при полном капе откладывается (analys.held_cup).
#[tokio::test]
async fn analyst_wave_held_when_cup_full() {
    let analyst = AnalystUseCase {
        board: empty_board(),
        boards: Vec::new(),
        sessions: Arc::new(SessionRouter::uniform(gateway_with_busy_sessions(2))),
        state: empty_tick_state(),
        clock: Arc::new(FixedClock),
        models: Arc::new(SpawnModels::canonical()),
        cup: 1,
    };
    let spawned = analyst.tick().await.unwrap();
    assert_eq!(spawned, 0, "кап полон — волна аналитики отложена");
}

/// Ревью-волна при полном капе откладывается (analys_review.held_cup).
#[tokio::test]
async fn analyst_review_wave_held_when_cup_full() {
    let reviewer = AnalystReviewUseCase {
        board: empty_board(),
        boards: Vec::new(),
        sessions: Arc::new(SessionRouter::uniform(gateway_with_busy_sessions(2))),
        state: empty_tick_state(),
        clock: Arc::new(FixedClock),
        models: Arc::new(SpawnModels::canonical()),
        cup: 1,
    };
    let spawned = reviewer.tick().await.unwrap();
    assert_eq!(spawned, 0, "кап полон — ревью-волна отложена");
}
