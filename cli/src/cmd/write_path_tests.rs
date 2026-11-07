//! Write-path тесты task create / body / comment на общем stateful
//! wiremock-ядре (cmd::mock_kernel, дедуп мок-билдеров 20261107052603).
//! Create — через create_with_pacing (orphan-rebind, verify-ретрай,
//! дубль-тайтл в render, missing после ретрая); body — полный --replace
//! (overwrite in place, docId стабилен) и строка вовсе без тела;
//! comment — comment_with_ts (секция, маркер, потерянный ответ, сбой, reply).
//!
//! Мок имитирует ядро честно: render «лагает» (строка видна после N вставок),
//! appendBlock может отвечать 500, применяя или теряя блок — ассерты проверяют
//! исходы (что куда записалось, что создано/не создано), не внутренние вызовы.

use crate::cmd::cli::TaskSub;
use crate::cmd::mock_kernel::{set_env, MockKernel, MockRow, MockScenario};
use crate::cmd::sy_dispatch::task_run;
use crate::siyuan::SiYuan;
use crate::siyuan_task::{
    comment_with_ts, create_with_pacing, CommentDraft, RowInsertPacing, SyBoard, SyTask,
};
use serde_json::Value;
use wiremock::MockServer;

const BOARD_HPATH: &str = "/uwuwu/Задачи uwuwu";
const BOUND_DOC: &str = "20261106090000-boundd1";
const ORPHAN_DOC: &str = "20261105090000-orpha1";
const OTHER_ROW: &str = "item-other";
const TASK_TITLE: &str = "таск";
const NEW_TITLE: &str = "новая задача";
const BODY_HPATH: &str = "/uwuwu/Задачи uwuwu/новая задача";

// ——— create: orphan-rebind, verify-ретрай, дубль в render, missing ———

