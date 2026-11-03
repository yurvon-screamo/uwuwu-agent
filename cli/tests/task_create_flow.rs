//! Интеграционные тесты идемпотентного создания (task create / cron set)
//! и task rm против mock-шлюза из tests/common.
//! Сценарии перечисляют RPC в фактическом порядке вызовов: мок
//! последовательный, каждый запрос снимает следующий Step. Все тесты —
//! serial: with_temp_home мутирует глобальный HOME.

mod common;

use common::{test_cfg, with_temp_home, MockGateway, Step, INIT, NOTIFIED};
use std::path::Path;
use uwuwu_cli::affine_client::{AffineClient, AffineConfig};
use uwuwu_cli::affine_tasks::{
    create_cron, create_task, rm_task, Board, CreateOutcome, TaskCx, TaskType,
};

fn rpc(sc: String) -> Step {
    Step::owned(
        200,
        None,
        format!(r#"{{"jsonrpc":"2.0","id":{{ID}},"result":{{"structuredContent":{sc}}}}}"#),
    )
}

fn rows_json(rows: Vec<String>) -> String {
    format!(r#"{{"ok":true,"rows":[{}]}}"#, rows.join(","))
}

fn row_json(row_id: &str, title: &str, linked: &str) -> String {
    let linked_field = if linked.is_empty() {
        String::new()
    } else {
        format!(r#","linkedDocId":"{linked}""#)
    };
    format!(r#"{{"rowBlockId":"{row_id}","title":"{title}"{linked_field}}}"#)
}

fn children_json(children: Vec<String>) -> String {
    format!(r#"{{"ok":true,"children":[{}]}}"#, children.join(","))
}

fn child_json(doc_id: &str, title: &str, in_trash: bool) -> String {
    format!(r#"{{"docId":"{doc_id}","title":"{title}","inTrash":{in_trash}}}"#)
}

fn handshake() -> Vec<Step> {
    vec![
        Step::basic(200, Some("sess-A"), INIT),
        Step::basic(202, None, NOTIFIED),
    ]
}

const REPLACE_OK: &str = r#"{"ok":true}"#;
const REPLACE_FAIL: &str = r#"{"ok":false,"error":"инцидент"}"#;
const TAGS_HAVE_DEV: &str = r#"{"tags":[{"name":"Dev"}]}"#;
const TAG_OK: &str = r#"{"ok":true}"#;
const NEW_ROW: &str = r#"{"rowBlockId":"row-1"}"#;
const DOC_NEW: &str = r#"{"docId":"doc-new"}"#;
const DELETE_OK: &str = r#"{"ok":true}"#;
const TRASH_OK: &str = r#"{"ok":true,"previouslyInTrash":false,"inTrash":true}"#;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

fn task_cx<'a>(client: &'a mut AffineClient, cfg: &'a AffineConfig) -> TaskCx<'a> {
    let board = Board {
        doc_id: "board-1".into(),
        db_id: "db-1".into(),
    };
    // board живёт до конца теста — утекает осознанно ( tempdir-стиль фикстур)
    let board: &'static Board = Box::leak(Box::new(board));
    TaskCx {
        client,
        cfg,
        board,
        root: Path::new("/tmp"),
    }
}

#[test]
#[serial_test::serial]
fn retry_recovers_orphan_without_second_doc() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![child_json(
            "doc-orphan",
            "Тайтл",
            false,
        )])));
        steps.push(rpc(REPLACE_OK.into()));
        steps.push(rpc(TAGS_HAVE_DEV.into()));
        steps.push(rpc(TAG_OK.into()));
        steps.push(rpc(NEW_ROW.into()));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let (row, outcome) = rt
            .block_on(create_task(&mut cx, "Тайтл", "", TaskType::Dev))
            .unwrap();
        assert_eq!(outcome, CreateOutcome::Recovered);
        assert_eq!(row.body_doc, "doc-orphan");
        assert!(!mock.called("create_doc_from_markdown"));
        let add_row = mock
            .requests()
            .into_iter()
            .find(|r| r.contains("add_database_row"))
            .unwrap();
        assert!(add_row.contains("doc-orphan"), "row request: {add_row}");
    });
}

