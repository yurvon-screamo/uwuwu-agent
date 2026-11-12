//! Тесты тела: write/replace/full-replace через CLI-моки, section-хирургия,
//! carry-over комментов, strip обёртки get, снапшот --base, attach-канал body.

use super::body_comments::carry_over_comments_section;
use super::body_comments::collect_threads;

use super::body_sections::replace_md_section;
use super::body_comments::require_fresh_snapshot;

use super::strip::strip_task_get_output;
use super::task::task_run;

use super::body_sections::SectionEdit;
use crate::cmd::cli::TaskSub;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use super::test_support::*;
use super::attach::collect_asset_refs;
use serde_json::json;
use super::body::read_base_snapshot;

/// task body --replace (force: false) через task_run: контент пишется в
/// temp-файл, снапшот --base опционален. Result — для unwrap_err на
/// тестах отказного пути.
async fn run_body_replace(
    id: &str,
    snapshot: Option<&std::path::Path>,
    content: &str,
) -> anyhow::Result<()> {
    let md = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(md.path(), content).unwrap();
    task_run(
        "uwuwu",
        TaskSub::Body {
            id: id.into(),
            stdin: false,
            md: Some(md.path().to_string_lossy().into_owned()),
            replace: true,
            replace_section: None,
            base: snapshot.map(|p| p.to_string_lossy().into_owned()),
            force: false,
            attach: Vec::new(),
        },
    )
    .await
}

async fn write_body_via_cli(server: &MockServer) -> anyhow::Result<()> {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    let md = tempfile::NamedTempFile::new()?;
    std::fs::write(md.path(), "маркер-контент\n")?;
    task_run(
        "uwuwu",
        TaskSub::Body {
            id: "таск".into(),
            stdin: false,
            md: Some(md.path().to_string_lossy().into_owned()),
            replace: false,
            replace_section: None,
            base: None,
            force: false,
            attach: Vec::new(),
        },
    )
    .await
}

/// BDD-сценарий задачи: bound-строка + док-сирота с тем же заголовком →
/// контент попадает в привязанное тело, docId ответа = привязанный,
/// новых доков не создаётся (сирота стоит ПЕРВОЙ в ответе getIDsByHPath).
#[tokio::test]
#[serial_test::serial]
async fn body_writes_into_bound_doc_and_never_creates_docs() {
    let (server, create_calls, append_bodies, _deletes) =
        mock_siyuan(vec![ORPHAN_DOC.to_string(), BOUND_DOC.to_string()], true).await;
    write_body_via_cli(&server).await.unwrap();

    assert_eq!(
        create_calls.load(Ordering::SeqCst),
        0,
        "createDocWithMd не должен вызываться"
    );
    {
        let appends = append_bodies.lock().unwrap();
        assert_eq!(appends.len(), 1, "один appendBlock в привязанный док");
        assert!(
            appends[0].contains("маркер-контент"),
            "контент в appendBlock"
        );
        assert!(
            appends[0].contains(&format!("\"parentID\":\"{BOUND_DOC}\"")),
            "append в привязанный док, не в сироту"
        );
    }
}

/// Rewrite-путь (--replace-section): existing-экспорт несёт обёртку
/// (frontmatter + рендер-строка имени дока) — после перезаписи НИ имя,
/// НИ frontmatter не реимпортируются реальными блоками (симптом
/// «--replace-section множит заголовки»).
#[tokio::test]
#[serial_test::serial]
async fn replace_section_rewrites_without_name_and_frontmatter_blocks() {
    let export = "---\ntitle: таск\n---\n\n# таск\n\n## Проверка\nстарое тело секции\n".to_string();
    let (server, create_calls, append_bodies, _deletes) =
        mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    let md = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(md.path(), "новое содержимое секции\n").unwrap();
    task_run(
        "uwuwu",
        TaskSub::Body {
            id: "таск".into(),
            stdin: false,
            md: Some(md.path().to_string_lossy().into_owned()),
            replace: false,
            replace_section: Some("Проверка".into()),
            base: None,
            force: false,
            attach: Vec::new(),
        },
    )
    .await
    .unwrap();

    assert_eq!(create_calls.load(Ordering::SeqCst), 0);
    {
        let appends = append_bodies.lock().unwrap();
        assert_eq!(appends.len(), 1, "один appendBlock пересборки");
        let payload = &appends[0];
        assert!(
            payload.contains("новое содержимое секции"),
            "новая секция в appendBlock"
        );
        assert!(
            !payload.contains("# таск"),
            "имя дока не реимпортируется блоком"
        );
        assert!(
            !payload.contains("title: таск"),
            "frontmatter не реимпортируется блоком"
        );
        assert!(
            !payload.contains("старое тело секции"),
            "старая секция заменена"
        );
    }
}