/// Сценарий create-флоу: очередь itemID-ответов, лаг render, помеха чужой
/// строкой, тело-сирота.
fn create_scenario(
    replies: &[&'static str],
    visible_after: usize,
    impedance: bool,
    orphan: bool,
) -> MockScenario {
    let mut sc = MockScenario {
        item_add_replies: replies.to_vec(),
        row_visible_after_adds: visible_after,
        inserted_row: MockRow::new("item-row", NEW_TITLE),
        ..MockScenario::default()
    };
    if impedance {
        sc.impedance_row = Some(MockRow::new(OTHER_ROW, NEW_TITLE).with_doc(ORPHAN_DOC));
    }
    if orphan {
        sc.hpath_ids.push((BODY_HPATH, vec![ORPHAN_DOC]));
    }
    sc
}

/// Create идёт напрямую через create_with_pacing с мгновенным пейсингом
/// (минуя dispatch-клей: attach/dry_run пути этих тестов не касаются).
async fn create_via_flow(server: &MockServer) -> anyhow::Result<()> {
    set_env(server);
    let sy = SiYuan::from_env()?;
    create_with_pacing(
        &sy,
        "uwuwu",
        NEW_TITLE,
        "dev",
        "",
        &RowInsertPacing::instant(),
    )
    .await
    .map(|_| ())
}

/// Идемпотентный ретрай (ADR-009 «достроить сироту»): строка потерялась,
/// тело-сирота цело → строка привязывается к нему, новый док НЕ создаётся.
#[tokio::test]
#[serial_test::serial]
async fn create_rebinds_lost_row_to_orphan_body_without_new_doc() {
    let (kernel, server) = MockKernel::start(create_scenario(&["item-row"], 1, false, true)).await;

    create_via_flow(&server)
        .await
        .expect("ретрай перепривязывает сироту");

    assert_eq!(kernel.created_docs().len(), 0, "новый док не минтится");
    assert_eq!(kernel.item_add_count(), 1, "одна вставка строки");
    let args = kernel.item_add_args();
    assert!(
        args[0].get("blockID").and_then(|b| b.as_str()) == Some(ORPHAN_DOC),
        "item_add привязан к docId сироты: {args:?}"
    );
    let appends = kernel.appends();
    assert_eq!(
        appends.len(),
        1,
        "после подтверждения пишется только тег типа"
    );
    assert!(appends[0].contains("#dev"), "тег типа: {appends:?}");
}

/// Verify-вставки: success item_add не виден в render (лаг ядра) → ретрай ×1,
/// второй item_add подтверждается, тег пишется один раз.
#[tokio::test]
#[serial_test::serial]
async fn create_retries_row_insert_once_when_render_lags() {
    let (kernel, server) =
        MockKernel::start(create_scenario(&["item-a", "item-row"], 2, false, false)).await;

    create_via_flow(&server)
        .await
        .expect("ретрай доводит строку до render");

    assert_eq!(kernel.created_docs().len(), 1, "тело создаётся один раз");
    assert_eq!(kernel.item_add_count(), 2, "item_add + ретрай");
    assert_eq!(
        kernel.item_updates().len(),
        2,
        "Draft проставляется каждой вставке"
    );
    let appends = kernel.appends();
    assert_eq!(appends.len(), 1, "тег типа один, без дублей после ретрая");
}

/// Дедуп по факту render: в ленте появилась чужая строка с тем же тайтлом →
/// громкий отказ, вторую строку не минтим, тег не пишется.
#[tokio::test]
#[serial_test::serial]
async fn create_bails_when_render_shows_title_with_other_item() {
    let (kernel, server) =
        MockKernel::start(create_scenario(&["item-a"], usize::MAX, true, false)).await;

    let err = create_via_flow(&server)
        .await
        .expect_err("чужая строка с тем же тайтлом — отказ");

    assert!(
        err.to_string().contains("вторую строку не создаю"),
        "суть отказа: {err}"
    );
    assert_eq!(kernel.item_add_count(), 1, "ретрая не было");
    assert!(
        kernel.appends().is_empty(),
        "тег не пишется в неустановленное тело"
    );
}

/// Строка не видна в render и после ретрая → громкая ошибка с item и телом.
#[tokio::test]
#[serial_test::serial]
async fn create_bails_loudly_when_row_missing_after_retry() {
    let (kernel, server) = MockKernel::start(create_scenario(
        &["item-a", "item-b"],
        usize::MAX,
        false,
        false,
    ))
    .await;

    let err = create_via_flow(&server)
        .await
        .expect_err("строка так и не видна — громкая ошибка");

    assert!(
        err.to_string().contains("после вставки и ретрая"),
        "суть отказа: {err}"
    );
    assert_eq!(kernel.item_add_count(), 2, "ровно ретрай ×1");
    assert!(
        kernel.appends().is_empty(),
        "тег не пишется в неподтверждённую задачу"
    );
}

// ——— body: полный --replace и detached-строка ———

const OLD_BLOCK: &str = "20261106090000-blk0001";

/// Сценарий body-флоу: строка задачи на доске (с привязкой или без),
/// последовательность get_children, статический экспорт дока.
fn body_scenario(bound: bool) -> MockScenario {
    let mut row = MockRow::new("item-task", TASK_TITLE);
    if bound {
        row.doc = Some(BOUND_DOC.to_string());
    }
    MockScenario {
        exports: vec![(BOUND_DOC, "старый контент\n".to_string())],
        get_children_seq: if bound {
            vec![format!("- [p] старый блок ({OLD_BLOCK})"), String::new()]
        } else {
            vec![String::new()]
        },
        rows: vec![row],
        ..MockScenario::default()
    }
}

async fn body_via_cli(
    server: &MockServer,
    replace: bool,
    replace_section: Option<&str>,
    content: &str,
) -> anyhow::Result<()> {
    set_env(server);
    let md = tempfile::NamedTempFile::new()?;
    std::fs::write(md.path(), content)?;
    task_run(
        "uwuwu",
        TaskSub::Body {
            id: TASK_TITLE.into(),
            stdin: false,
            md: Some(md.path().to_string_lossy().into_owned()),
            replace,
            replace_section: replace_section.map(str::to_string),
            // тест про docId-стабильность overwrite, не про freshness-гард (#119):
            // force возвращает исходную семантику «replace проходит» без снапшота
            base: None,
            force: replace,
            attach: Vec::new(),
        },
    )
    .await
}

/// Полный --replace: контент перезаписывается на месте (волны удаления →
/// append), createDocWithMd не зовётся — docId тела стабилен.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_overwrites_doc_in_place_without_recreating() {
    let (kernel, server) = MockKernel::start(body_scenario(true)).await;

    body_via_cli(&server, true, None, "новое тело\n")
        .await
        .expect("replace проходит");

    assert_eq!(kernel.created_docs().len(), 0, "док не пересоздаётся");
    let deletes = kernel.deletes();
    assert_eq!(*deletes, vec![OLD_BLOCK.to_string()], "старый блок удалён");
    let appends = kernel.appends();
    assert_eq!(appends.len(), 1, "новый контент одним append");
    assert!(
        appends[0].contains("новое тело"),
        "контент записан: {appends:?}"
    );
    assert!(appends[0].contains(BOUND_DOC), "append в привязанный док");
}

