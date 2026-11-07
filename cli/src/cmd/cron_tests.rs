//! cron set/list/pause/resume на общем stateful wiremock-ядре
//! (cmd::mock_kernel, дедуп мок-билдеров 20261107052603): колонки крон-доски
//! валидируются до мутаций (без сирот), строка привязана к телу (blockID),
//! Status — mSelect, список печатает Schedule/Last Run, семья pause/resume/
//! list умеет --project.
//!
//! Сценарии:
//! 1. доска без колонки Schedule → громкий отказ ДО создания тела/строки
//! 2. полноценная доска → bound-строка + Schedule/Status(mSelect)/Last Run
//! 3. дубль-тайтла в живых строках → отказ без мутаций
//! 4. item_add без строки в render → отказ после ретрая ×1 (verify вставки)
//! 5. pause с --project пишет Status на доске указанного проекта
//! 6. таблица list несёт значения Schedule и Last Run
//! 7. парс: --project у list/pause/resume/rm, дефолт None

use crate::cmd::cli::CronSub;
use crate::cmd::mock_kernel::{set_env, MockKernel, MockRow, MockScenario, MockBoard};
use crate::cmd::sy_dispatch::{cron_run, cron_set_with_pacing, format_cron_table};
use crate::siyuan::SiYuan;
use crate::siyuan_task::{RowInsertPacing, SyTask};
use serde_json::json;
use wiremock::MockServer;

const CRON_DOC: &str = "k-doc";
const CRON_AV: &str = "av-k";
const FOIL_DOC: &str = "f-doc";
const FOIL_AV: &str = "av-f";

/// Колонки крон-доски: полный набор (ветки/расписание) или легаси-шаблон
/// без Schedule/Last Run.
fn cron_keys(full: bool) -> Vec<(&'static str, &'static str)> {
    let mut keys = vec![
        ("key-title", "Первичный ключ"),
        ("key-status", "Status"),
        ("key-branch", "Branch"),
        ("key-pr", "PR"),
    ];
    if full {
        keys.push(("key-schedule", "Schedule"));
        keys.push(("key-lastrun", "Last Run"));
    }
    keys
}

/// Сценарий cron-флоу: крон-доска uwuwu (ключи по сценарию) + фольга foilcap
/// (полные ключи) — pause с --project целится в чужую доску.
fn cron_scenario(cron_keys_full: bool) -> MockScenario {
    MockScenario {
        notebook: "nb-projects",
        boards: vec![
            MockBoard {
                doc_id: CRON_DOC,
                av_id: CRON_AV,
                keys: cron_keys(cron_keys_full),
            },
            MockBoard {
                doc_id: FOIL_DOC,
                av_id: FOIL_AV,
                keys: cron_keys(true),
            },
        ],
        hpath_ids: vec![
            ("/uwuwu/Кроны uwuwu", vec![CRON_DOC]),
            ("/foilcap/Кроны foilcap", vec![FOIL_DOC]),
        ],
        inserted_row: MockRow::new("item-mock", "новый крон"),
        ..MockScenario::default()
    }
}

/// Строка крон-доски: активная, с расписанием (render-вид кронов).
fn cron_row(item: &str, title: &str, last_run: &str) -> MockRow {
    MockRow::new(item, title)
        .with_status("active")
        .with_schedule("*/5 * * * *")
        .with_last_run(last_run)
}

/// set через cron_set_with_pacing с мгновенным пейсингом: тесты не наследуют
/// прод-паузу verify-ретрая (пауза 1.5 s вынесена в RowInsertPacing).
async fn set_via_flow(server: &MockServer, title: &str) -> anyhow::Result<()> {
    set_env(server);
    let sy = SiYuan::from_env()?;
    cron_set_with_pacing(
        &sy,
        "uwuwu",
        title,
        "10m",
        "<промпт крона>",
        &RowInsertPacing::instant(),
    )
    .await
}

/// (1) легаси-доска без Schedule: громкий отказ, мутаций не было.
#[tokio::test]
#[serial_test::serial]
async fn set_bails_loud_before_mutations_when_board_lacks_schedule_key() {
    let (kernel, server) = MockKernel::start(cron_scenario(false)).await;

    let err = set_via_flow(&server, "ночной крон")
        .await
        .expect_err("доска без Schedule должна давать отказ");

    assert!(
        err.to_string().contains("Schedule"),
        "сообщение должно называть недостающую колонку: {err}"
    );
    assert!(
        kernel.created_docs().is_empty(),
        "тело не должно создаваться до валидации колонок"
    );
    assert!(
        kernel.item_add_count() == 0,
        "строка не должна создаваться до валидации колонок"
    );
}