/// Гард уровня на мок-ядре: тёзка «Подсекция» только на ### → task_run
/// возвращает Err с уровнем и строкой, записи в ядро нет — ни appendBlock
/// пересборки, ни deleteBlock-шторма overwrite_doc_md (тот, что валил ядро).
#[tokio::test]
#[serial_test::serial]
async fn replace_section_twin_other_level_refuses_without_kernel_write() {
    let export =
        "---\ntitle: таск\n---\n\n# таск\n\n## Родитель\nввод\n### Подсекция\nдетали\n".to_string();
    let (server, create_calls, append_bodies, delete_calls) =
        mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    let md = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(md.path(), "новое содержимое\n").unwrap();
    let err = task_run(
        "uwuwu",
        TaskSub::Body {
            id: "таск".into(),
            stdin: false,
            md: Some(md.path().to_string_lossy().into_owned()),
            replace: false,
            replace_section: Some("Подсекция".into()),
            base: None,
            force: false,
            attach: vec![],
        },
    )
    .await
    .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("h3") && msg.contains("### Подсекция"),
        "суть отказа: {msg}"
    );
    assert_eq!(create_calls.load(Ordering::SeqCst), 0, "доки не создаются");
    {
        let appends = append_bodies.lock().unwrap();
        assert!(
            appends.is_empty(),
            "appendBlock не вызывался: {:?}",
            *appends
        );
    }
    assert_eq!(
        delete_calls.load(Ordering::SeqCst),
        0,
        "deleteBlock-шторма нет — ядро не тронуто"
    );
}

/// Гард свежести полного --replace: без --base и без --force — отказ с
/// рецептом до первой мутации (экспорт read-only): ни appendBlock
/// пересборки, ни deleteBlock-шторма — ядро не тронуто.
#[tokio::test]
#[serial_test::serial]
async fn full_replace_without_snapshot_or_force_refuses_without_kernel_write() {
    let export = "---\ntitle: таск\n---\n\n# таск\n\nтело\n".to_string();
    let (server, create_calls, append_bodies, delete_calls) =
        mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    let err = run_body_replace("таск", None, "новое тело\n")
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("--base") && msg.contains("--force"),
        "рецепт в отказе: {msg}"
    );
    assert_eq!(create_calls.load(Ordering::SeqCst), 0, "доки не создаются");
    {
        let appends = append_bodies.lock().unwrap();
        assert!(
            appends.is_empty(),
            "appendBlock не вызывался: {:?}",
            *appends
        );
    }
    assert_eq!(delete_calls.load(Ordering::SeqCst), 0, "ядро не тронуто");
}

/// Свежий снапшот (--base) — полный вывод task get (шапка карточки +
/// обёртка экспорта + рендер-шум пустыми строками + хвост «Аттачи:») —
/// сверка проходит нормализованно и запись состоялась.
#[tokio::test]
#[serial_test::serial]
async fn full_replace_with_fresh_task_get_snapshot_writes() {
    let export = "---\ntitle: таск\n---\n\n# таск\n\nтело\n\n## Комменты\n- коммент\n".to_string();
    let (server, create_calls, append_bodies, _deletes) =
        mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    // снапшот — «task get целиком»: рендер-шум (пустые строки) не рвёт сверку
    let snapshot = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
            snapshot.path(),
            "Задача:   таск\nСтатус:   Design\nСтрока:   item-1\n\n----- тело -----\n\n---\ntitle: таск\n---\n\n# таск\n\nтело\n\n\n## Комменты\n\n\n- коммент\n\nАттачи:\n  /tmp/uwuwu-task-x/скрин.png\n",
        )
        .unwrap();
    run_body_replace("таск", Some(snapshot.path()), "новое тело\n")
        .await
        .unwrap();

    assert_eq!(create_calls.load(Ordering::SeqCst), 0);
    {
        let appends = append_bodies.lock().unwrap();
        assert!(
            appends.iter().any(|p| p.contains("новое тело")),
            "запись состоялась: {:?}",
            *appends
        );
    }
}

/// Свежий снапшот на тайтле с маркер-подстрокой (живой кейс
/// 20261108003111): срезка --base по ровной строке маркера — replace
/// пишет, ложно отказа «тело менялось» нет.
#[tokio::test]
#[serial_test::serial]
async fn full_replace_with_fresh_snapshot_with_marker_title_writes() {
    let title = "тайтл с «----- тело -----» внутри";
    let export = format!("---\ntitle: {title}\n---\n\n# {title}\n\nтело\n");
    let (server, create_calls, append_bodies, _deletes) =
        mock_siyuan_with_export_titled(vec![BOUND_DOC.to_string()], true, export, title).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    let snapshot = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
            snapshot.path(),
            format!(
                "Задача:   {title}\nСтатус:   Design\nСтрока:   item-1\n\n----- тело -----\n\n---\ntitle: {title}\n---\n\n# {title}\n\nтело\n\nАттачи:\n  /tmp/uwuwu-task-x/скрин.png\n"
            ),
        )
        .unwrap();
    run_body_replace(title, Some(snapshot.path()), "новое тело\n")
        .await
        .unwrap();
    assert_eq!(create_calls.load(Ordering::SeqCst), 0);
    {
        let appends = append_bodies.lock().unwrap();
        assert!(
            appends.iter().any(|p| p.contains("новое тело")),
            "запись состоялась: {:?}",
            *appends
        );
    }
}

/// Устаревший снапшот (тело менялось после снятия) — отказ «менялся
/// после снапшота» до первой мутации: ядро не тронуто.
#[tokio::test]
#[serial_test::serial]
async fn full_replace_with_stale_snapshot_refuses_without_kernel_write() {
    let export = "---\ntitle: таск\n---\n\n# таск\n\nтело\n\nпараллельная правка\n".to_string();
    let (server, _create, append_bodies, delete_calls) =
        mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    let snapshot = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(snapshot.path(), "----- тело -----\n\nтело\n").unwrap();
    let err = run_body_replace("таск", Some(snapshot.path()), "новое тело\n")
        .await
        .unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("меняло") && msg.contains("после снапшота") && msg.contains("task comment"),
        "суть отказа и порядок replace → comment: {msg}"
    );
    {
        let appends = append_bodies.lock().unwrap();
        assert!(appends.is_empty(), "записи нет: {:?}", *appends);
    }
    assert_eq!(delete_calls.load(Ordering::SeqCst), 0, "ядро не тронуто");
}