/// Красный тест класса «дубль титульного H1» (20261107051501). Контент,
/// собранный из get-снапшота, местами несёт титульную строку `# {тайтл}`
/// (nmcfoxy 07.11 00:18: все блоки тела перезаписаны одним overwrite, h1
/// пришёл в markdown записи) — write-путь стрипает титульные H1-строки
/// до отправки: тело не получает дубль к док-заголовку.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_strips_title_h1_from_content() {
    let (kernel, server) = MockKernel::start(body_scenario(true)).await;

    let content = format!("# {TASK_TITLE}\n\nновое тело\n\n# {TASK_TITLE}\nещё текст\n");
    body_via_cli(&server, true, None, &content)
        .await
        .expect("replace проходит");

    let appends = kernel.appends();
    assert_eq!(appends.len(), 1, "новый контент одним append");
    let data = append_data_field(&appends[0]);
    assert!(
        data.contains("новое тело"),
        "полезный контент записан: {appends:?}"
    );
    assert!(
        !data
            .lines()
            .any(|l| l.trim_end() == format!("# {TASK_TITLE}")),
        "титульный H1 не проходит в запись (дубль к док-заголовку): {appends:?}"
    );
}

/// --replace-section: секционный контент с титульной строкой тоже не должен
/// положить дубль в тело — секция вставляется под своим h2, титульный H1
/// внутри неё стал бы вторым H1 в доке.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_section_strips_title_h1_from_content() {
    let mut scenario = body_scenario(true);
    scenario.exports = vec![(
        BOUND_DOC,
        "# титул экспорта\n\n## Постановка\n\nстарая постановка\n\n## План\n\nплан\n".to_string(),
    )];
    scenario.get_children_seq = vec![String::new()];
    let (kernel, server) = MockKernel::start(scenario).await;

    let content = format!("# {TASK_TITLE}\n\nновая постановка\n");
    body_via_cli(&server, false, Some("Постановка"), &content)
        .await
        .expect("replace-section проходит");

    let appends = kernel.appends();
    assert_eq!(appends.len(), 1, "пересборка одним append");
    let data = append_data_field(&appends[0]);
    assert!(
        data.contains("новая постановка"),
        "секция заменена: {appends:?}"
    );
    assert!(
        data.contains("## План"),
        "остальное тело сохранено: {appends:?}"
    );
    assert!(
        !data
            .lines()
            .any(|l| l.trim_end() == format!("# {TASK_TITLE}")),
        "титульный H1 из секционного контента не проходит в запись: {appends:?}"
    );
}

/// Сбой append после очистки: новый контент не записался → авто-откат
/// возвращает пре-имидж, команда падает честно, контент не потерян.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_append_failure_restores_pre_image() {
    let mut scenario = body_scenario(true);
    scenario.append_fail_posts = 1;
    scenario.append_applies = true;
    let (kernel, server) = MockKernel::start(scenario).await;

    let err = body_via_cli(&server, true, None, "новое тело\n")
        .await
        .expect_err("сбой записи — команда не молчит");

    assert!(
        format!("{err:#}").contains("восстановлен"),
        "откат репортится в ошибке: {err:#}"
    );
    let appends = kernel.appends();
    assert_eq!(
        appends.len(),
        1,
        "ровно восстановительный append (отказавший POST не в логе): {appends:?}"
    );
    let restored = append_data_field(&appends[0]);
    assert!(
        restored.contains("старый контент"),
        "исходный контент возвращён в док: {restored:?}"
    );
}

