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
    comment_with_ts, create_row_with_body, create_with_pacing, CommentDraft, RowInsertPacing,
    SyBoard, SyTask,
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
        "feature",
        "",
        "Draft",
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
    assert!(
        appends.is_empty(),
        "тег типа в тело больше не пишется: {appends:?}"
    );
    let type_writes: Vec<_> = kernel
        .item_updates()
        .into_iter()
        .filter(|(_, key, _, v)| {
            key == "key-type"
                && v.pointer("/mSelect/0/content").and_then(|c| c.as_str()) == Some("feature")
        })
        .collect();
    assert_eq!(
        type_writes.len(),
        1,
        "тип dev записан в колонку «Тип» ровно один раз"
    );
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
        3,
        "Draft каждой вставке (×2) + тип один раз"
    );
    let appends = kernel.appends();
    assert_eq!(appends.len(), 0, "тег типа в тело больше не пишется");
    let type_writes: Vec<_> = kernel
        .item_updates()
        .into_iter()
        .filter(|(_, key, _, v)| {
            key == "key-type"
                && v.pointer("/mSelect/0/content").and_then(|c| c.as_str()) == Some("feature")
        })
        .collect();
    assert_eq!(type_writes.len(), 1, "тип без дублей после ретрая");
}

/// Новые типы канона через полный dispatch-путь create: валидатор пропускает,
/// клетка «Тип» получает канон, тело — костяк (analys делит spike-костяк,
/// docs/other — feature; инварант пинят юниты body_template_maps_new_types…).
#[tokio::test]
#[serial_test::serial]
async fn create_new_types_write_canonical_cell_and_skeleton() {
    for (ty, skeleton_marker) in [
        ("analys", "## Контекст"),
        ("docs", "## Постановка"),
        ("other", "## Постановка"),
    ] {
        let (kernel, server) =
            MockKernel::start(create_scenario(&["item-row"], 1, false, false)).await;
        set_env(&server);

        task_run(
            "uwuwu",
            TaskSub::Create {
                title: NEW_TITLE.into(),
                r#type: Some(ty.into()),
                stdin: false,
                attach: Vec::<std::path::PathBuf>::new(),
                schedule: None,
                agent: None,
                status: None,
                dry_run: false,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("create --type {ty} проходит dispatch: {e}"));

        let type_writes: Vec<_> = kernel
            .item_updates()
            .into_iter()
            .filter(|(_, key, _, v)| {
                key == "key-type"
                    && v.pointer("/mSelect/0/content").and_then(|c| c.as_str()) == Some(ty)
            })
            .collect();
        assert_eq!(
            type_writes.len(),
            1,
            "клетка «Тип» = канон {ty} записан ровно один раз: {:?}",
            kernel.item_updates()
        );
        let (_, md) = kernel
            .created_docs()
            .first()
            .cloned()
            .expect("тело задачи создано");
        assert!(
            md.contains(skeleton_marker),
            "тело {ty} несёт костяк-маркер {skeleton_marker}: {md}"
        );
    }
}

/// create БЕЗ --type (дефолт-ветка dispatch) пишет в клетку «Тип»
/// DEFAULT_TASK_TYPE (feature), не выпиленный dev: дыра ревью PR #221 —
/// захардкоженный "dev" был мимо валидатора и реинкарнировал выпиленный
/// тип на доске штатным путём (agents/developer.md: create без --type).
#[tokio::test]
#[serial_test::serial]
async fn create_without_type_defaults_to_feature_cell_and_skeleton() {
    let (kernel, server) = MockKernel::start(create_scenario(&["item-row"], 1, false, false)).await;
    set_env(&server);

    task_run(
        "uwuwu",
        TaskSub::Create {
            title: NEW_TITLE.into(),
            r#type: None,
            stdin: false,
            attach: Vec::<std::path::PathBuf>::new(),
            schedule: None,
            agent: None,
            status: None,
            dry_run: false,
        },
    )
    .await
    .expect("create без --type проходит dispatch");

    let type_writes: Vec<_> = kernel
        .item_updates()
        .into_iter()
        .filter(|(_, key, _, v)| {
            key == "key-type"
                && v.pointer("/mSelect/0/content").and_then(|c| c.as_str()) == Some("feature")
        })
        .collect();
    assert_eq!(
        type_writes.len(),
        1,
        "клетка «Тип» = дефолт feature записан ровно один раз: {:?}",
        kernel.item_updates()
    );
    assert!(
        !kernel
            .item_updates()
            .iter()
            .any(|(_, key, _, v)| key == "key-type"
                && v.pointer("/mSelect/0/content").and_then(|c| c.as_str()) == Some("dev")),
        "выпиленный dev не должен вминтиться дефолтом: {:?}",
        kernel.item_updates()
    );
    let (_, md) = kernel
        .created_docs()
        .first()
        .cloned()
        .expect("тело задачи создано");
    assert!(
        md.contains("## Постановка") && md.contains("<срезы/шаги"),
        "тело дефолта — фичевый костяк: {md}"
    );
}

// ——— create с контентом: семантика «передал контент — тело твоё» (20261109101129) ———

/// create с непустым контентом (--stdin): контент — тело целиком как есть,
/// скелет не вкатывается (плейсхолдеров «## План»/«## Проверка» нет).
#[tokio::test]
#[serial_test::serial]
async fn create_with_nonempty_content_writes_content_as_body_without_skeleton() {
    let (kernel, server) = MockKernel::start(create_scenario(&["item-row"], 1, false, false)).await;

    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    let content = "## Постановка\n\nсвоя формулировка от агента\n";
    create_with_pacing(
        &sy,
        "uwuwu",
        NEW_TITLE,
        "feature",
        content,
        "Draft",
        &RowInsertPacing::instant(),
    )
    .await
    .expect("create с контентом ok");

    let (_, md) = kernel
        .created_docs()
        .first()
        .cloned()
        .expect("тело задачи создано");
    assert_eq!(
        md, content,
        "контент записан как есть — скелет не оборачивает"
    );
    assert!(
        !md.contains("<срезы/шаги") && !md.contains("<исполняемое"),
        "плейсхолдеров скелета в теле нет: {md}"
    );
}

/// create без контента — регресс: типовой скелет как раньше.
#[tokio::test]
#[serial_test::serial]
async fn create_without_content_writes_type_skeleton() {
    let (kernel, server) = MockKernel::start(create_scenario(&["item-row"], 1, false, false)).await;

    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    create_with_pacing(
        &sy,
        "uwuwu",
        NEW_TITLE,
        "feature",
        "",
        "Draft",
        &RowInsertPacing::instant(),
    )
    .await
    .expect("create без контента ok");

    let (_, md) = kernel
        .created_docs()
        .first()
        .cloned()
        .expect("тело задачи создано");
    assert!(
        md.contains("## Постановка") && md.contains("<срезы/шаги") && md.contains("<исполняемое"),
        "пустой контент — типовой скелет feature: {md}"
    );
}

/// Титульный H1 в контенте стрипается (паритет task body: createDocWithMd
/// оставил бы его heading-блоком — дубль к заголовку дока); чужой H1 и
/// прочий контент — как есть.
#[tokio::test]
#[serial_test::serial]
async fn create_with_title_h1_in_content_strips_only_title_h1() {
    let (kernel, server) = MockKernel::start(create_scenario(&["item-row"], 1, false, false)).await;

    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    let content = format!("# {NEW_TITLE}\n\n## Постановка\n\nтело\n\n# Чужой Раздел\n\nостаётся\n");
    create_with_pacing(
        &sy,
        "uwuwu",
        NEW_TITLE,
        "feature",
        &content,
        "Draft",
        &RowInsertPacing::instant(),
    )
    .await
    .expect("create с контентом ok");

    let (_, md) = kernel
        .created_docs()
        .first()
        .cloned()
        .expect("тело задачи создано");
    assert!(
        !md.contains(&format!("# {NEW_TITLE}\n")),
        "титульный H1 стрипается: {md}"
    );
    assert!(md.contains("# Чужой Раздел"), "чужой H1 остаётся: {md}");
    assert!(
        md.contains("## Постановка\n\nтело"),
        "контент после стрипа как есть: {md}"
    );
    assert!(
        !md.contains("<срезы/шаги"),
        "скелет при непустом контенте не вкатывается: {md}"
    );
}

/// Whitespace-only контент — «пустой»: типовой скелет, как без --stdin.
#[tokio::test]
#[serial_test::serial]
async fn create_with_whitespace_only_content_writes_skeleton() {
    let (kernel, server) = MockKernel::start(create_scenario(&["item-row"], 1, false, false)).await;

    set_env(&server);
    let sy = SiYuan::from_env().unwrap();
    create_with_pacing(
        &sy,
        "uwuwu",
        NEW_TITLE,
        "feature",
        "   \n\n  ",
        "Draft",
        &RowInsertPacing::instant(),
    )
    .await
    .expect("create с whitespace-контентом ok");

    let (_, md) = kernel
        .created_docs()
        .first()
        .cloned()
        .expect("тело задачи создано");
    assert!(
        md.contains("<срезы/шаги"),
        "whitespace-only = пустой: скелет: {md}"
    );
}

/// Dry-run ветка create — read-only осмотр: при живом мок-ядре ноль записей
/// (строка/док/атрибуты не минтятся). Печатаемое тело собирает та же
/// create_body_md, что и боевую запись (юнит-пин равенства в siyuan_task_tests);
/// content-ветка dry-run не тестируется на dispatch — read_stdin читает
/// реальный stdin процесса, обвязки мока в репо нет (рамка ревью: тест без
/// stdin).
#[tokio::test]
#[serial_test::serial]
async fn create_dry_run_is_read_only_with_mock_kernel() {
    let (kernel, server) = MockKernel::start(create_scenario(&["item-row"], 1, false, false)).await;

    set_env(&server);
    task_run(
        "uwuwu",
        TaskSub::Create {
            title: "dry-run осмотр".into(),
            r#type: None,
            stdin: false,
            attach: Vec::<std::path::PathBuf>::new(),
            schedule: None,
            agent: None,
            status: None,
            dry_run: true,
        },
    )
    .await
    .expect("dry-run проходит без отказов");

    assert!(kernel.created_docs().is_empty(), "dry-run не создаёт доков");
    assert!(
        kernel.item_add_args().is_empty(),
        "dry-run не вставляет строк"
    );
    assert!(
        kernel.attr_writes().is_empty(),
        "dry-run не пишет атрибутов"
    );
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
    let type_writes: Vec<_> = kernel
        .item_updates()
        .into_iter()
        .filter(|(_, key, _, _)| key == "key-type")
        .collect();
    assert!(
        type_writes.is_empty(),
        "тип не пишется в неустановленное тело"
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

/// Минт колонки «Тип»: на доске без колонки create создаёт select-колонку
/// через addAttributeViewKey и записывает клетку (ревью Common: goal-путь
/// (None) колонку не минтит — отдельный тест ниже).
#[tokio::test]
#[serial_test::serial]
async fn create_mints_type_column_when_board_lacks_it() {
    let mut sc = create_scenario(&["item-row"], 1, false, false);
    sc.boards[0].keys.retain(|(_, name)| *name != "Тип");
    let (kernel, server) = MockKernel::start(sc).await;

    create_via_flow(&server)
        .await
        .expect("create на доске без «Тип» минтит колонку");

    assert_eq!(
        kernel.add_cols(),
        vec![("av-1".to_string(), "Тип".to_string(), "select".to_string())],
        "одна новая select-колонка «Тип»"
    );
    let type_writes: Vec<_> = kernel
        .item_updates()
        .into_iter()
        .filter(|(_, _, _, v)| {
            v.pointer("/mSelect/0/content").and_then(|c| c.as_str()) == Some("feature")
        })
        .collect();
    assert_eq!(
        type_writes.len(),
        1,
        "клетка типа записана на новой колонке"
    );
}

/// Goal-путь (ty=None) не минтит колонку «Тип» и не пишет клетку —
/// доски «Цели <proj>» остаются без лишней колонки (ревью Common).
#[tokio::test]
#[serial_test::serial]
async fn create_with_none_type_does_not_mint_type_column() {
    let mut sc = create_scenario(&["item-row"], 1, false, false);
    sc.boards[0].keys.retain(|(_, name)| *name != "Тип");
    let (kernel, server) = MockKernel::start(sc).await;
    set_env(&server);
    let sy = SiYuan::from_env().unwrap();

    create_row_with_body(
        &sy,
        &SyBoard {
            doc_id: "board-doc".into(),
            av_id: "av-1".into(),
            block_id: "board-block".into(),
            hpath: BOARD_HPATH.into(),
            notebook: "nb-1".into(),
        },
        NEW_TITLE,
        "todo",
        "## Суть\nконтекст\n",
        None,
        &RowInsertPacing::instant(),
    )
    .await
    .expect("goal-путь создаёт строку");

    assert!(
        kernel.add_cols().is_empty(),
        "колонка «Тип» не минтится: {:?}",
        kernel.add_cols()
    );
    let type_writes: Vec<_> = kernel
        .item_updates()
        .into_iter()
        .filter(|(_, key, _, v)| {
            key == "key-type"
                || v.pointer("/mSelect/0/content").and_then(|c| c.as_str()) == Some("feature")
        })
        .collect();
    assert!(type_writes.is_empty(), "клетка типа не пишется");
}

/// create на доске с колонкой «Тип» не дёргает addAttributeViewKey
/// (ensure экономит мутацию).
#[tokio::test]
#[serial_test::serial]
async fn create_reuses_existing_type_column_without_mint() {
    let (kernel, server) = MockKernel::start(create_scenario(&["item-row"], 1, false, false)).await;

    create_via_flow(&server)
        .await
        .expect("create на доске с «Тип»");

    assert!(
        kernel.add_cols().is_empty(),
        "колонка уже есть — минта нет: {:?}",
        kernel.add_cols()
    );
}

// ——— edit --type: запись, снятие, отказ до записей ———

/// Сценарий edit-флоу: строка с привязанным телом на доске с колонкой «Тип».
fn edit_type_scenario() -> MockScenario {
    let mut sc = MockScenario {
        rows: vec![
            MockRow::new("item-t1", "таск с типом")
                .with_status("Todo")
                .with_type("feature")
                .with_doc(BOUND_DOC),
            MockRow::new("item-t2", "таск без типа")
                .with_status("Todo")
                .with_doc(BOUND_DOC),
        ],
        ..MockScenario::default()
    };
    sc.block_attrs = vec![(BOUND_DOC, vec![])]; // getBlockAttrs жив (body_doc резолв)
    sc
}

async fn edit_type_via_cli(server: &MockServer, id: &str, ty: Option<&str>) -> anyhow::Result<()> {
    set_env(server);
    task_run(
        "uwuwu",
        TaskSub::Edit {
            id: id.into(),
            status: None,
            title: None,
            pr: None,
            branch: None,
            r#type: ty.map(String::from),
            schedule: None,
            agent: None,
            planned: None,
        },
    )
    .await
}

/// edit --type пишет клетку: mSelect content — канон типа.
#[tokio::test]
#[serial_test::serial]
async fn edit_type_writes_type_cell() {
    let (kernel, server) = MockKernel::start(edit_type_scenario()).await;

    edit_type_via_cli(&server, "item-t2", Some("Bug"))
        .await
        .expect("edit --type ok");

    let type_writes: Vec<_> = kernel
        .item_updates()
        .into_iter()
        .filter(|(_, key, item, _)| key == "key-type" && item == "item-t2")
        .collect();
    assert_eq!(type_writes.len(), 1, "одна запись клетки: {type_writes:?}");
    assert_eq!(
        type_writes[0]
            .3
            .pointer("/mSelect/0/content")
            .and_then(|c| c.as_str()),
        Some("bug"),
        "канон типа в нижнем регистре"
    );
}

/// edit --type "" очищает клетку: пустой mSelect без опций.
#[tokio::test]
#[serial_test::serial]
async fn edit_type_empty_clears_type_cell() {
    let (kernel, server) = MockKernel::start(edit_type_scenario()).await;

    edit_type_via_cli(&server, "item-t1", Some(""))
        .await
        .expect("edit --type \"\" ok");

    let type_writes: Vec<_> = kernel
        .item_updates()
        .into_iter()
        .filter(|(_, key, item, _)| key == "key-type" && item == "item-t1")
        .collect();
    assert_eq!(type_writes.len(), 1, "одна запись клетки: {type_writes:?}");
    let cleared = type_writes[0]
        .3
        .get("mSelect")
        .and_then(|m| m.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(false);
    assert!(cleared, "пустой mSelect — очистка: {:?}", type_writes[0].3);
}

/// edit --type с мусором — громкий отказ до любых записей (паритет pr).
#[tokio::test]
#[serial_test::serial]
async fn edit_type_invalid_value_bails_before_any_write() {
    let (kernel, server) = MockKernel::start(edit_type_scenario()).await;

    let err = edit_type_via_cli(&server, "item-t2", Some("hotfix"))
        .await
        .expect_err("мусорный тип — отказ");

    assert!(
        err.to_string().contains(
            "feature | refactoring | legacy | idea | qa | infrastructure | \
                 bug | spike | analys | docs | other"
        ),
        "суть отказа: {err}"
    );
    assert!(
        kernel.item_updates().is_empty(),
        "ни одной записи до отказа: {:?}",
        kernel.item_updates()
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

/// Env-гард тестового пейсинга отката: remove на Drop — паника теста не
/// протекает env в последующие serial-тесты (иначе маскируется их реальное
/// расписание).
struct RestorePacingGuard;
impl Drop for RestorePacingGuard {
    fn drop(&mut self) {
        std::env::remove_var("UWUWU_TEST_RESTORE_BACKOFF_MS");
    }
}

/// Рестарт ядра посреди overwrite (класс 08.11: смерти процесса ядра,
/// NRestarts=3 за вечер): мок сносит поколение MCP-сессий после второго
/// tools/call (render-резолв строки, затем block get живости привязки) —
/// get_children исходной очистки ловит 404 стейл-сессии, overwrite умирает,
/// и откат обязан пережить это: инвалидация сессии на границе попыток →
/// ре-инициализация → дочистка/аппенд пре-имиджа. До фикса единственная
/// попытка отката шла с тем же мёртвым sid → дамп → ручная реставрация.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_rollback_survives_kernel_reset_via_backoff_retries() {
    let mut scenario = body_scenario(true);
    scenario.delete_fail_posts = 4;
    scenario.mcp_restart_after_tools_calls = 2;
    let (kernel, server) = MockKernel::start(scenario).await;

    std::env::set_var("UWUWU_TEST_RESTORE_BACKOFF_MS", "1,1,1");
    let _pacing = RestorePacingGuard;

    let err = body_via_cli(&server, true, None, "новое тело\n")
        .await
        .expect_err("сбой записи — команда не молчит");

    assert!(
        format!("{err:#}").contains("восстановлен"),
        "откат пережил рестарт ядра: {err:#}"
    );
    assert!(
        kernel.mcp_initializes() >= 2,
        "ре-инициализация MCP-сессии на пути отката: {}",
        kernel.mcp_initializes()
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

/// Рестарт ядра гасит MCP-сессию посреди составной команды (задача
/// 20261109095605-qxcouod): recovery в единой точке mcp-вызовов сам
/// восстанавливает сессию — гашение после первого tools/call (render-резолв
/// строки), get_children очистки ловит 404 стейл-сессии → re-init + повтор —
/// и overwrite доезжает штатно, откат не нужен. До общего recovery команда
/// падала на стейл-сессии без попытки восстановления. Reject 404 не
/// исполняет мутацию (session_reject мока отвечает до диспетчеризации) —
/// append нового тела ровно один: once-семантика повтора.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_survives_kernel_reset_via_mcp_session_recovery() {
    let mut scenario = body_scenario(true);
    scenario.mcp_restart_after_tools_calls = 1;
    let (kernel, server) = MockKernel::start(scenario).await;

    body_via_cli(&server, true, None, "новое тело\n")
        .await
        .expect("recovery сессии вытягивает команду после рестарта ядра");

    assert!(
        kernel.mcp_initializes() >= 2,
        "recovery: повторная инициализация сессии: {}",
        kernel.mcp_initializes()
    );
    let appends = kernel.appends();
    assert_eq!(appends.len(), 1, "ровно append нового тела: {appends:?}");
    let body = append_data_field(&appends[0]);
    assert!(
        body.contains("новое тело"),
        "записано новое тело, откат не понадобился: {body:?}"
    );
}

/// Все попытки отката неуспешны (аппенды отката падают весь бэкофф-цикл):
/// прежний worst-case — дамп пре-имиджа 0600 + громкая ошибка с числом
/// попыток; команда честно падает, контент сохранён.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_rollback_exhausts_retries_dumps_pre_image() {
    let mut scenario = body_scenario(true);
    scenario.delete_fail_posts = 4;
    scenario.append_fail_posts = 4;
    let (_kernel, server) = MockKernel::start(scenario).await;

    std::env::set_var("UWUWU_TEST_RESTORE_BACKOFF_MS", "1,1,1");
    let _pacing = RestorePacingGuard;
    let dump_path = std::env::temp_dir().join(format!("uwuwu-overwrite-preimage-{BOUND_DOC}.md"));
    let _ = std::fs::remove_file(&dump_path);

    let err = body_via_cli(&server, true, None, "новое тело\n")
        .await
        .expect_err("сбой записи — команда не молчит");

    let message = format!("{err:#}");
    assert!(
        message.contains("откат не удался"),
        "исчерпание ретраев репортится: {message}"
    );
    assert!(
        message.contains("после 4 попыток"),
        "число попыток в ошибке: {message}"
    );
    let dumped = std::fs::read_to_string(&dump_path).expect("дамп пре-имиджа записан");
    assert!(
        dumped.contains("старый контент"),
        "в дампе исходный контент: {dumped:?}"
    );
    let _ = std::fs::remove_file(&dump_path);
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

// ——— гард вставки обёртки task get в контент (202611080031) ———

/// Полный вывод `task get` вокруг тела — снапшот-форма, которую гард
/// должен встречать на входе контента (но принимать на --base).
fn get_output_snapshot(body: &str) -> String {
    format!(
        "Задача:   {TASK_TITLE}\nСтатус:   Design\nСтрока:   item-task\nДоска:    {BOARD_HPATH}\n\n\
         ----- тело -----\n\n{body}\n"
    )
}

async fn body_via_cli_gated(
    server: &MockServer,
    replace: bool,
    replace_section: Option<&str>,
    content: &str,
    base: Option<&str>,
    force: bool,
) -> anyhow::Result<()> {
    set_env(server);
    let md = tempfile::NamedTempFile::new()?;
    std::fs::write(md.path(), content)?;
    let base_file = base.map(|text| {
        let f = tempfile::NamedTempFile::new().expect("temp-снапшот");
        std::fs::write(f.path(), text).expect("снапшот записан");
        f
    });
    task_run(
        "uwuwu",
        TaskSub::Body {
            id: TASK_TITLE.into(),
            stdin: false,
            md: Some(md.path().to_string_lossy().into_owned()),
            replace,
            replace_section: replace_section.map(str::to_string),
            // as_ref, не map: NamedTempFile дропает файл при move — путь
            // должен пережить task_run
            base: base_file
                .as_ref()
                .map(|f| f.path().to_string_lossy().into_owned()),
            force,
            attach: Vec::new(),
        },
    )
    .await
}

fn assert_no_write(kernel: &MockKernel, why: &'static str) {
    assert!(kernel.created_docs().is_empty(), "{why}");
    assert!(kernel.deletes().is_empty(), "{why}: {:?}", kernel.deletes());
    assert!(kernel.appends().is_empty(), "{why}: {:?}", kernel.appends());
}

/// Replace с полным выводом get — отказ с рецептом, ноль мутаций ядра.
/// --force не обходит: санитария входа — инвариант, не часть сверки свежести.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_with_full_task_get_output_rejects_without_write() {
    let (kernel, server) = MockKernel::start(body_scenario(true)).await;

    let content = get_output_snapshot("## Постановка\n\nсуть\n");
    let err = body_via_cli_gated(&server, true, None, &content, None, true)
        .await
        .expect_err("полный вывод get на входе — отказ");

    assert!(
        format!("{err:#}").contains("обёртку вывода task get"),
        "отказ называет обёртку и рецепт: {err:#}"
    );
    assert_no_write(&kernel, "отказ гарда до записи");
}

/// Append с маркером — тот же отказ: гард один на все контентные пути.
#[tokio::test]
#[serial_test::serial]
async fn body_append_with_task_get_marker_rejects_without_write() {
    let (kernel, server) = MockKernel::start(body_scenario(true)).await;

    let content = "дополнение\n\n----- тело -----\n\nмусор\n";
    let err = body_via_cli_gated(&server, false, None, content, None, false)
        .await
        .expect_err("маркер в append-входе — отказ");

    assert!(
        format!("{err:#}").contains("обёртку вывода task get"),
        "отказ называет обёртку: {err:#}"
    );
    assert_no_write(&kernel, "отказ гарда до записи");
}

/// Replace-section с frontmatter-хвостом get (шапку срезали, frontmatter
/// остался) — второй сигнатурный путь, отказ до записи.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_section_with_frontmatter_tail_rejects_without_write() {
    let (kernel, server) = MockKernel::start(body_scenario(true)).await;

    let content = "---\ntitle: таск\n---\n\nновая постановка\n";
    let err = body_via_cli_gated(&server, true, Some("Постановка"), content, None, false)
        .await
        .expect_err("frontmatter-шапка на входе — отказ");

    assert!(
        format!("{err:#}").contains("frontmatter"),
        "отказ называет frontmatter-сигнатуру: {err:#}"
    );
    assert_no_write(&kernel, "отказ гарда до записи");
}

/// Регресс-паритет с контрактом --base (ADR-053): снапшот полным выводом
/// get + чистый контент — гард молчит, cli срезает обёртку при сверке сам,
/// replace проходит.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_snapshot_as_full_get_output_passes() {
    let (kernel, server) = MockKernel::start(body_scenario(true)).await;

    let snapshot = get_output_snapshot("старый контент");
    body_via_cli_gated(&server, true, None, "новое тело\n", Some(&snapshot), false)
        .await
        .expect("снапшот --base допустим полным выводом get");

    let appends = kernel.appends();
    assert_eq!(appends.len(), 1, "новый контент одним append");
    assert!(
        append_data_field(&appends[0]).contains("новое тело"),
        "контент записан: {appends:?}"
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

// ——— фрагментированная секция «Комменты»: гард на write-пути (20261108110035) ———

/// Фикстура инцидента 08.11: чужая qlty-секция за «Комменты» (replace-section
/// «не найдена — в конец»), поздний коммент аппендом в конец дока — за чужим
/// h2. Юнит-тесты гарда (sy_dispatch::tests) зовут carry напрямую; здесь
/// полный write-путь: dispatch → carry → отказ до записи ядра.
fn fragmented_exports() -> Vec<(&'static str, String)> {
    vec![(
        BOUND_DOC,
        "\
старое тело

## Комменты
- **AGENTS** (2026-11-08 10:00): головной коммент

## Машинный ревью (qlty)
находки линтера

- **AGENTS** (2026-11-08 11:00): PR готов к ревью (хвостовой)
"
        .to_string(),
    )]
}

/// Главный сценарий: полный replace фрагментированного тела без «Комменты»
/// в контенте — громкий отказ с перечнем хвостовых комментов, ядро не
/// тронуто. force=true: tail-гард — инвариант переноса, --force его не
/// обходит (в отличие от freshness-сверки).
#[tokio::test]
#[serial_test::serial]
async fn body_replace_on_fragmented_comments_refuses_before_write() {
    let mut scenario = body_scenario(true);
    scenario.exports = fragmented_exports();
    let (kernel, server) = MockKernel::start(scenario).await;

    let err = body_via_cli(&server, true, None, "новое тело\n")
        .await
        .expect_err("фрагментированная «Комменты» + хвостовой коммент — отказ");

    let msg = format!("{err:#}");
    assert!(msg.contains("фрагментирована"), "суть отказа: {msg}");
    assert!(
        msg.contains("PR готов к ревью"),
        "перечень несёт хвостовой коммент: {msg}"
    );
    assert!(
        !msg.contains("головной коммент"),
        "головной переносится, в потерях не числится: {msg}"
    );
    assert_no_write(&kernel, "гард отказывает до записи");
}

/// Влив хвостового коммента в контент: replace проходит, запись несёт
/// и перенесённую головную секцию «Комменты», и влитой хвост.
#[tokio::test]
#[serial_test::serial]
async fn body_replace_with_tail_inlined_passes_and_carries_head_section() {
    let mut scenario = body_scenario(true);
    scenario.exports = fragmented_exports();
    let (kernel, server) = MockKernel::start(scenario).await;

    let content = "\
новое тело

## qlty (влитой хвост)

- **AGENTS** (2026-11-08 11:00): PR готов к ревью (хвостовой)
";
    body_via_cli(&server, true, None, content)
        .await
        .expect("влитой хвост — replace проходит");

    let appends = kernel.appends();
    assert_eq!(appends.len(), 1, "запись одним append");
    let data = append_data_field(&appends[0]);
    assert!(
        data.contains("PR готов к ревью"),
        "влитой хвост записан: {data}"
    );
    assert!(
        data.contains("## Комменты\n- **AGENTS** (2026-11-08 10:00): головной коммент"),
        "головная секция перенесена: {data}"
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
        task_type: String::new(),
        branch: String::new(),
        pr: String::new(),
        dev_session: String::new(),
        planned: String::new(),
        planned_ms: 0,
        schedule_cell: String::new(),
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
) -> anyhow::Result<(crate::siyuan_task::CommentInserted, CommentOutcome)> {
    let (kernel, server) = MockKernel::start(comment_scenario(base_blocks, apply, lost)).await;
    set_env(&server);
    let sy = SiYuan::from_env()?;
    let draft = CommentDraft {
        author: "AGENTS",
        text,
        reply_to,
    };
    let inserted =
        comment_with_ts(&sy, &comment_board(), &comment_task(), &draft, COMMENT_TS).await?;
    let md = kernel.doc_blocks_of(BOUND_DOC).join("\n");
    let append_attempts = kernel.appends().len();
    Ok((
        inserted,
        CommentOutcome {
            md,
            append_attempts,
        },
    ))
}

/// Секция «Комменты» уже есть: заголовок не дублируется, дописана строка.
#[tokio::test]
#[serial_test::serial]
async fn comment_with_existing_section_appends_only_the_line() {
    let (inserted, outcome) = run_comment(
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
    // контракт ответов: свежая вставка — fresh + маркер как записался
    assert!(inserted.fresh, "первая вставка — fresh");
    assert_eq!(
        inserted.marker, "- **AGENTS** (2026-11-06 12:00): готово",
        "маркер без отступа (не reply)"
    );
}

/// Маркер уже в доке (повтор команды / применённый, но потерянный ответ) —
/// append не делается, коммент остаётся в единственном экземпляре.
#[tokio::test]
#[serial_test::serial]
async fn comment_with_marker_already_in_doc_appends_nothing() {
    let line = "- **AGENTS** (2026-11-06 12:00): готово";
    let (inserted, outcome) = run_comment(&["## Комменты", line], true, false, "готово", None)
        .await
        .expect("дубль распознан как успех");

    assert_eq!(outcome.append_attempts, 0, "записи не было");
    assert_eq!(
        outcome.md.matches(line).count(),
        1,
        "без задвоения: {}",
        outcome.md
    );
    // контракт ответов: дедуп-хит отличим от вставки — fresh=false,
    // ответ печатает «уже был», а не «коммент записан»
    assert!(!inserted.fresh, "дедуп-хит — не вставка");
    assert_eq!(inserted.marker, line, "маркер как записался");
}

/// Потерянный ответ: appendBlock ответил 500, но блок применился —
/// контрольное чтение узнаёт маркер, повтор НЕ делается.
#[tokio::test]
#[serial_test::serial]
async fn comment_survives_lost_append_response_when_marker_landed() {
    let (inserted, outcome) = run_comment(&["## Контекст"], false, true, "готово", None)
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
    // применённая потерянная вставка — fresh=true: маркер в доке, ответ
    // «коммент записан» не врёт
    assert!(inserted.fresh, "маркер приземлился — вставка состоялась");
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
    let (inserted, outcome) = run_comment(
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
    // контракт ответов: reply-маркер в ответе несёт отступ (как записался)
    assert_eq!(
        inserted.marker, "  - **AGENTS** (2026-11-06 12:00): ответ",
        "reply-маркер с отступом"
    );
    assert!(inserted.fresh);
}
