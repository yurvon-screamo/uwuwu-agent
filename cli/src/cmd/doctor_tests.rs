//! doctor на общем stateful wiremock-ядре (cmd::mock_kernel): проба
//! board-orphans — тела-сироты видны, чистая доска чиста, SQL-отказ — SKIP.
//! Exit-семантика наблюдения: находка-осадок не портит вердикт волны.

use crate::cmd::mock_kernel::{set_env, MockBoard, MockKernel, MockRow, MockScenario};
use crate::siyuan::SiYuan;
use crate::siyuan_doctor::{run, ProbeOutcome, ProbeVerdict};
use std::path::PathBuf;

const BOARD_DOC: &str = "doc-board";
const BOARD_AV: &str = "av-doctor";
const CHILD_BOUND: &str = "20261108000000-child1";
const CHILD_ORPHAN: &str = "20261108000000-child2";
const CHILD_LEGACY: &str = "20261108000000-child3";
const CHILD_BOUND_HPATH: &str = "/uwuwu/Задачи uwuwu/привязанная задача";
const CHILD_ORPHAN_HPATH: &str = "/uwuwu/Задачи uwuwu/тело без строки";
const CHILD_LEGACY_HPATH: &str = "/uwuwu/Задачи uwuwu/легаси без привязки";
const BOARD_HPATH: &str = "/uwuwu/Задачи uwuwu";

/// Сценарий: доска с одной привязанной строкой; дети доски задаются тестом.
fn orphan_scenario() -> MockScenario {
    MockScenario {
        notebook: "nb-projects",
        boards: vec![MockBoard {
            doc_id: BOARD_DOC,
            av_id: BOARD_AV,
            keys: vec![("key-title", "Primary Key"), ("key-status", "Status")],
        }],
        hpath_ids: vec![(BOARD_HPATH, vec![BOARD_DOC])],
        rows: vec![MockRow::new("20261108000000-row1", "привязанная задача")
            .with_status("In Progress")
            .with_doc(CHILD_BOUND)],
        ..MockScenario::default()
    }
}

/// Изоляция FS-пробы doctor'а от живого воркспейса: пустой WS → SKIP.
fn isolated_workspace() -> PathBuf {
    let ws = std::env::temp_dir().join(format!("uwuwu-doctor-test-{}", std::process::id()));
    std::fs::create_dir_all(&ws).expect("mkdir ws");
    std::env::set_var("UWUWU_SIYUAN_WS", &ws);
    ws
}

/// Вердикт пробы board-orphans: (вид, текст). Паника, если пробы нет.
fn board_orphans_verdict(outcomes: &[ProbeOutcome]) -> (&'static str, String) {
    let outcome = outcomes
        .iter()
        .find(|o| o.name == "board-orphans")
        .expect("board-orphans outcome присутствует");
    match &outcome.verdict {
        ProbeVerdict::Clean => ("clean", String::new()),
        ProbeVerdict::Skipped(why) => ("skipped", why.clone()),
        ProbeVerdict::Degraded(why) => ("degraded", why.clone()),
        ProbeVerdict::Observed(what) => ("observed", what.clone()),
    }
}

/// Дети доски без строки попадают в наблюдение с тайтлом и датой;
/// exit-код остаётся чистым — наблюдение не деградация.
#[tokio::test]
#[serial_test::serial]
async fn board_orphans_probe_lists_unbound_doc() {
    let mut scenario = orphan_scenario();
    scenario.board_children = vec![(
        BOARD_HPATH,
        vec![
            (CHILD_BOUND, CHILD_BOUND_HPATH, "20261108000000"),
            (CHILD_ORPHAN, CHILD_ORPHAN_HPATH, "20261108010134"),
        ],
    )];
    let (_kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);
    let _ws = isolated_workspace();

    let sy = SiYuan::from_env_with_timeout(10).expect("client");
    let outcomes = run(&sy, "uwuwu", 30).await;
    let (kind, what) = board_orphans_verdict(&outcomes);
    assert_eq!(kind, "observed", "верdict board-orphans: {outcomes:?}");
    assert!(what.contains("тело без строки"), "{what}");
    assert!(what.contains("2026-11-08"), "{what}");
    assert!(!what.contains("привязанная задача"), "{what}");
    // exit-код здесь не ассертим: wave-проба теста видит живой docker
    // с реальными отказами ядра; семантика Observed покрыта юнит-тестом.
}

/// Дети с тайтлами легаси/detached строк (без привязки) сиротами не считаются.
#[tokio::test]
#[serial_test::serial]
async fn board_orphans_probe_respects_title_of_detached_row() {
    let mut scenario = orphan_scenario();
    scenario
        .rows
        .push(MockRow::new("20261108000000-legacy", "легаси без привязки"));
    scenario.board_children = vec![(
        BOARD_HPATH,
        vec![
            (CHILD_BOUND, CHILD_BOUND_HPATH, "20261108000000"),
            (CHILD_LEGACY, CHILD_LEGACY_HPATH, "20261108010134"),
        ],
    )];
    let (_kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);
    let _ws = isolated_workspace();

    let sy = SiYuan::from_env_with_timeout(10).expect("client");
    let outcomes = run(&sy, "uwuwu", 30).await;
    let (kind, verdict) = board_orphans_verdict(&outcomes);
    assert_eq!(kind, "clean", "легаси-тайтл не сирота: {verdict}");
}

/// SQL-отказ выборки детей — честный SKIP пробы (не молчаливый Clean).
#[tokio::test]
#[serial_test::serial]
async fn board_orphans_probe_skips_on_sql_failure() {
    let mut scenario = orphan_scenario();
    scenario.board_children = vec![(
        BOARD_HPATH,
        vec![(CHILD_ORPHAN, CHILD_ORPHAN_HPATH, "20261108010134")],
    )];
    scenario.sql_children_fail = true;
    let (_kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);
    let _ws = isolated_workspace();

    let sy = SiYuan::from_env_with_timeout(10).expect("client");
    let outcomes = run(&sy, "uwuwu", 30).await;
    let (kind, why) = board_orphans_verdict(&outcomes);
    assert_eq!(kind, "skipped", "SQL-отказ — SKIP: {outcomes:?}");
    assert!(why.contains("SQL детей доски"), "{why}");
}
