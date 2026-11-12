//! Тесты шапки карточки (format_task_header/created_header) и reload_task.

use super::task::helpers::created_header;
use super::task::task_run;

use super::task::helpers::format_task_header;

use crate::cmd::cli::TaskSub;
use crate::cmd::mock_kernel::{set_env, MockBoard, MockKernel, MockRow, MockScenario};
use crate::siyuan::SiYuan;
use crate::siyuan_task::SyBoard;

pub(super) fn header_board() -> SyBoard {
    SyBoard {
        doc_id: "board-doc".into(),
        av_id: "av-1".into(),
        block_id: "block-1".into(),
        hpath: "/uwuwu/Задачи uwuwu".into(),
        notebook: "nb".into(),
    }
}

pub(super) fn header_task() -> crate::siyuan_task::SyTask {
    crate::siyuan_task::SyTask {
        item_id: "20261111111111-rowhdr".into(),
        title: "таска".into(),
        status: "In Progress".into(),
        task_type: "feature".into(),
        priority: String::new(),
        autodone: false,
        branch: "work/task-x".into(),
        pr: "https://git.example/pr/1".into(),
        dev_session: "ses-live".into(),
        planned: "2026-11-12 09:00".into(),
        planned_ms: 0,
        schedule_cell: "1d".into(),
        body_doc: Some("20261111111111-bodyh1".into()),
    }
}

/// Фулл-шапка: все строки карточки, включая «Сессия dev» (контракт claim).
#[test]
fn task_header_full_shape() {
    let header = format_task_header(&header_board(), &header_task(), Some("1d"), Some("triage"));
    for line in [
        "Задача:   таска",
        "Статус:   In Progress",
        "Тип:      feature",
        "Строка:   20261111111111-rowhdr",
        "Доска:    /uwuwu/Задачи uwuwu",
        "Ветка:    work/task-x",
        "PR:       https://git.example/pr/1",
        "Сессия dev: ses-live",
        "Запланировано: 2026-11-12 09:00",
        "Расписание: 1d (крон: файр в статусе Вейт)",
    ] {
        assert!(header.contains(line), "нет строки «{line}» в:\n{header}");
    }
    assert!(
        header.contains("Агент: triage (вечная сессия крона)"),
        "{header}"
    );
    // шапка — не фулл-ответ create/body: тела в ней нет
    assert!(!header.contains("----- тело"));
}

/// Минимальная шапка: пустые колонки строк не порождают (паритет get).
#[test]
fn task_header_omits_empty_cells() {
    let task = crate::siyuan_task::SyTask {
        item_id: "20261111111111-rowmin".into(),
        title: "минимум".into(),
        status: "Todo".into(),
        task_type: String::new(),
        priority: String::new(),
        autodone: false,
        branch: String::new(),
        pr: String::new(),
        dev_session: String::new(),
        planned: String::new(),
        planned_ms: 0,
        schedule_cell: String::new(),
        body_doc: None,
    };
    let header = format_task_header(&header_board(), &task, None, None);
    assert!(header.contains("Статус:   Todo"), "{header}");
    assert!(!header.contains("Тип:"), "{header}");
    assert!(!header.contains("Ветка:"), "{header}");
    assert!(!header.contains("PR:"), "{header}");
    assert!(!header.contains("Сессия dev"), "{header}");
    assert!(!header.contains("Запланировано"), "{header}");
    assert!(!header.contains("Расписание"), "{header}");
    assert!(!header.contains("Агент:"), "{header}");
}

/// Шапка из факта create (ADR-067-окно): stdout сохраняет «Строка: <id>»
/// для потребителей (крон-ран-карточки) даже при срыве перечитывания.
#[test]
fn created_header_carries_row_id() {
    let board = header_board();
    let header = created_header(
        &board,
        "20261111230000-lagrow",
        "лаг-ран",
        "feature",
        "Todo",
    );
    for line in [
        "Задача:   лаг-ран",
        "Статус:   Todo",
        "Тип:      feature",
        "Строка:   20261111230000-lagrow",
        "Доска:    /uwuwu/Задачи uwuwu",
    ] {
        assert!(header.contains(line), "нет строки «{line}» в:\n{header}");
    }
    // шапка из факта create — тела в ней нет (экспорт не перечитан)
    assert!(!header.contains("----- тело"));
}

/// Композиция read-after-write: reload_task перечитывает render доски и
/// находит строку по itemID (риг — состояние ядра после мутации).
#[tokio::test]
#[serial_test::serial]
async fn reload_task_reads_rows_by_item_id() {
    let scenario = MockScenario {
        notebook: "nb-projects",
        boards: vec![MockBoard {
            doc_id: "rw-doc",
            av_id: "av-rw",
            keys: vec![("key-title", "Первичный ключ"), ("key-status", "Status")],
            legacy: false,
        }],
        hpath_ids: vec![("/uwuwu/Задачи uwuwu", vec!["rw-doc"])],
        rows: vec![MockRow::new("20261111111111-rerwl", "таска").with_status("Todo")],
        ..MockScenario::default()
    };
    let (_kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    let board = crate::siyuan_task::find_board(&sy, "uwuwu").await.unwrap();
    let fresh = super::task::helpers::reload_task(&sy, &board, "20261111111111-rerwl")
        .await
        .expect("строка после мутации видна в render");
    assert_eq!(fresh.status, "Todo");
    assert_eq!(fresh.title, "таска");
    // NotFound — специфичный отказ, не паника (вызывающий решает: ADR-067)
    let err = super::task::helpers::reload_task(&sy, &board, "no-such-item")
        .await
        .expect_err("чужой itemID не резолвится");
    assert!(err.to_string().contains("не видна в render"), "{err}");
}

/// Мёртвая привязка на get — один резолв тела: проба живости привязки
/// (MCP block get) зовётся ровно один раз на вызов (ревью раунд 3 Low:
/// шапка и экспорт резолвили по отдельности — warning деградации
/// печатался дважды). Тело при этом экспортируется fallback-доком
/// по заголовку — degraded-состояние не прячется.
#[tokio::test]
#[serial_test::serial]
async fn get_dead_binding_resolves_body_once() {
    let scenario = MockScenario {
        notebook: "nb-projects",
        boards: vec![MockBoard {
            doc_id: "db-doc",
            av_id: "av-db",
            keys: vec![("key-title", "Первичный ключ"), ("key-status", "Status")],
            legacy: false,
        }],
        hpath_ids: vec![
            ("/uwuwu/Задачи uwuwu", vec!["db-doc"]),
            (
                "/uwuwu/Задачи uwuwu/мёртвая привязка",
                vec!["20261112000000-fallbk"],
            ),
        ],
        rows: vec![MockRow::new("20261112000001-deadw", "мёртвая привязка")
            .with_status("Draft")
            .with_doc("20261111000000-deadd1")],
        // привязка мертва (block get отвечает «not found»)
        block_get_alive: false,
        exports: vec![("20261112000000-fallbk", "тело по заголовку".into())],
        ..MockScenario::default()
    };
    let (kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);
    task_run(
        "uwuwu",
        TaskSub::Get {
            id: "20261112000001-deadw".into(),
            no_body: false,
        },
    )
    .await
    .expect("мёртвая привязка — fallback по заголовку, не отказ");
    // один резолв: проба живости привязки ровно одна на get
    // (два вызова = возврат двойного резолва раунда 3)
    assert_eq!(
        kernel.block_get_calls("20261111000000-deadd1"),
        1,
        "проба живости мёртвой привязки зовётся один раз на get"
    );
}