/// --force старше --base: осознанный overwrite идёт без сверки —
/// устаревший снапшот не мешает записи.
#[tokio::test]
#[serial_test::serial]
async fn full_replace_with_force_writes_despite_stale_snapshot() {
    let export = "---\ntitle: таск\n---\n\n# таск\n\nтело\n\nпараллельная правка\n".to_string();
    let (server, _create, append_bodies, _deletes) =
        mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    let snapshot = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(snapshot.path(), "----- тело -----\n\nтело\n").unwrap();
    let md = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(md.path(), "новое тело\n").unwrap();
    task_run(
        "uwuwu",
        TaskSub::Body {
            id: "таск".into(),
            stdin: false,
            md: Some(md.path().to_string_lossy().into_owned()),
            replace: true,
            replace_section: None,
            base: Some(snapshot.path().to_string_lossy().into_owned()),
            force: true,
            attach: Vec::new(),
        },
    )
    .await
    .unwrap();

    {
        let appends = append_bodies.lock().unwrap();
        assert!(
            appends.iter().any(|p| p.contains("новое тело")),
            "force-запись состоялась: {:?}",
            *appends
        );
    }
}

/// Авто-перенос системной секции на живом мок-ядре: existing с комментами,
/// контент без «## Комменты» → запись несёт контент + перенесённую секцию.
#[tokio::test]
#[serial_test::serial]
async fn full_replace_carries_over_comments_section_into_write() {
    let export = "---\ntitle: таск\n---\n\n# таск\n\nтело\n\n## Комменты\n- **AGENTS** (ts): тик\n"
        .to_string();
    let (server, _create, append_bodies, _deletes) =
        mock_siyuan_with_export(vec![BOUND_DOC.to_string()], true, export).await;
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    let snapshot = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
            snapshot.path(),
            "----- тело -----\n\n---\ntitle: таск\n---\n\n# таск\n\nтело\n\n## Комменты\n- **AGENTS** (ts): тик\n",
        )
        .unwrap();
    run_body_replace("таск", Some(snapshot.path()), "переписанное тело\n")
        .await
        .unwrap();

    {
        let appends = append_bodies.lock().unwrap();
        let payload = appends.join("\n");
        assert!(
            payload.contains("переписанное тело"),
            "контент записан: {payload}"
        );
        assert!(
            payload.contains("## Комменты") && payload.contains("- **AGENTS** (ts): тик"),
            "история комментов пережила replace: {payload}"
        );
    }
}

#[test]
fn strip_task_get_output_cuts_card_header_and_attach_tail() {
    let full_get = "Задача:   таск\nСтатус:   Design\nСтрока:   item-1\n\n----- тело -----\n\n---\ntitle: таск\n---\n\nтело\n\nАттачи:\n  /tmp/uwuwu-task-x/скрин.png\n";
    assert_eq!(
        strip_task_get_output(full_get),
        "---\ntitle: таск\n---\n\nтело\n"
    );
    let raw_export = "---\ntitle: таск\n---\n\nтело\n";
    assert_eq!(strip_task_get_output(raw_export), raw_export);
    let clean_body = "тело без разметки\n";
    assert_eq!(strip_task_get_output(clean_body), clean_body);
}

/// Регресс 20261108003111 (BDD-сценарий задачи 20261108045651): тайтл с
/// маркер-подстрокой попадает в шапку get-вывода раньше настоящего
/// маркера — срезка по ПОДСТРОКЕ (split_once) даёт мусорный остаток
/// («…» внутри\nСтатус: …»), сверка --base ложно отказывает. Построчный
/// матч по ровной строке маркера остаток не портит.
#[test]
fn strip_task_get_output_survives_marker_substring_in_header() {
    let full_get = "Задача:   тайтл с «----- тело -----» внутри\nСтатус:   Design\nСтрока:   item-1\n\n----- тело -----\n\n---\ntitle: таск\n---\n\nтело\n";
    let stripped = strip_task_get_output(full_get);
    assert!(
        stripped.starts_with("---\ntitle: таск"),
        "остаток = тело, получено: {stripped:?}"
    );
}

/// Ровная строка маркера внутри код-фенса чистого тела — не граница
/// шапки (зеркало write-side тестов task_get_wrapper_signature): вход
/// проходит насквозь без срезки.
#[test]
fn strip_task_get_output_marker_line_in_fence_is_not_boundary() {
    let clean_body = "текст\n\n```gherkin\n----- тело -----\n```\n\nхвост\n";
    assert_eq!(strip_task_get_output(clean_body), clean_body);
}

/// Строка «Аттачи:» внутри код-фенса тела — не граница хвоста: срезка —
/// по настоящему «Аттачи:» вне фенса.
#[test]
fn strip_task_get_output_attach_line_in_fence_is_not_tail_boundary() {
    let full_get = "Задача:   таск\nСтатус:   Design\n\n----- тело -----\n\nшапка тела\n\n```\nАттачи:\n```\n\nхвост тела\n\nАттачи:\n  /tmp/uwuwu-task-x/скрин.png\n";
    let stripped = strip_task_get_output(full_get);
    assert!(
        stripped.contains("```\nАттачи:\n```"),
        "фенс-«Аттачи:» не граница: {stripped:?}"
    );
    assert!(
        stripped.ends_with("хвост тела\n"),
        "срезано по настоящему хвосту: {stripped:?}"
    );
}

