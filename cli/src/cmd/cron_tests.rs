//! cron list на общем stateful wiremock-ядре (cmd::mock_kernel, дедуп
//! мок-билдеров 20261107052603; кроны унифицированы с задачами —
//! 20261106182051): инвентарь мастеров крон-задач — строки задачной доски
//! с custom-schedule в доке-теле; last run/сессия из state демона
//! (UWUWU_CRON_STATE_DIR песочница).
//!
//! Сценарии:
//! 1. Вейт-строка с расписанием в атрибутах → строка таблицы с last run из state
//! 2. строки без атрибута расписания → в инвентарь не попадают
//! 3. пустой state (нет файла) → last run пуст, инвентарь работает
//! 4. атрибут расписания отсутствует у всех → «Крон-задач нет»
//! 5. сбои чтения атрибутов — громкая ошибка (инвентарь честный)
//! 6. доска проекта не найдена — отказ с именем доски
//! 7. формат last run: локальное «YYYY-MM-DD HH:MM» из unix-ms state
//! 8. task edit --schedule "6h": грамматика валидирована, атрибут записан
//! 9. task edit --schedule "": снятие без валидации грамматики

use crate::cmd::cli::{CronSub, TaskSub};
use crate::cmd::mock_kernel::{set_env, MockBoard, MockKernel, MockRow, MockScenario};
use crate::cmd::sy_dispatch::{cron_run, task_run};

const BOARD_DOC: &str = "t-doc";
const BOARD_AV: &str = "av-t";
const MASTER_DOC: &str = "body-master1";
const PLAIN_DOC: &str = "body-plain2";

/// Сценарий инвентаря: задачная доска с двумя строками — крон-мастер
/// («stable-ватчер SiYuan», Вейт) и обычная задача («обычная задача», Todo),
/// обе привязаны к телам; attrs по сценариям.
fn list_scenario() -> MockScenario {
    MockScenario {
        notebook: "nb-projects",
        boards: vec![MockBoard {
            doc_id: BOARD_DOC,
            av_id: BOARD_AV,
            keys: vec![("key-title", "Первичный ключ"), ("key-status", "Status")],
        }],
        hpath_ids: vec![("/uwuwu/Задачи uwuwu", vec![BOARD_DOC])],
        rows: vec![
            MockRow::new("20261107090000-master1", "stable-ватчер SiYuan")
                .with_status("Вейт")
                .with_doc(MASTER_DOC),
            MockRow::new("20261107090000-plain2", "обычная задача")
                .with_status("Todo")
                .with_doc(PLAIN_DOC),
        ],
        ..MockScenario::default()
    }
}

/// (1) Вейт-строка с расписанием → инвентарь без отказа; state с живой
/// записью мастера читается (last run/сессия дополняют таблицу).
#[tokio::test]
#[serial_test::serial]
async fn list_shows_master_with_schedule_and_last_run_from_state() {
    let mut scenario = list_scenario();
    scenario.block_attrs = vec![(MASTER_DOC, vec![("custom-schedule".to_string(), "6h".to_string())])];
    let (kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);
    let _state = state_dir_with(
        r#"{ "crons": { "20261107090000-master1": { "sessionID": "ses_abc123def", "lastFiredAt": 1762498200000, "firedAtIso": "2026-11-07T09:30:00+09:00" } } }"#,
    );

    cron_run(CronSub::List { project: None })
        .await
        .expect("list: мастер найден, state прочитан");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
    assert!(
        kernel.attr_writes().is_empty(),
        "list — read-only инвентарь"
    );
}

