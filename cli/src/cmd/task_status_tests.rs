//! Dispatch-интеграция task edit --status: whitelist канона статусов
//! (задача 20261109005123). Юнит validate_status живёт в siyuan_task.rs;
//! здесь граница dispatch → edit → mSelect: в клетку уходит канон-форма,
//! не эхо ввода (ядро апсертит mSelect-опцию по точному content — эхо
//! минтило бы дубль-опцию другого регистра), а не-канон отвергается ДО
//! записи. Прецеденты рига — goal_tests (edit_canonicalizes_status...),
//! cron_tests (edit_schedule_garbage_rejected_before_write).

use crate::cmd::cli::TaskSub;
use crate::cmd::mock_kernel::{set_env, MockBoard, MockKernel, MockRow, MockScenario};
use crate::cmd::sy_dispatch::task_run;
use serde_json::json;

const BOARD_DOC: &str = "st-doc";
const BOARD_AV: &str = "av-st";
const ROW_ITEM: &str = "20261109000000-rowstat";
const ROW_DOC: &str = "body-rowstat";

/// Задачная доска uwuwu с колонкой Status и одной строкой (Todo).
fn edit_scenario() -> MockScenario {
    MockScenario {
        notebook: "nb-projects",
        boards: vec![MockBoard {
            doc_id: BOARD_DOC,
            av_id: BOARD_AV,
            keys: vec![("key-title", "Первичный ключ"), ("key-status", "Status")],
        }],
        hpath_ids: vec![("/uwuwu/Задачи uwuwu", vec![BOARD_DOC])],
        rows: vec![MockRow::new(ROW_ITEM, "таска")
            .with_status("Todo")
            .with_doc(ROW_DOC)],
        ..MockScenario::default()
    }
}

fn edit(status: Option<&str>) -> TaskSub {
    TaskSub::Edit {
        id: ROW_ITEM.into(),
        status: status.map(String::from),
        title: None,
        pr: None,
        branch: None,
        r#type: None,
        schedule: None,
        agent: None,
        planned: None,
    }
}

/// Главный путь: lowercase-форма канонизируется ДО записи — в mSelect уходит
/// канон-форма «Ревью плана», не эхо ввода «ревью плана» (ядро апсертит
/// mSelect-опцию по точному content — эхо минтило бы дубль-опцию другого
/// регистра). «Ревью результата» как носитель канон-проверки не годится —
/// на нём сидит гард мержабельности (review_gate, свои тесты ниже).
#[tokio::test]
#[serial_test::serial]
async fn edit_status_writes_canonical_form_not_echo() {
    let (kernel, server) = MockKernel::start(edit_scenario()).await;
    set_env(&server);

    task_run("uwuwu", edit(Some("ревью плана")))
        .await
        .expect("lowercase-форма канона проходит");

    let writes = kernel.item_updates();
    assert_eq!(writes.len(), 1, "ровно одна запись клетки: {writes:?}");
    let (av, key, item, value) = &writes[0];
    assert_eq!(av, BOARD_AV);
    assert_eq!(key, "key-status");
    assert_eq!(item, ROW_ITEM);
    assert_eq!(
        value,
        &json!({ "mSelect": [{ "content": "Ревью плана" }] }),
        "в клетку уходит канон-форма, не эхо ввода"
    );
}

/// Гард ревью: перевод в «Ревью результата» с пустой PR-клеткой — отказ
/// с рецептом до любых записей (gh при этом не вызывается — клетки нет).
#[tokio::test]
#[serial_test::serial]
async fn edit_status_review_result_empty_pr_rejected_before_write() {
    let (kernel, server) = MockKernel::start(edit_scenario()).await;
    set_env(&server);

    let err = task_run("uwuwu", edit(Some("Ревью результата")))
        .await
        .expect_err("перевод без PR отвергается");

    let msg = err.to_string();
    assert!(msg.contains("без PR"), "суть отказа: {msg}");
    assert!(msg.contains("gh pr create"), "рецепт добивки: {msg}");
    assert!(
        kernel.item_updates().is_empty(),
        "гард до записи: доска не тронута"
    );
    assert_eq!(
        kernel.rows()[0].status,
        "Todo",
        "клетка статуса не изменилась"
    );
}