/// CRLF-вход: lines()+trim_end гасит \r — маркер и «Аттачи:» матчатся,
/// ведущие пустые строки после маркера срезаются без \r-остатка.
#[test]
fn strip_task_get_output_handles_crlf_lines() {
    let full_get = "Задача:   таск\r\nСтатус:   Design\r\n\r\n----- тело -----\r\n\r\nтело\r\n\r\nАттачи:\r\n  /tmp/x.png\r\n";
    let stripped = strip_task_get_output(full_get);
    assert!(
        stripped.starts_with("тело"),
        "остаток после CRLF: {stripped:?}"
    );
    assert!(!stripped.contains("Аттачи:"), "хвост срезан: {stripped:?}");
}

#[test]
fn require_fresh_snapshot_accepts_noise_and_refuses_delta() {
    let existing = "тело\n\n## Комменты\n- коммент\n";
    // свежий снапшот с рендер-шумом: сверка нормализованная, не байтовая
    let noisy = "шапка\n----- тело -----\n\nтело\n\n\n## Комменты\n\n\n- коммент\n";
    require_fresh_snapshot("таск", existing, Some(noisy)).unwrap();
    // чужая правка после снапшота
    let stale = "----- тело -----\n\nтело без секции\n";
    let err = require_fresh_snapshot("таск", existing, Some(stale)).unwrap_err();
    assert!(err.to_string().contains("меняло"));
    assert!(err.to_string().contains("после снапшота"));
    // снапшота нет вовсе
    let err = require_fresh_snapshot("таск", existing, None).unwrap_err();
    assert!(err.to_string().contains("--base") && err.to_string().contains("--force"));
}

#[test]
fn carry_over_comments_appends_existing_section_verbatim() {
    let existing = "старое тело\n\n## Комменты\n- **AGENTS** (2026-11-06):  вердикт ^abc123\n  вложенный ответ\n";
    let out = carry_over_comments_section("таск", existing, "новое тело\n").unwrap();
    assert!(out.starts_with("новое тело\n"), "контент основа: {out}");
    assert!(
        out.contains("## Комменты\n- **AGENTS** (2026-11-06):  вердикт ^abc123\n  вложенный ответ")
    );
}

#[test]
fn carry_over_comments_keeps_content_with_its_own_section() {
    let existing = "старое\n\n## Комменты\n- старый коммент\n";
    let out =
        carry_over_comments_section("таск", existing, "новое\n\n## Комменты\n- новая история\n")
            .unwrap();
    assert_eq!(out, "новое\n\n## Комменты\n- новая история\n");
}

#[test]
fn carry_over_comments_refuses_twin_on_other_level() {
    let existing = "старое\n\n## Комменты\n- коммент\n";
    let err = carry_over_comments_section("таск", existing, "новое\n\n### Комменты\n").unwrap_err();
    assert!(err.to_string().contains("h3") && err.to_string().contains("дубль"));
}

#[test]
fn carry_over_comments_noop_without_existing_section() {
    let out = carry_over_comments_section("таск", "старое без секции\n", "новое\n").unwrap();
    assert_eq!(out, "новое\n");
}

/// Фикстура инцидента 08.11 (20261108044740): qlty-секция вставилась после
/// «Комменты» (replace-section «не найдена — в конец»), поздние комменты
/// легли за чужим h2 (task comment аппендит в конец дока).
fn fragmented_existing() -> String {
    "\
старое тело

## Комменты
- **AGENTS** (2026-11-08 10:00): головной коммент

## Машинный ревью (qlty)
находки линтера

- **AGENTS** (2026-11-08 11:00): PR готов к мержу
- **Юра** (2026-11-08 11:30): мержу сам
"
    .to_string()
}

#[test]
fn carry_over_comments_refuses_fragmented_section_tail_loss() {
    let err =
        carry_over_comments_section("таск", &fragmented_existing(), "новое тело\n").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("фрагментирована"), "суть ошибки: {msg}");
    assert!(
        msg.contains("PR готов к мержу") && msg.contains("мержу сам"),
        "перечень хвостовых комментов: {msg}"
    );
    assert!(
        !msg.contains("головной коммент"),
        "головной коммент переносится, в потерях не числится: {msg}"
    );
}

#[test]
fn carry_over_comments_passes_when_tail_comments_inlined_into_content() {
    let content = "\
новое тело

## Машинный ревью (qlty)
находки линтера

- **AGENTS** (2026-11-08 11:00): PR готов к мержу
- **Юра** (2026-11-08 11:30): мержу сам
";
    let out = carry_over_comments_section("таск", &fragmented_existing(), content).unwrap();
    assert!(out.contains("PR готов к мержу"), "хвост сохранён: {out}");
    assert!(
        out.contains("## Комменты\n- **AGENTS** (2026-11-08 10:00): головной коммент"),
        "головная секция перенесена: {out}"
    );
}

#[test]
fn carry_over_comments_allows_conscious_replacement_despite_fragmentation() {
    let out = carry_over_comments_section(
        "таск",
        &fragmented_existing(),
        "новое\n\n## Комменты\n- новая история\n",
    )
    .unwrap();
    assert_eq!(out, "новое\n\n## Комменты\n- новая история\n");
}

#[test]
fn carry_over_comments_tolerates_foreign_tail_without_comment_lines() {
    let existing = "старое\n\n## Комменты\n- **AGENTS** (2026-11-08 10:00): головной\n\n## Машинный ревью (qlty)\nнаходки линтера\n";
    let out = carry_over_comments_section("таск", existing, "новое\n").unwrap();
    assert!(out.contains("головной"), "секция перенесена: {out}");
}