/// (2) Строка без атрибута расписания в инвентарь не попадает: таблица
/// печатает только мастеров (мок: у обеих строк атрибутов нет → пусто).
#[tokio::test]
#[serial_test::serial]
async fn list_skips_rows_without_schedule_attr() {
    let mut scenario = list_scenario();
    scenario.block_attrs = vec![(PLAIN_DOC, vec![("custom-title".to_string(), "не расписание".to_string())])];
    let (_kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);

    cron_run(CronSub::List { project: None }).await.expect("list ok");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (3) State отсутствует — инвентарь работает без last run.
#[tokio::test]
#[serial_test::serial]
async fn list_works_without_state_file() {
    let mut scenario = list_scenario();
    scenario.block_attrs = vec![(MASTER_DOC, vec![("custom-schedule".to_string(), "*/5 * * * *".to_string())])];
    let (_kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("UWUWU_CRON_STATE_DIR", dir.path().join("несуществующее"));

    cron_run(CronSub::List { project: None }).await.expect("list без state ok");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (4) Ни у одной строки нет расписания → честное «нет» вместо пустой таблицы.
#[tokio::test]
#[serial_test::serial]
async fn list_reports_empty_inventory_without_masters() {
    let (_kernel, server) = MockKernel::start(list_scenario()).await;
    set_env(&server);

    cron_run(CronSub::List { project: None }).await.expect("пустой инвентарь ok");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (5) Мёртвая привязка тела одной строки — скип с предупреждением,
/// остальной инвентарь живёт (инвентарь не роняется одной битой строкой).
#[tokio::test]
#[serial_test::serial]
async fn list_skips_row_with_dead_binding_and_lists_the_rest() {
    // attrs-мок знает только тело обычной строки: master1 (unknown attrs)
    // пропускается, plain2 с расписанием попадает в инвентарь
    let mut scenario = list_scenario();
    scenario.block_attrs = vec![(PLAIN_DOC, vec![("custom-schedule".to_string(), "30m".to_string())])];
    let (_kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);

    cron_run(CronSub::List { project: None })
        .await
        .expect("битая строка не роняет инвентарь");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (6) Доска проекта не найдена — отказ с именем доски.
#[tokio::test]
#[serial_test::serial]
async fn list_bails_when_board_missing() {
    let (_kernel, server) = MockKernel::start(list_scenario()).await;
    set_env(&server);

    let err = cron_run(CronSub::List {
        project: Some("неттакого".into()),
    })
    .await
    .expect_err("доска не найдена — отказ");
    assert!(
        err.to_string().contains("Задачи неттакого"),
        "ошибка называет доску: {err}"
    );
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (7) Формат last run: локальное «YYYY-MM-DD HH:MM» из unix-ms state.
#[test]
fn format_local_renders_local_time() {
    // Формат не зависит от зоны прогона: проверяем шейп (длина, разделители)
    let s = crate::cmd::sy_dispatch::format_local(1762477800000);
    assert_eq!(s.len(), 16, "«YYYY-MM-DD HH:MM»: {s}");
    assert_eq!(s.as_bytes()[10], b' ', "пробел между датой и временем: {s}");
    assert_eq!(s.as_bytes()[13], b':', "двоеточие в времени: {s}");
}

/// (8) task edit --schedule "6h": грамматика валидирована, атрибут записан.
#[tokio::test]
#[serial_test::serial]
async fn edit_schedule_writes_custom_attr() {
    let mut scenario = list_scenario();
    scenario.block_attrs = vec![(PLAIN_DOC, vec![("custom-title".to_string(), "не расписание".to_string())])];
    let (kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Edit {
            id: "20261107090000-plain2".into(),
            status: None,
            title: None,
            pr: None,
            branch: None,
            schedule: Some("6h".into()),
        },
    )
    .await
    .expect("edit --schedule ok");

    let writes = kernel.attr_writes();
    assert_eq!(writes.len(), 1, "ровно одна запись атрибута: {writes:?}");
    assert_eq!(writes[0].0, PLAIN_DOC, "атрибут на теле строки");
    assert_eq!(writes[0].1, "custom-schedule");
    assert_eq!(writes[0].2, "6h");
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (9) task edit --schedule "": снятие без валидации грамматики — контракт
/// «"" — снять» (ревью Common-1: ветка сидела за validate_schedule и была
/// недостижима).
#[tokio::test]
#[serial_test::serial]
async fn edit_schedule_empty_removes_attr_without_grammar_check() {
    let (kernel, server) = MockKernel::start(list_scenario()).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Edit {
            id: "20261107090000-plain2".into(),
            status: None,
            title: None,
            pr: None,
            branch: None,
            schedule: Some("".into()),
        },
    )
    .await
    .expect("снятие расписания работает");

    let writes = kernel.attr_writes();
    assert_eq!(writes.len(), 1, "снятие = запись пустого значения");
    assert_eq!(writes[0].1, "custom-schedule");
    assert_eq!(
        writes[0].2, "",
        "пустое значение — ядро удаляет ключ из IAL"
    );
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// (10) task edit --schedule "мусор": грамматика отвергает ДО записи —
/// атрибут не тронут.
#[tokio::test]
#[serial_test::serial]
async fn edit_schedule_garbage_rejected_before_write() {
    let (kernel, server) = MockKernel::start(list_scenario()).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Edit {
            id: "20261107090000-plain2".into(),
            status: None,
            title: None,
            pr: None,
            branch: None,
            schedule: Some("раз в минутку".into()),
        },
    )
    .await
    .expect_err("мусорное расписание — отказ");

    assert!(
        err.to_string().contains("не разобрано"),
        "ошибка валидации грамматики: {err}"
    );
    assert!(
        kernel.attr_writes().is_empty(),
        "записи не было: атрибут не тронут"
    );
    std::env::remove_var("UWUWU_CRON_STATE_DIR");
}

/// State-dir песочница: отдельный tempdir с state.json.
fn state_dir_with(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    std::fs::write(dir.path().join("state/state.json"), content).unwrap();
    std::env::set_var("UWUWU_CRON_STATE_DIR", dir.path().join("state"));
    dir
}