/// Одновременное снятие PR (--pr "") с переводом в «Ревью результата» —
/// отказ: вход в ревью требует живой PR-клетки, а не её очистки.
#[tokio::test]
#[serial_test::serial]
async fn edit_status_review_result_pr_clearing_rejected() {
    let (kernel, server) = MockKernel::start(edit_scenario()).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Edit {
            id: ROW_ITEM.into(),
            status: Some("Ревью результата".into()),
            title: None,
            pr: Some("".into()),
            branch: None,
            r#type: None,
            schedule: None,
            agent: None,
            planned: None,
        },
    )
    .await
    .expect_err("снятие PR с входом в ревью отвергается");

    let msg = err.to_string();
    assert!(msg.contains("снятием PR"), "суть отказа: {msg}");
    assert!(
        kernel.item_updates().is_empty(),
        "гард до записи: доска не тронута"
    );
}

/// Гард вешается только на целевой статус: «Тестирование» с пустой клеткой
/// проходит как раньше (его ставит ревьювер, PR-контекст его не касается).
#[tokio::test]
#[serial_test::serial]
async fn edit_status_testing_passes_without_guard() {
    let (kernel, server) = MockKernel::start(edit_scenario()).await;
    set_env(&server);

    task_run("uwuwu", edit(Some("Тестирование")))
        .await
        .expect("негардированный статус проходит без PR-клетки");

    assert_eq!(kernel.item_updates().len(), 1, "запись клетки была");
}

/// Пин инцидента: строка «Проверка плана» (носитель 20261104060555-5wj9o6q,
/// origa) отвергается у входа — отказ с эхом ввода и перечнем канона, записи
/// в доску нет (левая строка минтила бы мусорную опцию).
#[tokio::test]
#[serial_test::serial]
async fn edit_status_rejects_incident_string_before_write() {
    let (kernel, server) = MockKernel::start(edit_scenario()).await;
    set_env(&server);

    let err = task_run("uwuwu", edit(Some("Проверка плана")))
        .await
        .expect_err("не-канон отвергается");

    let msg = err.to_string();
    assert!(msg.contains("Проверка плана"), "эхо ввода: {msg}");
    assert!(
        msg.contains("Ревью плана"),
        "перечень канона в отказе: {msg}"
    );
    assert!(
        kernel.item_updates().is_empty(),
        "отказ до записи: доска не тронута"
    );
    assert_eq!(
        kernel.rows()[0].status,
        "Todo",
        "клетка статуса не изменилась"
    );
}

/// «Без частичных обновлений»: не-канон при совместном edit (--title +
/// --status) роняет всю операцию — отказ до любых мутаций.
#[tokio::test]
#[serial_test::serial]
async fn edit_status_rejects_before_partial_updates() {
    let (kernel, server) = MockKernel::start(edit_scenario()).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Edit {
            id: ROW_ITEM.into(),
            status: Some("ревью планы".into()),
            title: Some("тайтл не должен смениться".into()),
            pr: None,
            branch: None,
            r#type: None,
            schedule: None,
            agent: None,
            planned: None,
        },
    )
    .await
    .expect_err("опечатка в статусе роняет операцию");

    assert!(err.to_string().contains("ревью планы"), "{err}");
    assert!(
        kernel.item_updates().is_empty(),
        "записей клетки нет: доска не тронута"
    );
}

/// Пустое и пробельное — отказ (семантики снятия у статуса нет), записей нет.
#[tokio::test]
#[serial_test::serial]
async fn edit_status_empty_and_blank_rejected() {
    let (kernel, server) = MockKernel::start(edit_scenario()).await;
    set_env(&server);

    for bad in ["", "   "] {
        let err = task_run("uwuwu", edit(Some(bad)))
            .await
            .expect_err("пустое/пробельное — отказ");
        assert!(
            err.to_string().contains("не разобран"),
            "внятный отказ: {err}"
        );
    }
    assert!(
        kernel.item_updates().is_empty(),
        "пустое не доходит до записи"
    );
}