/// Detached-строка (привязки нет) + ≥2 доков с тем же заголовком →
/// явная ошибка со списком docId, не молчаливая запись в «первый».
#[tokio::test]
#[serial_test::serial]
async fn body_errors_listing_duplicates_for_detached_row() {
    let (server, _create, _append, _deletes) =
        mock_siyuan(vec![ORPHAN_DOC.to_string(), "doc-b".to_string()], false).await;
    let err = write_body_via_cli(&server).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("несколько доков"), "суть ошибки: {msg}");
    assert!(
        msg.contains(ORPHAN_DOC) && msg.contains("doc-b"),
        "список docId в ошибке: {msg}"
    );
}

#[test]
fn replace_md_section_replaces_body_keeps_heading() {
    let md = "## А\nстарое\n\n## Б\nвторое\n";
    let (out, edit) = replace_md_section(md, "А", "новое содержимое\n").unwrap();
    assert!(out.contains("## А\nновое содержимое"));
    assert!(out.contains("## Б\nвторое"));
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn replace_md_section_appends_missing_section() {
    let md = "## А\nпервое\n";
    let (out, edit) = replace_md_section(md, "Новая", "контент").unwrap();
    assert!(out.contains("## Новая\nконтент"));
    assert!(out.contains("## А\nпервое"));
    assert_eq!(edit, SectionEdit::Appended);
}

#[test]
fn replaces_first_and_removes_duplicate_sections() {
    let md = "# Таск\n\n## Причина\nпервое вхождение\n\n## Причина\nвторое вхождение\n\n## Фикс\nправка\n";
    let (out, edit) = replace_md_section(md, "Причина", "новый механизм\n").unwrap();
    assert_eq!(out.matches("## Причина").count(), 1, "секция остаётся одна");
    assert!(
        !out.contains("первое вхождение"),
        "старый контент первого удалён"
    );
    assert!(!out.contains("второе вхождение"), "контент дубля удалён");
    assert!(
        out.contains("## Фикс\nправка"),
        "соседняя секция не тронута"
    );
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 1
        }
    );
}

#[test]
fn ends_section_at_higher_level_heading() {
    let md = "## А\nстарое\n# Заголовок H1\nхвост тайтла\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
    assert!(out.contains("# Заголовок H1"), "h1 переживает replace");
    assert!(
        out.contains("хвост тайтла"),
        "контент после h1 переживает replace"
    );
    assert!(out.contains("## Б\nвторое"));
}

#[test]
fn h3_stays_inside_section() {
    let md = "## А\nввод\n### Подсекция\nдетали подсекции\n\n## Б\nвторое\n### Б-под\nб-детали\n";
    let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
    assert!(
        !out.contains("детали подсекции"),
        "h3 не рвёт диапазон: контент под h3 заменяется вместе с секцией"
    );
    assert!(
        out.contains("### Б-под\nб-детали"),
        "h3 чужой секции не тронут"
    );
    assert!(out.contains("## Б\nвторое"));
}

#[test]
fn normalizes_heading_and_argument() {
    let md = "##  Постановка\nстарое\n";
    let (out, edit) = replace_md_section(md, " Постановка ", "новое\n").unwrap();
    assert!(out.contains("новое"), "матч по нормализованному тексту");
    assert_eq!(out.matches("##").count(), 1, "без append-дубля секции");
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn heading_preserved_byte_for_byte() {
    let md = "##  Причина ^20261104120000-ab12cd\nстарое\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "Причина", "новое\n").unwrap();
    assert!(
        out.contains("##  Причина ^20261104120000-ab12cd\nновое"),
        "заматченный заголовок пишется исходной строкой (пробелы и ^id живы)"
    );
    assert_eq!(out.matches("##").count(), 2, "без append-дубля");
}

#[test]
fn matches_heading_with_block_id_suffix() {
    let md = "## Причина ^20261104120000-ab12cd\nстарое\n\n## Б\nвторое\n";
    let (out, edit) = replace_md_section(md, "Причина", "новое\n").unwrap();
    assert!(out.contains("новое"), "суффикс ^id не мешает матчу");
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn fenced_h2_does_not_bound_section() {
    let md = "## А\nстарое\n```md\n## Фейк внутри фенса\n```\nхвост секции\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
    assert!(
        !out.contains("Фейк"),
        "h2 внутри фенса — контент секции, заменяется"
    );
    assert!(!out.contains("хвост секции"), "секция длится сквозь фенс");
    assert!(out.contains("## Б\nвторое"));
}

#[test]
fn hash_line_inside_fence_survives() {
    let md = "## Сценарий (BDD)\nстарое\n```gherkin\nСценарий: x\n  Дано y\n  Тогда z  # факт\n```\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "Сценарий (BDD)", "новое\n").unwrap();
    assert!(
        out.contains("## Б\nвторое"),
        "граница секции — настоящий заголовок Б"
    );
    assert_eq!(out.matches("## Б").count(), 1, "секция Б не задвоена");
    assert!(
        !out.contains("# факт"),
        "старый контент секции заменён целиком"
    );
}

#[test]
fn empty_content_clears_section() {
    let md = "## А\nстарое\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "А", "").unwrap();
    assert!(out.contains("## А"), "заголовок сохранён");
    assert!(!out.contains("старое"), "контент очищен пустым вводом");
    assert!(out.contains("## Б\nвторое"));
}

#[test]
fn blank_line_restored_before_next_heading() {
    let md = "## А\nстарое\n\n## Б\nвторое\n";
    let (out, _) = replace_md_section(md, "А", "новое\n").unwrap();
    assert!(
        out.contains("новое\n\n## Б"),
        "пустая строка между секциями восстановлена"
    );
}

// ——— гард уровня: h2-матча нет, тёзка на уровне 1 или ≥3 → отказ, записи нет ———

#[test]
fn h3_twin_refuses_with_level_and_byte_line() {
    let md = "## Родитель\nввод\n### Подсекция\nдетали подсекции\n";
    let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("h3"), "уровень тёзки в ошибке: {msg}");
    assert!(
        msg.contains("### Подсекция"),
        "байт-точная строка заголовка: {msg}"
    );
    assert!(msg.contains("--replace"), "рецепт обхода в ошибке: {msg}");
}

#[test]
fn h1_twin_refuses_replacement() {
    let md = "# Подсекция\nтело тайтла\n";
    let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("h1"),
        "h1-тёзка гардится тем же правилом: {msg}"
    );
}