#[test]
#[serial_test::serial]
fn duplicate_is_explicit_error_not_silent_reuse() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![row_json("r1", "Тайтл", "doc-1")])));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let err = rt
            .block_on(create_task(&mut cx, "Тайтл", "", TaskType::Dev))
            .unwrap_err()
            .to_string();
        assert!(err.contains("дубль"), "got: {err}");
        assert!(err.contains("r1") && err.contains("doc-1"), "got: {err}");
        assert!(!mock.called("list_children"));
        assert!(!mock.called("create_doc_from_markdown"));
        assert!(!mock.called("add_database_row"));
    });
}

#[test]
#[serial_test::serial]
fn duplicate_by_row_title_even_if_body_is_not_a_child() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![row_json("r1", "Тайтл", "")])));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let err = rt
            .block_on(create_task(&mut cx, "Тайтл", "", TaskType::Dev))
            .unwrap_err()
            .to_string();
        assert!(err.contains("дубль"), "got: {err}");
        assert!(err.contains("без тела"), "got: {err}");
        assert!(!mock.called("list_children"));
        assert!(!mock.called("create_doc_from_markdown"));
    });
}

#[test]
#[serial_test::serial]
fn linked_child_is_never_orphaned() {
    with_temp_home(|| {
        // строка «Новое» заняла тело doc-1; ретрай со старым тайтлом «Старое»
        // НЕ должен тронуть занятое тело — свежее создание
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![row_json("r1", "Новое", "doc-1")])));
        steps.push(rpc(children_json(vec![child_json(
            "doc-1",
            "Старое",
            false,
        )])));
        steps.push(rpc(DOC_NEW.into()));
        steps.push(rpc(TAGS_HAVE_DEV.into()));
        steps.push(rpc(TAG_OK.into()));
        steps.push(rpc(NEW_ROW.into()));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let (_, outcome) = rt
            .block_on(create_task(&mut cx, "Старое", "", TaskType::Dev))
            .unwrap();
        assert_eq!(outcome, CreateOutcome::Created);
        assert!(!mock.called("replace_doc_with_markdown"));
        let add_row = mock
            .requests()
            .into_iter()
            .find(|r| r.contains("add_database_row"))
            .unwrap();
        assert!(add_row.contains("doc-new"), "row request: {add_row}");
        assert!(!add_row.contains("doc-1"), "row request: {add_row}");
    });
}

#[test]
#[serial_test::serial]
fn busy_candidate_excluded_orphan_still_recovered() {
    with_temp_home(|| {
        // два ребёнка со старым тайтлом: один занят живой строкой, один свободен
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![row_json("r1", "Новое", "doc-T")])));
        steps.push(rpc(children_json(vec![
            child_json("doc-T", "Старое", false),
            child_json("doc-U", "Старое", false),
        ])));
        steps.push(rpc(REPLACE_OK.into()));
        steps.push(rpc(TAGS_HAVE_DEV.into()));
        steps.push(rpc(TAG_OK.into()));
        steps.push(rpc(NEW_ROW.into()));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let (row, outcome) = rt
            .block_on(create_task(&mut cx, "Старое", "", TaskType::Dev))
            .unwrap();
        assert_eq!(outcome, CreateOutcome::Recovered);
        assert_eq!(row.body_doc, "doc-U");
        let replace = mock
            .requests()
            .into_iter()
            .find(|r| r.contains("replace_doc_with_markdown"))
            .unwrap();
        assert!(replace.contains("doc-U"), "replace request: {replace}");
        assert!(!replace.contains("doc-T"), "replace request: {replace}");
        assert!(!mock.called("create_doc_from_markdown"));
    });
}

#[test]
#[serial_test::serial]
fn ambiguous_bodies_are_explicit_error() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![
            child_json("doc-A", "Тайтл", false),
            child_json("doc-B", "Тайтл", false),
        ])));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let err = rt
            .block_on(create_task(&mut cx, "Тайтл", "", TaskType::Dev))
            .unwrap_err()
            .to_string();
        assert!(err.contains("несколько тел"), "got: {err}");
        assert!(err.contains("doc-A") && err.contains("doc-B"), "got: {err}");
        assert!(!mock.called("create_doc_from_markdown"));
        assert!(!mock.called("add_database_row"));
    });
}

