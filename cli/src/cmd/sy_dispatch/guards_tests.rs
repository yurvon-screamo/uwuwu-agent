//! Тесты гвардов входа: comment-пред-гвард (trailing_var_arg), path-guard
//! (честные отказы на не-путевых входах).

use super::guards::guarded_comment_text;
use super::guards::pre_guard_positional_comment;

use super::task::task_run;
use super::guards::validate_comment_text;

use crate::cmd::cli::TaskSub;

use super::guards::read_body_input;
use super::attach::validate_attach_files;

#[test]
fn comment_guard_rejects_subcommand_name_as_text() {
    let err = validate_comment_text(false, "list").expect_err("«list» — подкоманда, не текст");
    assert!(err.to_string().contains("подкоманд"), "суть отказа: {err}");
    assert!(
        err.to_string().contains("task get/thread"),
        "подсказка чтения: {err}"
    );
}

#[test]
fn comment_guard_rejects_flag_swallowed_after_text() {
    let err = validate_comment_text(false, "текст --project x")
        .expect_err("флаг после текста глотается trailing_var_arg");
    assert!(err.to_string().contains("флаг"), "суть отказа: {err}");
    assert!(err.to_string().contains("--stdin"), "обход указан: {err}");
}

#[test]
fn comment_guard_rejects_dash_prefixed_word() {
    let err = validate_comment_text(false, "-n").expect_err("-n выглядит как флаг");
    assert!(err.to_string().contains("флаг"), "суть отказа: {err}");
}

#[test]
fn comment_guard_allows_markdown_bullets() {
    validate_comment_text(false, "- пункт один\n- ещё пункт")
        .expect("markdown-буллет — контент, не флаг");
}

#[test]
fn comment_guard_allows_minus_number_and_lone_dash() {
    validate_comment_text(false, "-5 баллов").expect("отрицательное число — контент");
    validate_comment_text(false, "-").expect("одиночный дефис — контент");
}

#[test]
fn comment_guard_rejects_empty_text() {
    let err = validate_comment_text(false, "").expect_err("пустой коммент — всегда баг");
    assert!(err.to_string().contains("пустой"), "суть отказа: {err}");
    validate_comment_text(true, "   ").expect_err("пустой stdin-коммент тоже отказан");
}

#[test]
fn comment_guard_skips_stdin_source() {
    validate_comment_text(true, "--не-флаг-а-текст").expect("stdin — явное намерение, не гвардим");
    validate_comment_text(true, "list из пайпа — тоже контент")
        .expect("stdin: подкоманды не гвардятся");
}

// ——— канал --md: явный контент без токен-гвардов, отказы чтения с именем пути ———

#[test]
fn comment_md_flag_like_content_passes_guard() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.md");
    std::fs::write(
        &file,
        "# Отчёт\n\nкоманда `bash -n x.sh` и флаг --frozen-lockfile в прозе\n",
    )
    .unwrap();
    let msg = guarded_comment_text(false, Some(file.to_str().unwrap()), &[])
        .expect("флагоподобные строки в файле — явный канал, гвард не режет");
    assert!(msg.contains("--frozen-lockfile"), "контент целиком: {msg}");
}

#[test]
fn comment_md_empty_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("empty.md");
    std::fs::write(&file, "   \n").unwrap();
    let err = guarded_comment_text(false, Some(file.to_str().unwrap()), &[])
        .expect_err("пустой файл — пустой коммент, отказ во всех каналах");
    assert!(err.to_string().contains("пустой"), "суть отказа: {err}");
}

#[test]
fn comment_md_missing_file_error_names_the_path() {
    let err = guarded_comment_text(false, Some("/no/such/report.md"), &[])
        .expect_err("нет файла — отказ");
    assert!(
        err.to_string().contains("/no/such/report.md"),
        "имя пути в отказе: {err}"
    );
}

#[test]
fn comment_md_directory_error_names_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let err = guarded_comment_text(false, Some(dir.path().to_str().unwrap()), &[])
        .expect_err("директория — не файл, отказ");
    assert!(
        err.to_string().contains(dir.path().to_str().unwrap()),
        "имя пути в отказе: {err}"
    );
}

#[test]
fn comment_md_with_positional_text_is_loud_conflict() {
    let err = guarded_comment_text(false, Some("/no/such/f.md"), &["текст".to_string()])
        .expect_err("--md и позиционный текст одновременно — конфликт каналов");
    assert!(
        err.to_string().contains("одним каналом"),
        "суть отказа: {err}"
    );
}

/// Пин фикса High ревью плана: пред-гвард при --md — no-op даже с пустым
/// позиционным (empty-bail пред-гварда не должен убить main-сценарий
/// `task comment <id> --md файл.md` до Comment-ветки).
#[test]
fn comment_pre_guard_is_noop_when_md_channel_set() {
    pre_guard_positional_comment(false, Some("f.md"), &[])
        .expect("--md — явный канал, пред-гвард no-op");
    pre_guard_positional_comment(true, None, &[]).expect("stdin — как раньше");
}

// ——— порядок диспетчера: гвард текста раньше bail по --project ———

/// `task comment <id> …` при пустом --project: оба пути падают до
/// SiYuan::from_env (гвард/байл — первые строки task_run), сети нет.
fn comment_command(stdin: bool, text: &[&str]) -> TaskSub {
    TaskSub::Comment {
        id: "20261104072048-4n1rwdt".into(),
        reply: None,
        stdin,
        md: None,
        text: text.iter().map(|s| s.to_string()).collect(),
    }
}