#[test]
fn twin_detection_is_normalized_double_space_and_block_id() {
    // регулярный прод-экспорт несёт двойные пробелы и ^block-id: детект
    // тёзки по сырой строке промолчал бы ровно на этом случае
    let md = "## Родитель\nввод\n###  Подсекция ^20261105230000-ab12cd\nдетали\n";
    let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("###  Подсекция ^20261105230000-ab12cd"),
        "байт-точная строка с пробелами и ^id: {msg}"
    );
}

#[test]
fn twin_inside_fence_appends_instead_of_guard() {
    let md = "## Родитель\n```md\n### Подсекция\n```\nхвост\n";
    let (out, edit) = replace_md_section(md, "Подсекция", "новое\n").unwrap();
    assert!(
        out.contains("## Подсекция\nновое"),
        "тёзка в фенсе — не заголовок, гард молчит: легитимный append"
    );
    assert_eq!(edit, SectionEdit::Appended);
}

#[test]
fn h2_match_wins_over_h3_twin_without_guard() {
    let md = "## Подсекция\nстарое\n\n## Другое\nввод\n### Подсекция\nдетали\n";
    let (out, edit) = replace_md_section(md, "Подсекция", "новое\n").unwrap();
    assert!(
        out.contains("## Подсекция\nновое"),
        "h2-матч заменён на месте"
    );
    assert!(
        out.contains("### Подсекция\nдетали"),
        "###-тёзка вне секции не тронута"
    );
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        },
        "гард не стреляет при живом h2"
    );
}

#[test]
fn first_twin_in_document_order_named_in_error() {
    let md = "### Подсекция\nверхняя\n\n# Подсекция\nнижняя\n";
    let err = replace_md_section(md, "Подсекция", "новое\n").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("h3"),
        "первая тёзка по порядку — детерминизм: {msg}"
    );
    assert!(msg.contains("### Подсекция"), "строка первой тёзки: {msg}");
    assert!(!msg.contains("h1"), "вторая тёзка не упоминается: {msg}");
}

// ——— нормализация запроса: ATX-префикс в --replace-section срезается ———

#[test]
fn prefixed_h3_query_refuses_instead_of_garbage_append() {
    // факт 06.11 (задача 20261104072048): запрос «### Замечания…» при живой
    // h3-секции → раньше not-found append «## ### Замечания…» в хвост тела
    let md = "## Финальный отчёт\nввод\n### Замечания и неочевидные решения\nдетали\n\n## Комменты\n- строка\n";
    let err = replace_md_section(md, "### Замечания и неочевидные решения", "новое\n").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("h3"),
        "запрос с префиксом матчит тёзку после среза хэшей: {msg}"
    );
    assert!(
        msg.contains("### Замечания и неочевидные решения"),
        "байт-точная строка: {msg}"
    );
}

#[test]
fn prefixed_h2_query_matches_live_section() {
    let md = "## X\nстарое\n\n## Б\nвторое\n";
    let (out, edit) = replace_md_section(md, "## X", "новое\n").unwrap();
    assert!(
        out.contains("## X\nновое"),
        "запрос «## X» матчит живую секцию «X»"
    );
    assert!(out.contains("## Б\nвторое"));
    assert_eq!(out.matches("## X").count(), 1, "без дубля секции");
    assert_eq!(
        edit,
        SectionEdit::Replaced {
            duplicates_removed: 0
        }
    );
}

#[test]
fn prefixed_query_appends_clean_heading() {
    let md = "## А\nтело\n";
    let (out, edit) = replace_md_section(md, "### Новая", "контент").unwrap();
    assert!(
        out.contains("## Новая\nконтент"),
        "append пишет чистый заголовок без хэшей запроса"
    );
    assert!(
        !out.contains("## ###"),
        "двойного префикса в теле нет: {out}"
    );
    assert_eq!(edit, SectionEdit::Appended);
}

#[test]
fn collect_threads_prints_normalized_section() {
    let md = "## Постановка\nтело\n\n##  Комменты\n- **AGENTS**: вопрос\n";
    let out = collect_threads(md);
    assert!(
        out.contains("##  Комменты"),
        "заголовок с двойным пробелом матчится"
    );
    assert!(out.contains("- **AGENTS**: вопрос"));
    assert!(!out.contains("тело"));
}

#[test]
fn collect_threads_ends_at_h1_boundary() {
    let md = "## Комменты\n- коммент\n# Тег\nтекст после тега\n";
    let out = collect_threads(md);
    assert!(out.contains("- коммент"));
    assert!(!out.contains("# Тег"), "h1 закрывает секцию");
    assert!(!out.contains("текст после тега"));
}