/// Сбой delete в середине очистки (одноразовый latched отказ: первые 4 POST
/// = первый delete_block со всеми ретраями) → авто-откат дочищает и
/// возвращает пре-имидж.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_delete_failure_restores_pre_image() {
    let mut scenario = body_scenario(true);
    scenario.append_applies = true;
    scenario.delete_fail_posts = 4;
    let (kernel, server) = MockKernel::start(scenario).await;

    let err = body_via_cli(&server, true, None, "новое тело\n")
        .await
        .expect_err("сбой очистки — команда не молчит");

    assert!(
        format!("{err:#}").contains("восстановлен"),
        "откат репортится в ошибке: {err:#}"
    );
    let appends = kernel.appends();
    assert_eq!(
        appends.len(),
        1,
        "ровно восстановительный append: {appends:?}"
    );
    let restored = append_data_field(&appends[0]);
    assert!(
        restored.contains("старый контент"),
        "исходный контент возвращён в док: {restored:?}"
    );
}

/// Сбой экспорта пре-имиджа: мутации отменены до очистки — док нетронут,
/// сбой чтения не читается как «док был пуст» (класс PR #132).
#[tokio::test]
#[serial_test::serial]
async fn body_replace_export_failure_aborts_before_mutation() {
    let mut scenario = body_scenario(true);
    scenario.export_fail = true;
    let (kernel, server) = MockKernel::start(scenario).await;

    let err = body_via_cli(&server, true, None, "новое тело\n")
        .await
        .expect_err("сбой чтения пре-имиджа — команда не молчит");

    assert!(
        format!("{err:#}").contains("пре-имидж"),
        "отказ указывает на пре-имидж: {err:#}"
    );
    assert!(
        kernel.deletes().is_empty(),
        "очистка не началась: {:?}",
        kernel.deletes()
    );
    assert!(
        kernel.appends().is_empty(),
        "записей нет: {:?}",
        kernel.appends()
    );
}

/// Поле data из захваченного тела appendBlock (JSON-конверт) — для
/// построчных ассертов по записанному markdown (сабстринг-поиск по JSON
/// с экранированными \n хрупок).
fn append_data_field(raw_append: &str) -> String {
    let body: Value = serde_json::from_str(raw_append).expect("appendBlock body — JSON");
    body.get("data")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string()
}

/// Строка вовсе без тела: тело создаётся по заголовку с контентом,
/// привязка строки (itemID) не трогается.
#[tokio::test]
#[serial_test::serial]
async fn body_on_row_without_body_creates_doc_by_title_without_binding() {
    let (kernel, server) = MockKernel::start(body_scenario(false)).await;

    body_via_cli(&server, false, None, "контент нового тела\n")
        .await
        .expect("body на detached-строке создаёт тело");

    assert_eq!(kernel.created_docs().len(), 1, "один createDoc");
    let created = kernel.created_docs();
    assert_eq!(
        created[0].0,
        format!("{BOARD_HPATH}/{TASK_TITLE}"),
        "тело создаётся по заголовку строки"
    );
    assert!(
        created[0].1.contains("контент нового тела"),
        "контент в markdown"
    );
    assert!(
        kernel.appends().is_empty(),
        "запись идёт createDocWithMd, не appendBlock"
    );
}

// ——— comment: секция, маркер, потерянный ответ, сбой, reply ———

const COMMENT_TS: &str = "2026-11-06 12:00";

/// Сценарий comment-флоу: док с начальными блоками, appendBlock применяет
/// блок и/или отвечает ошибкой (потерянный ответ = применённый + 500).
fn comment_scenario(blocks: &[&str], apply: bool, lost: bool) -> MockScenario {
    MockScenario {
        append_applies: apply || lost,
        append_status: if apply { 200 } else { 500 },
        doc_blocks: vec![(BOUND_DOC, blocks.iter().map(|s| s.to_string()).collect())],
        ..MockScenario::default()
    }
}

fn comment_board() -> SyBoard {
    SyBoard {
        doc_id: "board-doc".into(),
        av_id: "av-1".into(),
        block_id: "carrier-block".into(),
        hpath: BOARD_HPATH.into(),
        notebook: "nb-1".into(),
    }
}

fn comment_task() -> SyTask {
    SyTask {
        item_id: "item-1".into(),
        title: TASK_TITLE.into(),
        status: "Draft".into(),
        branch: String::new(),
        pr: String::new(),
        dev_session: String::new(),
        body_doc: Some(BOUND_DOC.into()),
    }
}

