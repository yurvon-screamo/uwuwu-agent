//! BDD-сценарии guard'а батч-сноса (задача 20261104215724): вердикты по
//! независимым источникам на общем stateful wiremock-ядре (cmd::mock_kernel)
//! с состоянием render (до/после item_remove), FS-роутером getFile и
//! write-ahead журналом.
//!
//! Сценарии плана ревью:
//! (а) Live+Dead без --live → отказ ЦЕЛИКОМ, ноль мутаций
//! (б) Dead+DraftDead с --yes → снесены, журнал записан, пост-чек зелёный
//! (в) Divergent → dry-run помечает, --yes отказывает весь батч
//! (г) undo восстанавливает; повторный undo — no-op (ноль мутаций)
//! (д) rm Live-не-Draft → отказ; rm Draft с контентным телом → журнал+снос
//! (е) контрольная проба доски miss → аборт без мутаций
//! (ж) пост-чек: лишняя пропажа тайтла → громкая ошибка + путь журнала

use crate::cmd::cli::TaskSub;
use crate::cmd::mock_kernel::{set_env, MockKernel, MockRow, MockScenario};
use crate::cmd::sy_dispatch::task_run;
use crate::siyuan_task::body_template;
use wiremock::MockServer;

const BOARD_FILE: &str = "data/nb-1/folder-doc/board-doc.sy";
const UNDO_JOURNAL: &str = "journal-test.json";

struct Row {
    item: &'static str,
    title: &'static str,
    status: &'static str,
    doc: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        item: "item-live",
        title: "живая задача",
        status: "In Progress",
        doc: Some("20261104000001-livexx1"),
    },
    Row {
        item: "item-dead",
        title: "мёртвая строка",
        status: "Todo",
        doc: Some("20261104000002-dedxxx2"),
    },
    Row {
        item: "item-draft",
        title: "черновик-пустышка",
        status: "Draft",
        doc: Some("20261104000003-dra0003"),
    },
    Row {
        item: "item-div",
        title: "див-строка",
        status: "Todo",
        doc: Some("20261104000004-divxx04"),
    },
    Row {
        item: "item-skel",
        title: "скелет-черновик",
        status: "Draft",
        doc: Some("20261104000005-skelxx5"),
    },
    Row {
        item: "item-draft-live",
        title: "живой черновик",
        status: "Draft",
        doc: Some("20261104000006-drlxx06"),
    },
];

/// Сценарий guard-флоу: фикстура шести строк доски, SQL-индекс (живой только
/// doc-live), hpath-выборки, FS-роутер (тела, див-док 500, доска), экспорты
/// для скелет-сравнения.
fn guard_scenario(missing_board_file: bool, drops_all: bool) -> MockScenario {
    let rows = ROWS
        .iter()
        .map(|r| {
            let mut row = MockRow::new(r.item, r.title).with_status(r.status);
            if let Some(doc) = r.doc {
                row = row.with_doc(doc);
            }
            row
        })
        .collect();
    MockScenario {
        rows,
        sql_docs: vec!["20261104000001-livexx1"],
        // undo восстанавливает строку с фиксированным itemID
        item_add_replies: vec!["item-restored"],
        hpath_ids: vec![
            ("/uwuwu", vec!["folder-doc"]),
            ("/uwuwu/Задачи uwuwu", vec!["board-doc"]),
            ("/uwuwu/Задачи uwuwu/живая задача", vec!["20261104000001-livexx1"]),
            ("/uwuwu/Задачи uwuwu/скелет-черновик", vec!["20261104000005-skelxx5"]),
            ("/uwuwu/Задачи uwuwu/живой черновик", vec!["20261104000006-drlxx06"]),
        ],
        exports: vec![
            ("20261104000005-skelxx5", skeleton_export("скелет-черновик")),
            (
                "20261104000006-drlxx06",
                format!(
                    "---\ntitle: живой черновик\n---\n\n# живой черновик\n\n{}\nзаметки триажа владельца\n",
                    body_template("dev", "")
                ),
            ),
            (
                "20261104000001-livexx1",
                "---\ntitle: живая задача\n---\n\n# живая задача\n\nживой контент владельца\n".to_string(),
            ),
        ],
        file_routes: vec![
            (
                "data/nb-1/folder-doc/board-doc/20261104000001-livexx1.sy",
                200,
                "{\"Type\":\"NodeDocument\"}",
            ),
            (
                "data/nb-1/folder-doc/board-doc/20261104000005-skelxx5.sy",
                200,
                "{\"Type\":\"NodeDocument\"}",
            ),
            (
                "data/nb-1/folder-doc/board-doc/20261104000006-drlxx06.sy",
                200,
                "{\"Type\":\"NodeDocument\"}",
            ),
            (
                "data/nb-1/folder-doc/board-doc/20261104000004-divxx04.sy",
                500,
                "{\"code\":-1,\"msg\":\"boom\"}",
            ),
            (BOARD_FILE, 200, "{\"Type\":\"NodeDocument\"}"),
        ],
        missing_file_path: if missing_board_file { BOARD_FILE } else { "" },
        item_remove_drops_all: drops_all,
        ..MockScenario::default()
    }
}