#[test]
#[serial_test::serial]
fn fresh_create_regression() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![])));
        steps.push(rpc(DOC_NEW.into()));
        steps.push(rpc(TAGS_HAVE_DEV.into()));
        steps.push(rpc(TAG_OK.into()));
        steps.push(rpc(NEW_ROW.into()));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let (row, outcome) = rt
            .block_on(create_task(&mut cx, "Тайтл", "", TaskType::Dev))
            .unwrap();
        assert_eq!(outcome, CreateOutcome::Created);
        assert_eq!(row.body_doc, "doc-new");
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|r| r.contains("create_doc_from_markdown"))
                .count(),
            1
        );
    });
}

#[test]
#[serial_test::serial]
fn trashed_child_does_not_block_fresh_create() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![child_json(
            "doc-dead",
            "Тайтл",
            true,
        )])));
        steps.push(rpc(DOC_NEW.into()));
        steps.push(rpc(TAGS_HAVE_DEV.into()));
        steps.push(rpc(TAG_OK.into()));
        steps.push(rpc(NEW_ROW.into()));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let (_, outcome) = rt
            .block_on(create_task(&mut cx, "Тайтл", "", TaskType::Dev))
            .unwrap();
        assert_eq!(outcome, CreateOutcome::Created);
    });
}

#[test]
#[serial_test::serial]
fn replace_failure_aborts_before_commit() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![child_json(
            "doc-orphan",
            "Тайтл",
            false,
        )])));
        steps.push(rpc(REPLACE_FAIL.into()));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let err = rt
            .block_on(create_task(&mut cx, "Тайтл", "", TaskType::Dev))
            .unwrap_err();
        // .context прячет деталь гейта из to_string — ассертим цепочку anyhow
        let chain = format!("{err:#}");
        assert!(chain.contains("инцидент"), "got: {chain}");
        assert!(!mock.called("add_database_row"));
    });
}

#[test]
#[serial_test::serial]
fn fresh_create_surfaces_queue_timeout_honestly() {
    with_temp_home(|| {
        // create_doc вернул structuredContent ok:false retryable:true
        // (WRITE_QUEUE_TIMEOUT): клиент исчерпывает ретраи (0..=3 → 4 попытки),
        // наружу — честная ошибка про ретраи и деталь гейта, не «тело создано»
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![])));
        for _ in 0..4 {
            steps.push(rpc(
                r#"{"ok":false,"code":"WRITE_QUEUE_TIMEOUT","error":"waited too long","retryable":true}"#
                    .into(),
            ));
        }
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let err = rt
            .block_on(create_task(&mut cx, "Тайтл", "", TaskType::Dev))
            .unwrap_err();
        // .context прячет retry-деталь из to_string — ассертим цепочку anyhow
        let chain = format!("{err:#}");
        assert!(chain.contains("failed after 3 retries"), "got: {chain}");
        assert!(chain.contains("waited too long"), "got: {chain}");
        assert!(!chain.contains("тело создано"), "got: {chain}");
        assert!(!mock.called("add_database_row"));
    });
}

#[test]
#[serial_test::serial]
fn row_commit_surfaces_queue_timeout_instead_of_fake_success() {
    with_temp_home(|| {
        // ok:false на КОММИТ-шаге (add_database_row) — наружу не может уйти
        // «создана» с пустой строкой: это воспроизводит класс инцидента
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![])));
        steps.push(rpc(DOC_NEW.into()));
        steps.push(rpc(TAGS_HAVE_DEV.into()));
        steps.push(rpc(TAG_OK.into()));
        // retryable:false — немедленный фейл коммит-шага: никакого ложного
        // «создана» с пустой строкой и никаких последующих записей
        steps.push(rpc(
            r#"{"ok":false,"code":"WRITE_QUEUE_TIMEOUT","error":"waited too long","retryable":false}"#
                .into(),
        ));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let err = rt
            .block_on(create_task(&mut cx, "Тайтл", "", TaskType::Dev))
            .unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("add_database_row"), "got: {chain}");
        assert!(chain.contains("waited too long"), "got: {chain}");
        assert!(!chain.contains("тело создано"), "got: {chain}");
    });
}

fn run_cron_create(
    mock: &MockGateway,
    title: &str,
) -> Result<(uwuwu_cli::affine_tasks::TaskRow, CreateOutcome), anyhow::Error> {
    let cfg = test_cfg(mock);
    let rt = runtime();
    let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
    let mut cx = task_cx(&mut client, &cfg);
    rt.block_on(create_cron(&mut cx, title, "5m", "# Крон\n"))
}