#[test]
fn collect_threads_prints_both_duplicate_sections() {
    let md = "## Комменты\n- первый\n\n## Комменты\n- второй\n";
    let out = collect_threads(md);
    assert!(out.contains("- первый"));
    assert!(
        out.contains("- второй"),
        "дубли секций печатаются обе — read-путь ничего не роняет"
    );
}

#[test]
fn collect_threads_prints_hash_tag_inside_section() {
    let md = "## Комменты\n- коммент\n#bug\n- коммент после тега\n";
    let out = collect_threads(md);
    assert!(
        out.contains("#bug"),
        "#bug без пробела — не заголовок, секцию не закрывает"
    );
    assert!(out.contains("- коммент после тега"));
}

#[test]
fn collect_threads_reports_empty() {
    assert_eq!(collect_threads("## А\nтело\n").trim_end(), "комментов нет");
}

#[test]
fn read_base_snapshot_multiline_arg_bails_with_base_hint_not_os_error() {
    let err = read_base_snapshot(Some("снапшот задачи\nвторая строка"))
        .expect_err("многострочный аргумент --base — это текст, не путь");
    let shown = err.to_string();
    assert!(shown.contains("--base ждёт путь к файлу"), "суть: {shown}");
    assert!(
        !shown.contains("os error"),
        "сырая OS-ошибка не годится: {shown}"
    );
}

#[test]
fn read_base_snapshot_valid_path_reads_content_and_absent_is_none() {
    let md = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(md.path(), "снапшот-контент").unwrap();
    let snapshot = read_base_snapshot(Some(md.path().to_str().unwrap())).unwrap();
    assert_eq!(snapshot.as_deref(), Some("снапшот-контент"));
    assert!(read_base_snapshot(None).unwrap().is_none());
}

/// Мок под body-путь: bound-строка «таск» (resolve id-first), upload и
/// appendBlock пишут в общий лог порядка, createDocWithMd фиксируется
/// (append-режим не должен его звать).
async fn mock_siyuan_for_body() -> (
    MockServer,
    Arc<Mutex<Vec<&'static str>>>,
    Arc<Mutex<Vec<String>>>,
) {
    let server = MockServer::start().await;
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let appends: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    mount_body_asset_upload(&server, &order).await;
    mount_body_read_apis(&server).await;
    mount_body_write_apis(&server, &order, &appends).await;
    mount_body_mcp(&server).await;

    (server, order, appends)
}

async fn body_via_cli(
    server: &MockServer,
    md: Option<&std::path::Path>,
    attach: &[std::path::PathBuf],
) -> anyhow::Result<()> {
    std::env::set_var("UWUWU_SIYUAN_URL", server.uri());
    std::env::set_var("UWUWU_SIYUAN_TOKEN", "test-token");
    task_run(
        "uwuwu",
        TaskSub::Body {
            id: "таск".into(),
            stdin: false,
            md: md.map(|p| p.to_string_lossy().into_owned()),
            replace: false,
            replace_section: None,
            base: None,
            force: false,
            attach: attach.to_vec(),
        },
    )
    .await
}

#[tokio::test]
#[serial_test::serial]
async fn body_with_attach_uploads_then_content_then_refs() {
    let (server, order, appends) = mock_siyuan_for_body().await;
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("скрин до.png");
    std::fs::write(&img, b"png").unwrap();
    let md = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(md.path(), "маркер-контент\n").unwrap();

    body_via_cli(&server, Some(md.path()), &[img])
        .await
        .expect("body --attach с контентом ok");

    assert_eq!(
        *order.lock().unwrap(),
        vec!["upload", "append", "append"],
        "порядок: upload → контент-операция → референсы; createDoc не зовётся"
    );
    let appends = appends.lock().unwrap();
    assert!(
        appends[0].contains("маркер-контент"),
        "первый append — контент: {}",
        appends[0]
    );
    assert!(
        appends[1].contains("![скрин до.png](assets/"),
        "второй append — референс картинки: {}",
        appends[1]
    );
}

#[tokio::test]
#[serial_test::serial]
async fn body_attach_only_skips_empty_content_write_and_appends_refs() {
    let (server, order, appends) = mock_siyuan_for_body().await;
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("после.png");
    std::fs::write(&img, b"png").unwrap();

    body_via_cli(&server, None, &[img])
        .await
        .expect("attach-only вызов валиден");

    assert_eq!(
        *order.lock().unwrap(),
        vec!["upload", "append"],
        "пустой контент-оп не делается: без пустого appendBlock перед референсами"
    );
    let appends = appends.lock().unwrap();
    assert_eq!(appends.len(), 1, "единственный append — референсы");
    assert!(
        !appends[0].contains("\"data\":\"\""),
        "в appendBlock не уходит пустой data: {}",
        appends[0]
    );
    let refs_md: String = serde_json::from_str::<serde_json::Value>(&appends[0])
        .expect("appendBlock body — json")
        .get("data")
        .and_then(|d| d.as_str())
        .expect("data — markdown референсов")
        .to_string();
    assert_eq!(
        collect_asset_refs(&refs_md),
        vec!["после-20261106090000-abc1234.png".to_string()],
        "round-trip референса attach-only: {refs_md:?}"
    );
}