#[derive(Debug)]
struct CommentOutcome {
    /// state дока после коммента (экспорт).
    md: String,
    /// сколько append-попыток сделал comment.
    append_attempts: usize,
}

async fn run_comment(
    base_blocks: &[&str],
    apply: bool,
    lost: bool,
    text: &str,
    reply_to: Option<&str>,
) -> anyhow::Result<CommentOutcome> {
    let (kernel, server) = MockKernel::start(comment_scenario(base_blocks, apply, lost)).await;
    set_env(&server);
    let sy = SiYuan::from_env()?;
    let draft = CommentDraft {
        author: "AGENTS",
        text,
        reply_to,
    };
    comment_with_ts(&sy, &comment_board(), &comment_task(), &draft, COMMENT_TS).await?;
    let md = kernel.doc_blocks_of(BOUND_DOC).join("\n");
    let append_attempts = kernel.appends().len();
    Ok(CommentOutcome {
        md,
        append_attempts,
    })
}

/// Секция «Комменты» уже есть: заголовок не дублируется, дописана строка.
#[tokio::test]
#[serial_test::serial]
async fn comment_with_existing_section_appends_only_the_line() {
    let outcome = run_comment(
        &[
            "## Контекст\nзаметки",
            "## Комменты",
            "- **AGENTS** (11.11 10:00): старый",
        ],
        true,
        false,
        "готово",
        None,
    )
    .await
    .expect("коммент ложится в существующую секцию");

    assert_eq!(outcome.append_attempts, 1, "только строка, без заголовка");
    assert!(
        outcome
            .md
            .contains("- **AGENTS** (2026-11-06 12:00): готово"),
        "новый: {}",
        outcome.md
    );
    assert_eq!(
        outcome.md.matches("## Комменты").count(),
        1,
        "заголовок единственный"
    );
    assert!(outcome.md.contains("старый"), "прежние комменты на месте");
}

/// Маркер уже в доке (повтор команды / применённый, но потерянный ответ) —
/// append не делается, коммент остаётся в единственном экземпляре.
#[tokio::test]
#[serial_test::serial]
async fn comment_with_marker_already_in_doc_appends_nothing() {
    let line = "- **AGENTS** (2026-11-06 12:00): готово";
    let outcome = run_comment(&["## Комменты", line], true, false, "готово", None)
        .await
        .expect("дубль распознан как успех");

    assert_eq!(outcome.append_attempts, 0, "записи не было");
    assert_eq!(
        outcome.md.matches(line).count(),
        1,
        "без задвоения: {}",
        outcome.md
    );
}

/// Потерянный ответ: appendBlock ответил 500, но блок применился —
/// контрольное чтение узнаёт маркер, повтор НЕ делается.
#[tokio::test]
#[serial_test::serial]
async fn comment_survives_lost_append_response_when_marker_landed() {
    let outcome = run_comment(&["## Контекст"], false, true, "готово", None)
        .await
        .expect("потерянный ответ не валит коммент");

    assert_eq!(
        outcome.append_attempts, 2,
        "заголовок + строка, без ретраев"
    );
    assert!(
        outcome.md.contains("## Комменты"),
        "заголовок применился: {}",
        outcome.md
    );
    assert!(
        outcome
            .md
            .contains("- **AGENTS** (2026-11-06 12:00): готово"),
        "строка: {}",
        outcome.md
    );
}

/// Append не применился и ответил ошибкой — честный сбой, коммента в доке нет.
#[tokio::test]
#[serial_test::serial]
async fn comment_fails_loudly_when_append_never_lands() {
    let err = run_comment(&["## Контекст"], false, false, "готово", None)
        .await
        .expect_err("не применённый append — ошибка");

    assert!(
        err.to_string().contains("500"),
        "транспортная причина в сообщении: {err}"
    );
}

/// Реплай пишется с отступом под родителя (`  - **…**`).
#[tokio::test]
#[serial_test::serial]
async fn comment_reply_line_is_indented_under_parent() {
    let outcome = run_comment(
        &["## Комменты", "- **owner** (11.11 09:00): вопрос"],
        true,
        false,
        "ответ",
        Some("20261105120000-parent"),
    )
    .await
    .expect("реплай ложится");

    assert!(
        outcome
            .md
            .contains("\n  - **AGENTS** (2026-11-06 12:00): ответ"),
        "реплай с отступом: {:?}",
        outcome.md
    );
}