#[test]
#[serial_test::serial]
fn cron_set_recovers_orphan() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![child_json(
            "doc-orphan",
            "Крон Т",
            false,
        )])));
        steps.push(rpc(REPLACE_OK.into()));
        steps.push(rpc(TAG_OK.into()));
        steps.push(rpc(NEW_ROW.into()));
        let mock = MockGateway::start(steps);
        let (row, outcome) = run_cron_create(&mock, "Крон Т").unwrap();
        assert_eq!(outcome, CreateOutcome::Recovered);
        assert_eq!(row.body_doc, "doc-orphan");
        let add_row = mock
            .requests()
            .into_iter()
            .find(|r| r.contains("add_database_row"))
            .unwrap();
        assert!(add_row.contains("doc-orphan"), "row request: {add_row}");
        assert!(add_row.contains("Schedule") && add_row.contains("active"));
        assert!(!mock.called("create_doc_from_markdown"));
    });
}

#[test]
#[serial_test::serial]
fn cron_set_duplicate_is_error() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![row_json("r1", "Крон Т", "doc-1")])));
        let mock = MockGateway::start(steps);
        let err = run_cron_create(&mock, "Крон Т").unwrap_err().to_string();
        assert!(err.contains("дубль"), "got: {err}");
        // сущность в сообщении — крон, не задача
        assert!(err.contains("крон с тайтлом"), "got: {err}");
        assert!(!mock.called("create_doc_from_markdown"));
        assert!(!mock.called("add_database_row"));
    });
}

#[test]
#[serial_test::serial]
fn rm_row_path_deletes_row_and_trashes_body() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![row_json("r1", "Задача", "doc-1")])));
        steps.push(rpc(DELETE_OK.into()));
        steps.push(rpc(TRASH_OK.into()));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let report = rt.block_on(rm_task(&mut cx, "r1")).unwrap();
        assert!(report.row_deleted && report.body_trashed);
        assert!(!report.was_in_trash);
        assert!(mock.called("delete_database_row"));
        assert!(mock.called("trash_doc"));
    });
}

#[test]
#[serial_test::serial]
fn rm_row_commit_failure_is_explicit_not_silent() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![row_json("r1", "Задача", "doc-1")])));
        // retryable:false — коммит удаления упал сразу: деструктив (trash)
        // не должен идти за упавшим delete
        steps.push(rpc(
            r#"{"ok":false,"code":"WRITE_QUEUE_TIMEOUT","error":"waited too long","retryable":false}"#
                .into(),
        ));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let err = rt.block_on(rm_task(&mut cx, "r1")).unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("delete_database_row"), "got: {chain}");
        assert!(chain.contains("waited too long"), "got: {chain}");
        assert!(!mock.called("trash_doc"));
    });
}

#[test]
#[serial_test::serial]
fn rm_orphan_path_trashes_body_without_row() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![child_json(
            "doc-orph",
            "Мусор",
            false,
        )])));
        steps.push(rpc(TRASH_OK.into()));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let report = rt.block_on(rm_task(&mut cx, "doc-orph")).unwrap();
        assert!(!report.row_deleted);
        assert!(report.body_trashed);
        assert!(!mock.called("delete_database_row"));
    });
}

#[test]
#[serial_test::serial]
fn rm_ambiguous_prefix_is_error() {
    with_temp_home(|| {
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![
            child_json("doc-1a", "Один", false),
            child_json("doc-1b", "Два", false),
        ])));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let err = rt
            .block_on(rm_task(&mut cx, "doc-1"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("неоднозначно"), "got: {err}");
        assert!(!mock.called("trash_doc"));
    });
}

#[test]
#[serial_test::serial]
fn rm_ambiguous_row_prefix_is_error_no_trash() {
    with_temp_home(|| {
        // High-сценарий ревью: префикс «abc» матчит body_doc одной строки и
        // row_id другой → неоднозначность должна ронять rm ДО любых трэшей
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![
            row_json("r1", "Т1", "abczz"),
            row_json("abc2", "Т2", ""),
        ])));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let err = rt
            .block_on(rm_task(&mut cx, "abc"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("неоднозначно"), "got: {err}");
        assert!(!mock.called("trash_doc"));
        assert!(!mock.called("delete_database_row"));
    });
}