/// upload-хендлер: порядок «upload» + assets-путь с hash-суффиксом.
async fn mount_body_asset_upload(server: &MockServer, order: &Arc<Mutex<Vec<&'static str>>>) {
    Mock::given(method("POST"))
        .and(path("/api/asset/upload"))
        .respond_with({
            let order = order.clone();
            move |req: &wiremock::Request| {
                order.lock().unwrap().push("upload");
                let body = String::from_utf8_lossy(&req.body);
                let name = body
                    .split("filename=\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .unwrap_or("f");
                let (stem, ext) = match name.rsplit_once('.') {
                    Some((stem, ext)) => (stem.to_string(), ext.to_string()),
                    None => (name.to_string(), "bin".to_string()),
                };
                ResponseTemplate::new(200).set_body_json(json!({
                    "code": 0,
                    "data": { "succFiles": [{ "index": 0, "name": name,
                              "path": format!("assets/{stem}-20261106090000-abc1234.{ext}") }] }
                }))
            }
        })
        .mount(server)
        .await;
}

/// read-only API стенда: ноутбук, av-носитель доски, поиск по hpath.
async fn mount_body_read_apis(server: &MockServer) {
    Mock::given(method("POST"))
            .and(path("/api/notebook/lsNotebooks"))
            .respond_with(|_req: &wiremock::Request| {
                ResponseTemplate::new(200).set_body_string(
                    json!({ "code": 0, "data": { "notebooks": [{ "id": "nb-1", "name": "Projects" }] } })
                        .to_string(),
                )
            })
            .mount(server)
            .await;
    Mock::given(method("POST"))
            .and(path("/api/query/sql"))
            .respond_with(|req: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let stmt = body.get("stmt").and_then(|s| s.as_str()).unwrap_or("");
                let data = if stmt.contains("type='av'") {
                    serde_json::json!([{ "id": CARRIER,
                        "markdown": format!("<div data-type=\"NodeAttributeView\" data-av-id=\"{AV_ID}\"></div>") }])
                } else {
                    serde_json::json!([])
                };
                ResponseTemplate::new(200).set_body_string(
                    serde_json::json!({ "code": 0, "data": data }).to_string(),
                )
            })
            .mount(server)
            .await;
    Mock::given(method("POST"))
        .and(path("/api/filetree/getIDsByHPath"))
        .respond_with(|req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let hpath = body.get("path").and_then(|p| p.as_str()).unwrap_or("");
            let data = if hpath == "/uwuwu/Задачи uwuwu" {
                serde_json::json!([BOARD_DOC])
            } else {
                serde_json::json!([])
            };
            ResponseTemplate::new(200)
                .set_body_string(serde_json::json!({ "code": 0, "data": data }).to_string())
        })
        .mount(server)
        .await;
}

/// write-API стенда: createDocWithMd («create») и appendBlock («append»).
async fn mount_body_write_apis(
    server: &MockServer,
    order: &Arc<Mutex<Vec<&'static str>>>,
    appends: &Arc<Mutex<Vec<String>>>,
) {
    Mock::given(method("POST"))
        .and(path("/api/filetree/createDocWithMd"))
        .respond_with({
            let order = order.clone();
            move |_req: &wiremock::Request| {
                order.lock().unwrap().push("create");
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":"doc-new"}"#)
            }
        })
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/block/appendBlock"))
        .respond_with({
            let order = order.clone();
            let appends = appends.clone();
            move |req: &wiremock::Request| {
                order.lock().unwrap().push("append");
                appends
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&req.body).to_string());
                ResponseTemplate::new(200).set_body_string(r#"{"code":0,"data":null}"#)
            }
        })
        .mount(server)
        .await;
}

/// MCP-роутер стенда: initialize/initialized, block.get, render (пагинация).
async fn mount_body_mcp(server: &MockServer) {
    Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(|req: &wiremock::Request| {
                let body = String::from_utf8_lossy(&req.body);
                if body.contains("\"initialize\"") {
                    return ResponseTemplate::new(200)
                        .set_body_string(
                            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"mock","version":"3"}}}"#,
                        )
                        .append_header("mcp-session-id", "sess-1");
                }
                if body.contains("notifications/initialized") {
                    return ResponseTemplate::new(200);
                }
                if body.contains("tools/call") {
                    let payload = if body.contains("\"block\"") && body.contains("\"get\"") {
                        // doc_block_exists: bound-док жив
                        format!("ID: {BOUND_DOC}\nType: NodeDocument\nHPath: /uwuwu/Задачи uwuwu/таск")
                    } else if body.contains("\"render\"") {
                        let page_done = body.contains("\"page\":2");
                        let rows = if page_done {
                            json!([])
                        } else {
                            json!([{ "id": ROW_ID, "cells": [{ "value": { "keyID": TITLE_KEY,
                                "type": "block",
                                "block": { "content": "таск", "id": BOUND_DOC, "refSubtype": "d" } } }] }])
                        };
                        json!({ "action": "render", "data": { "view": {
                            "columns": [{ "id": TITLE_KEY, "name": "Primary Key" }],
                            "rows": rows } } }).to_string()
                    } else {
                        json!({ "action": "unknown", "data": null }).to_string()
                    };
                    return ResponseTemplate::new(200).set_body_string(
                        serde_json::json!({
                            "jsonrpc": "2.0", "id": 2,
                            "result": { "content": [{ "type": "text", "text": payload }] }
                        })
                        .to_string(),
                    );
                }
                ResponseTemplate::new(200).set_body_string(
                    r#"{"jsonrpc":"2.0","error":{"code":-1,"message":"unexpected"}}"#,
                )
            })
            .mount(server)
            .await;
}