fn skeleton_export(title: &str) -> String {
    format!(
        "---\ntitle: {title}\n---\n\n# {title}\n\n{}\n\n#dev\n",
        body_template("dev", "")
    )
}

/// (а) Live+Dead без --live: отказ ЦЕЛИКОМ, ноль мутаций (и ноль журналов).
#[tokio::test]
#[serial_test::serial]
async fn clean_refuses_whole_batch_when_live_row_lacks_ack() {
    let (kernel, server) = MockKernel::start(guard_scenario(false, false)).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Clean {
            ids: "живая задача,мёртвая строка".into(),
            yes: true,
            live: None,
        },
    )
    .await
    .expect_err("батч с Live без ack обязан отказать");

    assert!(err.to_string().contains("ЦЕЛИКОМ"), "суть отказа: {err}");
    assert!(
        err.to_string().contains("живая задача"),
        "отказная строка названа: {err}"
    );
    assert_eq!(
        kernel.item_removes().len(),
        0,
        "ноль item_remove"
    );
    assert_eq!(
        kernel.put_files().len(),
        0,
        "журнал не пишется до гейта"
    );
}

/// (б) Dead+DraftDead с --yes: снесены одним item_remove, журнал до мутации,
/// пост-чек зелёный (живая осталась).
#[tokio::test]
#[serial_test::serial]
async fn clean_removes_dead_and_draft_rows_with_yes_and_writes_journal() {
    let (kernel, server) = MockKernel::start(guard_scenario(false, false)).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Clean {
            ids: "мёртвая строка,черновик-пустышка".into(),
            yes: true,
            live: None,
        },
    )
    .await
    .expect("dead+draftdead сносится с --yes");

    assert_eq!(
        kernel.item_removes(),
        vec!["item-dead".to_string(), "item-draft".to_string()],
        "один item_remove массивом"
    );
    assert_eq!(
        kernel.put_files().len(),
        1,
        "write-ahead журнал записан"
    );
    assert_eq!(
        kernel.removed_docs().len(),
        0,
        "у Dead/DraftDead тел нет — трэш не зовётся"
    );
    {
        let rows = kernel.rows();
        let items: Vec<&str> = rows.iter().map(|r| r.item.as_str()).collect();
        assert!(items.contains(&"item-live"), "живая строка на доске");
        assert!(
            !items.contains(&"item-dead") && !items.contains(&"item-draft"),
            "кандидаты сняты"
        );
    }
}

/// (в) Divergent: dry-run помечает и не мутирует; --yes отказывает весь батч.
#[tokio::test]
#[serial_test::serial]
async fn clean_marks_divergent_in_dry_run_and_refuses_execution() {
    let (kernel, server) = MockKernel::start(guard_scenario(false, false)).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Clean {
            ids: "див-строка".into(),
            yes: false,
            live: None,
        },
    )
    .await
    .expect("dry-run всегда проходит");
    assert_eq!(
        kernel.item_removes().len(),
        0,
        "dry-run не мутирует"
    );

    let err = task_run(
        "uwuwu",
        TaskSub::Clean {
            ids: "див-строка".into(),
            yes: true,
            live: None,
        },
    )
    .await
    .expect_err("Divergent не сносится");
    assert!(err.to_string().contains("ЦЕЛИКОМ"), "суть отказа: {err}");
    assert_eq!(kernel.item_removes().len(), 0);
}