#[test]
#[serial_test::serial]
fn create_without_row_id_is_explicit_error_not_fake_success() {
    with_temp_home(|| {
        // add_database_row вернул ok:true без rowBlockId — «создана» с пустой
        // строкой недопустима (симметрия с rm_task)
        let mut steps = handshake();
        steps.push(rpc(rows_json(vec![])));
        steps.push(rpc(children_json(vec![])));
        steps.push(rpc(DOC_NEW.into()));
        steps.push(rpc(TAGS_HAVE_DEV.into()));
        steps.push(rpc(TAG_OK.into()));
        steps.push(rpc(r#"{"ok":true}"#.into()));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let mut cx = task_cx(&mut client, &cfg);
        let err = rt
            .block_on(create_task(&mut cx, "Тайтл", "", TaskType::Dev))
            .unwrap_err()
            .to_string();
        assert!(err.contains("rowId не возвращён"), "got: {err}");
    });
}

#[test]
#[serial_test::serial]
fn cron_orphan_prefix_ambiguous_across_boards_is_error() {
    with_temp_home(|| {
        // один префикс на двух крон-досках — трэшить «первый попавшийся» нельзя
        let mut steps = handshake();
        // доска 1: find + read_doc + list_children
        steps.push(rpc(
            r#"{"ok":true,"matches":[{"id":"cron-b1","title":"Crons","inTrash":false}]}"#.into(),
        ));
        steps.push(rpc(
            r#"{"ok":true,"blocks":[{"id":"cron-db1","flavour":"affine:database"}]}"#.into(),
        ));
        steps.push(rpc(children_json(vec![child_json(
            "doc-x",
            "Крон X",
            false,
        )])));
        // доска 2: find + read_doc + list_children
        steps.push(rpc(
            r#"{"ok":true,"matches":[{"id":"cron-b2","title":"Кроны origa","inTrash":false}]}"#
                .into(),
        ));
        steps.push(rpc(
            r#"{"ok":true,"blocks":[{"id":"cron-db2","flavour":"affine:database"}]}"#.into(),
        ));
        steps.push(rpc(children_json(vec![child_json(
            "doc-x2",
            "Крон Y",
            false,
        )])));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let err = rt
            .block_on(uwuwu_cli::affine_tasks::rm_cron_orphan(
                &mut client,
                &cfg,
                &["Crons".to_string(), "Кроны origa".to_string()],
                "doc-x",
            ))
            .unwrap_err()
            .to_string();
        assert!(err.contains("неоднозначно"), "got: {err}");
        assert!(
            err.contains("doc-x") && err.contains("doc-x2"),
            "got: {err}"
        );
        assert!(!mock.called("trash_doc"));
    });
}

#[test]
#[serial_test::serial]
fn cron_orphan_path_reachable_when_board_absent() {
    with_temp_home(|| {
        // Common-фикс ревью раунда 2: отсутствующая доска («Crons» в трэше,
        // «Кроны <proj>» не создана) — норма воркспейса, НЕ read-сбой:
        // сирота-путь обязан срабатывать, второй тайтл читается и матчится
        let mut steps = handshake();
        // доска 1 «Crons»: тайтла нет (Absent), дерево тоже пусто
        steps.push(rpc(r#"{"ok":true,"matches":[]}"#.into()));
        steps.push(rpc(r#"{"ok":true,"nodes":[]}"#.into()));
        // доска 2 «Кроны origa»: живая, сирота в детях
        steps.push(rpc(
            r#"{"ok":true,"matches":[{"id":"cron-b2","title":"Кроны origa","inTrash":false}]}"#
                .into(),
        ));
        steps.push(rpc(
            r#"{"ok":true,"blocks":[{"id":"cron-db2","flavour":"affine:database"}]}"#.into(),
        ));
        steps.push(rpc(children_json(vec![child_json(
            "doc-x2",
            "Крон Y",
            false,
        )])));
        steps.push(rpc(TRASH_OK.into()));
        let mock = MockGateway::start(steps);
        let cfg = test_cfg(&mock);
        let rt = runtime();
        let mut client = rt.block_on(AffineClient::connect(&cfg)).unwrap();
        let outcome = rt.block_on(uwuwu_cli::affine_tasks::rm_cron_orphan(
            &mut client,
            &cfg,
            &["Crons".to_string(), "Кроны origa".to_string()],
            "doc-x2",
        ));
        let title = outcome.unwrap_or_else(|e| panic!("err: {e}"));
        assert_eq!(title, "Крон Y");
        assert!(mock.called("trash_doc"));
    });
}
