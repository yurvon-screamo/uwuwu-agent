//! doctor на общем stateful wiremock-ядре (cmd::mock_kernel): проба
//! board-orphans — тела-сироты видны, чистая доска чиста, SQL-отказ — SKIP.
//! Exit-семантика наблюдения: находка-осадок не портит вердикт волны.

use crate::cmd::mock_kernel::{set_env, MockBoard, MockKernel, MockRow, MockScenario};
use crate::siyuan::SiYuan;
use crate::siyuan_doctor::{run, ProbeOutcome, ProbeVerdict};

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

/// Изоляция FS-пробы doctor'а от живого воркспейса: пустой tempdir → SKIP;
/// каталог удаляется на дропе (Guard держится до конца теста).
fn isolated_workspace() -> tempfile::TempDir {
    let ws = tempfile::tempdir().expect("tempdir");
    std::env::set_var("UWUWU_SIYUAN_WS", ws.path());
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
    assert_eq!(kind, "observed", "verdict board-orphans: {outcomes:?}");
    assert!(what.contains("тело без строки"), "{what}");
    assert!(what.contains("2026-11-08"), "{what}");
    assert!(!what.contains("привязанная задача"), "{what}");
    // exit-код здесь не ассертим: wave-проба теста видит живой docker
    // с реальными отказами ядра; семантика Observed покрыта юнит-тестом.
    std::env::remove_var("UWUWU_SIYUAN_WS");
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
    std::env::remove_var("UWUWU_SIYUAN_WS");
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
    std::env::remove_var("UWUWU_SIYUAN_WS");
}

/// Стаб .sy по живому набору ключей: ID, Spec, Type, Properties,
/// Children (Content у стаба истории нет вовсе).
fn write_history_stub(history: &std::path::Path, snapshot_dir: &str, doc_id: &str, title: &str) {
    write_history_stub_at(history, snapshot_dir, &["nb", "parents"], doc_id, title);
}

/// Стаб сноса с доской в цепочке пути (тело задачи — саб-док дока доски).
fn write_history_task_body_stub(
    history: &std::path::Path,
    snapshot_dir: &str,
    board_doc: &str,
    doc_id: &str,
    title: &str,
) {
    write_history_stub_at(history, snapshot_dir, &["nb", board_doc], doc_id, title);
}

fn write_history_stub_at(
    history: &std::path::Path,
    snapshot_dir: &str,
    chain: &[&str],
    doc_id: &str,
    title: &str,
) {
    let stub = format!(
        r#"{{"ID":"{doc_id}","Spec":1,"Type":"NodeDocument","Properties":{{"title":"{title}"}},"Children":[]}}"#
    );
    let mut dir = history.join(snapshot_dir);
    for part in chain {
        dir = dir.join(part);
    }
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(format!("{doc_id}.sy")), stub).expect("write");
}

/// Вердикт пробы history-deletes: (вид, текст). Паника, если пробы нет.
fn history_deletes_verdict(outcomes: &[ProbeOutcome]) -> (&'static str, String) {
    let outcome = outcomes
        .iter()
        .find(|o| o.name == "history-deletes")
        .expect("history-deletes outcome присутствует");
    match &outcome.verdict {
        ProbeVerdict::Clean => ("clean", String::new()),
        ProbeVerdict::Skipped(why) => ("skipped", why.clone()),
        ProbeVerdict::Degraded(why) => ("degraded", why.clone()),
        ProbeVerdict::Observed(what) => ("observed", what.clone()),
    }
}

/// Сносы из истории воркспейса попадают в OBSERVE: снос тела задачи (доска
/// в цепочке пути снапшота + живой реестр досок) — задачной группой с
/// тайтлом; снапшоты `-update` событиями сноса не считаются; exit-семантика
/// наблюдения — проба не портит общий прогон (полный run).
#[tokio::test]
#[serial_test::serial]
async fn history_deletes_probe_lists_kernel_removes_from_workspace_history() {
    let mut scenario = orphan_scenario();
    scenario.board_children = vec![(
        BOARD_HPATH,
        vec![(CHILD_BOUND, CHILD_BOUND_HPATH, "20261108000000")],
    )];
    let (_kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);
    let ws = isolated_workspace();
    let history = ws.path().join("history");
    write_history_task_body_stub(
        &history,
        "2026-11-08-021321-delete",
        BOARD_DOC,
        "20261104033524-4d37yk0",
        "SiYuan: таймер бэкапов воркспейса (rsync-restic)",
    );
    write_history_stub(
        &history,
        "2026-11-08-010616-update",
        "20261108000000-updatedoc",
        "не снос",
    );

    let sy = SiYuan::from_env_with_timeout(10).expect("client");
    let outcomes = run(&sy, "uwuwu", 30).await;
    let (kind, what) = history_deletes_verdict(&outcomes);
    assert_eq!(kind, "observed", "verdict history-deletes: {outcomes:?}");
    assert!(
        what.contains("сносы тел задач за окно: 1 — 20261104033524-4d37yk0 «SiYuan: таймер бэкапов воркспейса (rsync-restic)»"),
        "{what}"
    );
    assert!(
        !what.contains("прочий оборот"),
        "все события задачи, хвост опущен: {what}"
    );
    assert!(
        !what.contains("updatedoc"),
        "update-снапшот не снос: {what}"
    );
    std::env::remove_var("UWUWU_SIYUAN_WS");
}

/// Нет истории воркспейса (мульти-PC) — честный SKIP пробы.
#[tokio::test]
#[serial_test::serial]
async fn history_deletes_probe_skips_without_workspace_history() {
    let mut scenario = orphan_scenario();
    scenario.board_children = vec![(
        BOARD_HPATH,
        vec![(CHILD_BOUND, CHILD_BOUND_HPATH, "20261108000000")],
    )];
    let (_kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);
    let _ws = isolated_workspace(); // history/ в tempdir не создаётся

    let sy = SiYuan::from_env_with_timeout(10).expect("client");
    let outcomes = run(&sy, "uwuwu", 30).await;
    let (kind, why) = history_deletes_verdict(&outcomes);
    assert_eq!(kind, "skipped", "нет history/ — SKIP: {outcomes:?}");
    assert!(why.contains("нет истории ядра"), "{why}");
    std::env::remove_var("UWUWU_SIYUAN_WS");
}