/// DraftSkeleton: снос с --yes, пустой скелет тела уходит в трэш.
#[tokio::test]
#[serial_test::serial]
async fn clean_removes_draft_skeleton_and_trashes_body() {
    let (kernel, server) = MockKernel::start(guard_scenario(false, false)).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Clean {
            ids: "скелет-черновик".into(),
            yes: true,
            live: None,
        },
    )
    .await
    .expect("DraftSkeleton сносится без ack");

    assert_eq!(kernel.item_removes().len(), 1);
    assert_eq!(
        kernel.removed_docs().len(),
        1,
        "скелет тела в трэш"
    );
    assert!(!kernel
        .rows()
        .iter()
        .any(|r| r.item == "item-skel"));
}

/// (е) Контрольная проба доски: файла доски нет → аборт без мутаций.
#[tokio::test]
#[serial_test::serial]
async fn clean_aborts_without_mutations_when_board_file_missing() {
    let (kernel, server) = MockKernel::start(guard_scenario(true, false)).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Clean {
            ids: "мёртвая строка".into(),
            yes: true,
            live: None,
        },
    )
    .await
    .expect_err("контрольная проба обязана прервать батч");

    assert!(
        err.to_string().contains("контрольная проба"),
        "суть отказа: {err}"
    );
    assert_eq!(kernel.item_removes().len(), 0, "ноль мутаций");
    assert_eq!(
        kernel.put_files().len(),
        0,
        "журнал не пишется"
    );
}

/// (ж) Пост-чек: item_remove снял больше плана → громкая ошибка с журналом.
#[tokio::test]
#[serial_test::serial]
async fn clean_post_check_alarms_on_unplanned_loss() {
    let (_kernel, server) = MockKernel::start(guard_scenario(false, true)).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Clean {
            ids: "мёртвая строка".into(),
            yes: true,
            live: None,
        },
    )
    .await
    .expect_err("лишняя пропажа обязана поднять аларм");

    let msg = err.to_string();
    assert!(msg.contains("ПОСТ-ЧЕК"), "суть ошибки: {msg}");
    assert!(
        msg.contains("живая задача"),
        "названа неплановая жертва: {msg}"
    );
    assert!(
        msg.contains("task undo"),
        "путь восстановления указан: {msg}"
    );
    assert!(msg.contains("data/assets/undo/"), "журнал упомянут: {msg}");
}

/// (д) rm: Live не-Draft → отказ; Draft с контентным телом (Live+Draft) →
/// журнал + снос строки и тела.
#[tokio::test]
#[serial_test::serial]
async fn rm_refuses_live_task_but_removes_contentful_draft() {
    let (kernel, server) = MockKernel::start(guard_scenario(false, false)).await;
    set_env(&server);

    let err = task_run(
        "uwuwu",
        TaskSub::Rm {
            id: "живая задача".into(),
        },
    )
    .await
    .expect_err("rm живой задачи обязан отказать");
    let msg = err.to_string();
    assert!(msg.contains("rm только для Draft"), "суть отказа: {msg}");
    assert!(msg.contains("task close"), "hint на close: {msg}");
    assert_eq!(kernel.item_removes().len(), 0);

    task_run(
        "uwuwu",
        TaskSub::Rm {
            id: "живой черновик".into(),
        },
    )
    .await
    .expect("контентный Draft (Live+Draft) сносится rm");
    assert_eq!(kernel.item_removes().len(), 1);
    assert_eq!(
        kernel.put_files().len(),
        1,
        "журнал до мутации"
    );
    assert_eq!(
        kernel.removed_docs().len(),
        1,
        "тело черновика в трэш"
    );
}