/// (2) полноценная доска: bound-строка, Schedule/Last Run текстом, Status mSelect.
#[tokio::test]
#[serial_test::serial]
async fn set_creates_bound_row_and_fills_cells() {
    let mut scenario = cron_scenario(true);
    scenario.row_visible_after_adds = 1;
    let (kernel, server) = MockKernel::start(scenario).await;

    set_via_flow(&server, "ночной крон")
        .await
        .expect("set на полной доске");

    let created = kernel.created_docs();
    assert_eq!(created.len(), 1, "ровно одно тело");
    assert!(
        created[0].0.contains("ночной крон"),
        "тело по hpath тайтла: {:?}",
        created[0]
    );

    let adds = kernel.item_add_args();
    assert_eq!(adds.len(), 1, "одна вставка строки (verify с первого раза)");
    let bound = adds[0]
        .get("blockID")
        .and_then(|b| b.as_str())
        .unwrap_or("");
    assert!(
        !bound.is_empty(),
        "строка привязана к телу (blockID), не detached: {:?}",
        adds[0]
    );
    assert!(
        adds[0].get("detached").is_none(),
        "флаг detached не используется: {:?}",
        adds[0]
    );

    let updates = kernel.item_updates();
    let by_key = |name: &str| updates.iter().find(|(_, k, _, _)| k == name);
    let status = by_key("key-status").expect("Status записан");
    assert_eq!(
        status.3,
        json!({ "mSelect": [{ "content": "active" }] }),
        "Status через mSelect"
    );
    let schedule = by_key("key-schedule").expect("Schedule записан");
    assert_eq!(
        schedule.3,
        json!({ "text": { "content": "10m" } }),
        "Schedule текстом"
    );
    assert!(
        by_key("key-lastrun").is_some(),
        "Last Run записан (пустое значение)"
    );
}

/// (3) тайтл уже жив строкой: отказ без мутаций (дедуп по строкам, не только телам).
#[tokio::test]
#[serial_test::serial]
async fn set_rejects_duplicate_title_in_live_rows_without_mutation() {
    let mut scenario = cron_scenario(true);
    scenario.rows = vec![cron_row("20261105110000-live1", "ночной крон", "")];
    let (kernel, server) = MockKernel::start(scenario).await;

    let err = set_via_flow(&server, "ночной крон")
        .await
        .expect_err("дубль-тайтл в живых строках");

    assert!(
        err.to_string().contains("уже есть"),
        "сообщение про дубль: {err}"
    );
    assert!(
        kernel.created_docs().is_empty(),
        "тело не создано"
    );
    assert!(
        kernel.item_add_count() == 0,
        "строка не создана"
    );
}

/// (4) ядро «потеряло» вставку: после ретрая ×1 — громкий отказ.
#[tokio::test]
#[serial_test::serial]
async fn set_bails_when_row_never_appears_in_render() {
    let mut scenario = cron_scenario(true);
    scenario.row_visible_after_adds = usize::MAX;
    let (kernel, server) = MockKernel::start(scenario).await;

    let err = set_via_flow(&server, "призрачный крон")
        .await
        .expect_err("строка не появилась в render");

    assert!(
        err.to_string().contains("не появилась"),
        "отказ объясняет причину: {err}"
    );
    assert_eq!(
        kernel.item_add_count(),
        2,
        "ровно один ретрай вставки"
    );
}

/// (5) pause с --project пишет Status на доске указанного проекта.
#[tokio::test]
#[serial_test::serial]
async fn pause_targets_project_board() {
    let mut scenario = cron_scenario(true);
    scenario.rows = vec![cron_row("20261105110000-kc1", "крон foilcap", "")];
    let (kernel, server) = MockKernel::start(scenario).await;
    set_env(&server);

    cron_run(CronSub::Pause {
        id: "kc1".into(),
        project: Some("foilcap".into()),
    })
    .await
    .expect("pause на доске foilcap");

    let updates = kernel.item_updates();
    let status = updates
        .iter()
        .find(|(av, k, _, _)| av == FOIL_AV && k == "key-status")
        .expect("Status записан на доске foilcap");
    assert_eq!(
        status.3,
        json!({ "mSelect": [{ "content": "paused" }] }),
        "paused через mSelect"
    );
}

