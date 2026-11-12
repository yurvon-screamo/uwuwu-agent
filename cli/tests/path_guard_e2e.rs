//! Гвард «путь-флаг получил не-путь» (задача 20261111065035-3lu602i, PR #229):
//! процессный контракт CLI — не-путевой вход в `--md`/`--attach`/`--base`
//! отдаёт exit 1 и самоисправляющий отказ в stderr вместо сырой OS-ошибки
//! `open()`.
//!
//! Уровень процесса, не функции: юниты `sy_dispatch::tests` зовут
//! `read_body_input`/`validate_attach_files`/`read_base_snapshot` напрямую и
//! не доказывают, что аргумент командной строки вообще долетает до гварда
//! (clap-диспатч может отрезать раньше: positional-ловушка `person replace`).
//! Здесь — живой бинарь. Гвард стоит до сети (sy_dispatch.rs:688–716,
//! resolve_task :717), поэтому отказные кейсы не требуют живого ядра.
//!
//! Машина-нюанс: cli резолвит UWUWU_SIYUAN_TOKEN рано, до dispatch — на
//! машине без fish vars/conf.json (CI-runner) отказ «токен не найден»
//! стреляет раньше гварда. Отказные кейсы сами ставят dummy-токен и
//! недостижимый URL: резолв проходит, гвард отказывает до сети — если
//! регрессия утащит вход за гвард, connect-ошибка честно уронит тест.
//!
//! Гейтнутый тест (`UWUWU_E2E=1`) добивает обратную сторону: валидный путь
//! проходит гвард до адресации задачи («не найдена», не подсказка) —
//! read-only resolve на живом ядре (env вызывающего: токен/URL не
//! подменяются), без мутаций доски.
//!
//! Запуск: cargo test (отказные — всегда); UWUWU_E2E=1 cargo test — плюс
//! валидные пути.

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_uwuwu-cli")
}

/// Отказной прогон: env без живого ядра (dummy-токен, URL в никуда) — см.
/// док-коммент модуля.
fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(bin())
        .args(args)
        .env("UWUWU_SIYUAN_TOKEN", "dummy-path-guard-test")
        .env("UWUWU_SIYUAN_URL", "http://127.0.0.1:1")
        .output()
        .expect("spawn uwuwu-cli");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Гейтнутый прогон: env вызывающего как есть (живое ядро).
fn run_live(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(bin())
        .args(args)
        .output()
        .expect("spawn uwuwu-cli");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").as_deref() == Ok("1")
}

/// Главный кейс постановки: многострочный текст в `--md` — отказ с подсказкой
/// `--stdin`, не «File name too long».
#[test]
fn task_body_multiline_md_bails_with_stdin_hint_at_process_level() {
    let (code, stdout, stderr) = run(&[
        "task",
        "body",
        "nosuchtask-zzz",
        "--project",
        "uwuwu",
        "--md",
        "строка один\nстрока два",
    ]);
    assert_eq!(code, Some(1), "exit-код отказа: stderr={stderr}");
    assert!(
        stdout.is_empty(),
        "отказ идёт в stderr, stdout пуст: {stdout}"
    );
    assert!(
        stderr.contains("--md ждёт путь к файлу"),
        "суть отказа: {stderr}"
    );
    assert!(
        stderr.contains("--stdin"),
        "подсказка самоисправления: {stderr}"
    );
    assert!(
        stderr.contains("строка один"),
        "начало offending-значения: {stderr}"
    );
    assert!(
        !stderr.contains("os error"),
        "сырая OS-ошибка не годится: {stderr}"
    );
}

/// Длинный однострочник (имя-компонент за NAME_MAX) — тот же отказ, диагностика
/// несёт префикс, не весь аргумент (дельта-замечание ревью, коммит 5fd7ca9).
#[test]
fn task_body_over_name_max_md_short_diagnostic_at_process_level() {
    let long = "слово ".repeat(120); // ~720 байт одним компонентом
    let (code, _, stderr) = run(&[
        "task",
        "body",
        "nosuchtask-zzz",
        "--project",
        "uwuwu",
        "--md",
        long.as_str(),
    ]);
    assert_eq!(code, Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("--md ждёт путь к файлу") && stderr.contains("--stdin"),
        "самоисправляющий отказ: {stderr}"
    );
    assert!(
        !stderr.contains("File name too long") && !stderr.contains("os error"),
        "ENAMETOOLONG не просачивается: {stderr}"
    );
    let len = stderr.chars().count();
    assert!(
        len < 300,
        "диагностика не тащит весь аргумент: len={len}: {stderr}"
    );
}

/// Опечатанный путь — честная OS-причина не маскируется: ENOENT в caused by,
/// подсказка сверху (инвариант из плана/ревью).
#[test]
fn task_body_missing_md_path_honest_enoent_with_hint_at_process_level() {
    let (code, _, stderr) = run(&[
        "task",
        "body",
        "nosuchtask-zzz",
        "--project",
        "uwuwu",
        "--md",
        "/nonexistent/нет-такого-файла.md",
    ]);
    assert_eq!(code, Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("No such file or directory"),
        "честная причина видна: {stderr}"
    );
    assert!(
        stderr.contains("--stdin"),
        "непрерывающая подсказка: {stderr}"
    );
}