/// (д-глоб) rm без --project: глобальный резолв + журнал с проектом доски
/// (имя выводится из hpath — undo-валидация остаётся проектной).
#[tokio::test]
#[serial_test::serial]
async fn rm_without_project_resolves_globally_and_journals_board_project() {
    let (kernel, server) = MockKernel::start(guard_scenario(false, false)).await;
    set_env(&server);

    task_run(
        "",
        TaskSub::Rm {
            id: "живой черновик".into(),
        },
    )
    .await
    .expect("rm без флага резолвит глобально и сносит контентный Draft");

    assert_eq!(kernel.item_removes().len(), 1, "строка снята");
    assert_eq!(kernel.put_files().len(), 1, "журнал до мутации");
    let journal_path = &kernel.put_files()[0];
    assert!(
        journal_path.contains("-uwuwu-rm.json"),
        "проект журнала — из доски (uwuwu), не пустая строка: {journal_path}"
    );
}

/// (г) undo: восстанавливает строку из журнала (detached — тела не было);
/// повторный undo — no-op: ни одного нового item_add.
#[tokio::test]
#[serial_test::serial]
async fn undo_restores_row_and_repeats_as_noop() {
    let mut scenario = guard_scenario(false, false);
    let journal = serde_json::json!({
        "created": "2026-11-04T21:00:00+00:00",
        "project": "uwuwu",
        "kind": "clean",
        "av_id": "av-1",
        "board_doc": "board-doc",
        "notebook": "nb-1",
        "rows": [{
            "item_id": "item-old",
            "title": "мёртвая строка",
            "status": "Todo",
            "branch": "cli/task-clean-guard",
            "pr": "https://github.com/yurvon-screamo/uwuwu-agent/pull/50",
            "extra_text_cells": [],
            "body_doc": "20261104000002-dedxxx2",
            "body_hpath": "/uwuwu/Задачи uwuwu/мёртвая строка",
            "body_md": null
        }]
    });
    scenario.files = vec![(
        format!("data/assets/undo/{UNDO_JOURNAL}"),
        journal.to_string().into_bytes(),
    )];
    let (kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);

    // строка снята с доски (журнал писался при её сносе) — иначе undo
    // честно no-op по тайтлу
    kernel.remove_row("item-dead");

    task_run(
        "uwuwu",
        TaskSub::Undo {
            journal: Some(UNDO_JOURNAL.into()),
            list: false,
        },
    )
    .await
    .expect("первый undo восстанавливает");
    let after_first = kernel.item_add_count();
    assert_eq!(after_first, 1, "одна вставка строки");
    assert!(
        kernel
            .rows()
            .iter()
            .any(|r| r.item == "item-restored" && r.title == "мёртвая строка"),
        "восстановленная строка отдаётся render'ом"
    );
    // Status (mSelect) + Branch (text) + PR (url) — три item_update по клеткам
    let updates = kernel.item_updates().len();
    assert!(
        updates >= 2,
        "Branch/PR клетки восстанавливаются item_update: {updates}"
    );

    task_run(
        "uwuwu",
        TaskSub::Undo {
            journal: Some(UNDO_JOURNAL.into()),
            list: false,
        },
    )
    .await
    .expect("повторный undo — идемпотентный no-op");
    assert_eq!(
        kernel.item_add_count(),
        after_first,
        "повтор не мутирует доску (тайтл уже на доске → skip)"
    );
}

#[tokio::test]
async fn undo_list_on_fresh_workspace_is_empty_not_error() {
    // фреш-воркспейс: data/assets/undo не существует (ядро — 404-конверт);
    // список журналов обязан быть бенигн-пустым, не красной ошибкой
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/file/readDir"))
        .respond_with(|_: &_| {
            ResponseTemplate::new(200)
                .set_body_string(r#"{"code":404,"msg":"path does not exist"}"#)
        })
        .mount(&server)
        .await;
    let sy = crate::siyuan::SiYuan::for_tests(server.uri());

    let journals = crate::siyuan_guard::Journal::list(&sy)
        .await
        .expect("нет undo-директории — журналов нет, не ошибка");
    assert!(journals.is_empty());
}