/// (6) таблица list несёт значения Schedule и Last Run.
#[test]
fn list_table_carries_schedule_and_last_run_values() {
    let rows = vec![SyTask {
        item_id: "20261105110000-kc9".into(),
        title: "утренний дайджест".into(),
        status: "active".into(),
        branch: String::new(),
        pr: String::new(),
        schedule: "09:30".into(),
        last_run: "2026-11-05 09:30".into(),
        dev_session: String::new(),
        body_doc: None,
    }];
    let table = format_cron_table(&rows);
    assert!(table.contains("09:30"), "Schedule виден: {table}");
    assert!(
        table.contains("2026-11-05 09:30"),
        "Last Run виден: {table}"
    );
    assert!(
        table.contains("утренний дайджест"),
        "заголовок виден: {table}"
    );
}

/// (7) парс: --project у всей семьи, дефолт — None (uwuwu).
#[test]
fn cron_family_accepts_project_flag() {
    use super::cli::{Cli, Commands};
    use clap::Parser as _;

    let mut argv: Vec<String> = vec!["uwuwu-cli".into()];
    argv.extend(["cron", "pause", "kc1", "--project", "foilcap"].map(String::from));
    let Commands::Cron { command } = Cli::try_parse_from(argv).unwrap().command else {
        panic!("expected cron");
    };
    let super::cli::CronSub::Pause { id, project } = command else {
        panic!("expected pause");
    };
    assert_eq!(id, "kc1");
    assert_eq!(project.as_deref(), Some("foilcap"));

    let mut argv: Vec<String> = vec!["uwuwu-cli".into()];
    argv.extend(["cron", "list"].map(String::from));
    let Commands::Cron { command } = Cli::try_parse_from(argv).unwrap().command else {
        panic!("expected cron");
    };
    let super::cli::CronSub::List { project } = command else {
        panic!("expected list");
    };
    assert!(project.is_none(), "дефолт — глобальная uwuwu");
}

// ——— cron rm (срез покрытия 20261105220435): журнал → строка → тело ———

/// Сценарий rm: крон-доска с одной живой строкой (привязка body-<item>);
/// `binding_alive: false` — привязка мертва (block get — not found).
fn rm_scenario(binding_alive: bool) -> MockScenario {
    let mut scenario = cron_scenario(true);
    scenario.rows = vec![cron_row("20261105120000-item01", "ночной крон", "")
        .with_doc("body-20261105120000-item01")];
    scenario.block_get_alive = binding_alive;
    scenario
}

/// cron rm: write-ahead undo-журнал ДО мутаций → item_remove строки →
/// тело (живая привязка) в трэш.
#[tokio::test]
#[serial_test::serial]
async fn rm_removes_row_and_trashes_body_with_undo_journal() {
    let (kernel, server) = MockKernel::start(rm_scenario(true)).await;
    set_env(&server);

    cron_run(CronSub::Rm {
        id: "20261105120000-item01".into(),
        project: None,
    })
    .await
    .expect("rm на живом кроне");

    let removes = kernel.item_removes();
    assert_eq!(
        removes,
        vec!["20261105120000-item01".to_string()],
        "строка снята item_remove"
    );
    let removed_docs = kernel.removed_docs();
    assert_eq!(
        removed_docs,
        vec![format!("body-{}", removes[0])],
        "привязанное тело в трэш (removeDocByID)"
    );
    let journals = kernel.put_files();
    assert_eq!(journals.len(), 1, "write-ahead журнал один");
    assert!(
        journals[0].ends_with("-uwuwu-cron-rm.json"),
        "журнал kind=cron-rm: {:?}",
        journals
    );
}

/// cron rm при мёртвой привязке тела: строка снимается, трэш не зовётся
/// (тела нет — удалять нечего), журнал всё равно пишется.
#[tokio::test]
#[serial_test::serial]
async fn rm_skips_body_trash_when_binding_is_dead() {
    let (kernel, server) = MockKernel::start(rm_scenario(false)).await;
    set_env(&server);

    cron_run(CronSub::Rm {
        id: "20261105120000-item01".into(),
        project: None,
    })
    .await
    .expect("rm проходит и с мёртвой привязкой");

    assert_eq!(
        *kernel.item_removes(),
        vec!["20261105120000-item01".to_string()],
        "строка снята"
    );
    assert!(
        kernel.removed_docs().is_empty(),
        "мёртвая привязка — removeDocByID не зовётся"
    );
    assert_eq!(
        kernel.put_files().len(),
        1,
        "журнал написан"
    );
}