/// `--attach` с текстом вместо пути — отказ называет флаг (task body носитель).
#[test]
fn task_body_attach_inline_text_names_flag_at_process_level() {
    let (code, _, stderr) = run(&[
        "task",
        "body",
        "nosuchtask-zzz",
        "--project",
        "uwuwu",
        "--attach",
        "первая строка\nвторая",
    ]);
    assert_eq!(code, Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("--attach ждёт путь к файлу"),
        "флаг назван: {stderr}"
    );
    assert!(
        stderr.contains("первая строка") && !stderr.contains("os error"),
        "начало значения без OS-мусора: {stderr}"
    );
}

/// `--attach` на create — тот же гвард на второй точке вызова.
#[test]
fn task_create_attach_inline_text_names_flag_at_process_level() {
    let (code, _, stderr) = run(&[
        "task",
        "create",
        "--project",
        "uwuwu",
        "--title",
        "zz-path-guard",
        "--attach",
        "раз\nдва",
    ]);
    assert_eq!(code, Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("--attach ждёт путь к файлу"),
        "гвард create: {stderr}"
    );
}

/// `--base` с текстом — отказ с per-flag подсказкой; при валидном `--md`
/// доходит именно до base-гварда (fail fast по порядку носителей).
#[test]
fn task_body_base_inline_text_names_flag_at_process_level() {
    let md_missing = [
        "task",
        "body",
        "nosuchtask-zzz",
        "--project",
        "uwuwu",
        "--replace",
        "--md",
        "/nonexistent/нет-такого.md",
        "--base",
        "снапшот задачи\nвторая строка",
    ];
    let (code, _, stderr) = run(&md_missing);
    // --md раньше --base: ENOENT с подсказкой — порядок fail fast сохранён
    assert_eq!(code, Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("--md ждёт путь к файлу") && stderr.contains("No such file"),
        "fail fast: --md стреляет раньше --base: {stderr}"
    );

    let valid_manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let md_valid_base_text = [
        "task",
        "body",
        "nosuchtask-zzz",
        "--project",
        "uwuwu",
        "--replace",
        "--md",
        valid_manifest,
        "--base",
        "снапшот задачи\nвторая строка",
    ];
    let (code, _, stderr) = run(&md_valid_base_text);
    assert_eq!(code, Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("--base ждёт путь к файлу"),
        "до base-гварда дошли: {stderr}"
    );
    assert!(!stderr.contains("os error"), "без OS-мусора: {stderr}");
}

/// `person replace --md` — второй носитель read_body_input: гвард наследуется.
#[test]
fn person_replace_md_inline_text_bails_at_process_level() {
    let (code, _, stderr) = run(&["person", "replace", "--md", "строка один\nстрока два"]);
    assert_eq!(code, Some(1), "stderr={stderr}");
    assert!(
        stderr.contains("--md ждёт путь к файлу") && stderr.contains("--stdin"),
        "тот же отказ на person replace: {stderr}"
    );
}

/// Гейтнутый (живое ядро, read-only): валидные пути проходят все три гварда
/// до адресации — отказ «задача не найдена» доказывает, что гвард пропустил
/// (не подсказка, не FS-ошибка). Один файл на все три роли: гварды смотрят
/// форму входа, не содержимое.
#[test]
fn valid_paths_reach_task_resolution_on_live_kernel() {
    if !e2e_enabled() {
        return;
    }
    let tmp = std::env::temp_dir().join("uwuwu-path-guard-valid.md");
    std::fs::write(&tmp, "контент тела\n").unwrap();
    let path = tmp.to_string_lossy().into_owned();

    let cases: [(&str, Vec<&str>); 2] = [
        (
            "--md + --base (replace)",
            vec![
                "task",
                "body",
                "nosuchtask-zzz",
                "--project",
                "uwuwu",
                "--replace",
                "--md",
                path.as_str(),
                "--base",
                path.as_str(),
            ],
        ),
        (
            "--attach",
            vec![
                "task",
                "body",
                "nosuchtask-zzz",
                "--project",
                "uwuwu",
                "--attach",
                path.as_str(),
            ],
        ),
    ];

    for (what, args) in &cases {
        let (code, _, stderr) = run_live(args);
        assert_eq!(code, Some(1), "{what}: stderr={stderr}");
        assert!(
            stderr.contains("не найдена"),
            "{what}: гвард пропустил валидный путь — отказ от адресации, не от гварда: {stderr}"
        );
        assert!(
            !stderr.contains("ждёт путь к файлу"),
            "{what}: гвард ложно сработал на валидном пути: {stderr}"
        );
    }
    let _ = std::fs::remove_file(&tmp);
}