/// Факт AGENTS-коммента задачи (крон-сессия 04.11, ×3): `task comment <id>
/// "текст" --project X` — флаг проглочен trailing_var_arg'ом → project=None →
/// bail «--project обязателен» маскировал причину под «флаг не передан».
/// Гвард должен стрелять РАНЬШЕ bail по --project.
#[tokio::test]
async fn comment_guard_fires_before_project_bail_on_swallowed_flag() {
    let err = task_run("", comment_command(false, &["текст", "--project", "X"]))
        .await
        .expect_err("флаг в позиционном тексте — отказ гварда");
    let msg = err.to_string();
    assert!(
        msg.contains("похож на флаг"),
        "реальная причина в ошибке: {msg}"
    );
    assert!(
        !msg.contains("обязателен"),
        "маскировка «флаг не передан» недопустима: {msg}"
    );
}

/// Пинну приоритета (замечание ревью плана): подкоманда-как-текст при
/// ОТСУТСТВУЮЩЕМ --project — тоже гвард раньше bail («list» вместо
/// «--project обязателен»).
#[tokio::test]
async fn comment_guard_fires_before_project_bail_on_subcommand_token() {
    let err = task_run("", comment_command(false, &["list"]))
        .await
        .expect_err("подкоманда-как-текст — отказ гварда");
    let msg = err.to_string();
    assert!(msg.contains("подкоманд"), "сообщение гварда: {msg}");
    assert!(
        !msg.contains("обязателен"),
        "приоритет ошибок сменился обратно: {msg}"
    );
}

/// Контроль: чистый позиционный текст при пустом --project — гвард
/// пропускает, команда уходит в глобальный резолв (bail «--project
/// обязателен» для id-семейства снят задачей 20261106230303; промах
/// резолва честный, не «флаг не передан»).
#[tokio::test]
async fn clean_comment_text_without_project_goes_to_global_resolve() {
    let err = task_run("", comment_command(false, &["текст"]))
        .await
        .expect_err("глобальный резолв без ядра/досок — отказ");
    let msg = err.to_string();
    assert!(
        !msg.contains("похож на флаг") && !msg.contains("подкоманд"),
        "чистый текст гвардом не отклоняется: {msg}"
    );
    assert!(
        !msg.contains("обязателен"),
        "bail «--project обязателен» для comment снят: {msg}"
    );
}

#[test]
fn read_body_input_multiline_md_arg_bails_with_stdin_hint_not_os_error() {
    let err = read_body_input(Some("строка один\nстрока два"), false, false)
        .expect_err("многострочный аргумент --md — это текст, не путь");
    let shown = err.to_string();
    assert!(shown.contains("--md ждёт путь к файлу"), "суть: {shown}");
    assert!(
        shown.contains("--stdin"),
        "подсказка самоисправления: {shown}"
    );
    assert!(shown.contains("строка один"), "начало значения: {shown}");
    assert!(
        !shown.contains("os error"),
        "сырая OS-ошибка не годится: {shown}"
    );
}

#[test]
fn read_body_input_over_name_max_md_arg_bails_with_stdin_hint_not_name_too_long() {
    let long = "слово ".repeat(120); // одним компонентом длиннее NAME_MAX — ENAMETOOLONG
    let err = read_body_input(Some(long.as_str()), false, false)
        .expect_err("имя длиннее NAME_MAX — это текст, не путь");
    let shown = err.to_string();
    assert!(shown.contains("--md ждёт путь к файлу"), "суть: {shown}");
    assert!(
        shown.contains("--stdin"),
        "подсказка самоисправления: {shown}"
    );
    assert!(
        !shown.contains("os error"),
        "File name too long не годится: {shown}"
    );
    assert!(
        shown.chars().count() < 300,
        "диагностика не тащит весь аргумент: len={}",
        shown.chars().count()
    );
}

#[test]
fn read_body_input_missing_md_path_keeps_honest_enoent_with_stdin_hint() {
    let err = read_body_input(Some("/nonexistent/нет-такого-файла.md"), false, false)
        .expect_err("нечитаемый путь — отказ");
    let full = format!("{err:#}"); // headline + caused by
    assert!(
        full.contains("No such file or directory"),
        "честная причина не маскируется: {full}"
    );
    assert!(
        err.to_string().contains("--stdin"),
        "непрерывающая подсказка сверху: {full}"
    );
}

#[test]
fn read_body_input_valid_md_path_reads_content() {
    let md = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(md.path(), "контент тела\n").unwrap();
    let content = read_body_input(Some(md.path().to_str().unwrap()), false, false).unwrap();
    assert_eq!(content, "контент тела\n");
}

#[test]
fn validate_attach_files_inline_text_bails_with_attach_hint_not_os_error() {
    let err = validate_attach_files(&["первая строка\nвторая".into()])
        .expect_err("многострочный аргумент --attach — это текст, не путь");
    let shown = err.to_string();
    assert!(
        shown.contains("--attach ждёт путь к файлу"),
        "суть: {shown}"
    );
    assert!(shown.contains("первая строка"), "начало значения: {shown}");
    assert!(
        !shown.contains("os error"),
        "сырая OS-ошибка не годится: {shown}"
    );
}
