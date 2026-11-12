//! Общие гварды входа команд: path-guard (ADR-094 — «путь-флаг получил
//! не-путь»: честный ENOENT, пин приоритета ошибок) и comment-пред-гвард
//! (trailing_var_arg, PR #77).

use anyhow::{bail, Context, Result};

/// Не-путь в путь-флаге: перевод строки (вставленный многострочник) либо
/// длина за ядерными лимитами Linux — компонент имени длиннее NAME_MAX или
/// путь длиннее PATH_MAX физически не существует как файл (ENAMETOOLONG),
/// значит это вставленный текст. Ноль ложных срабатываний: легитимный путь
/// бывает длиннее порога только суммарно, покомпонентно — никогда.
pub(super) fn looks_like_inline_text(value: &str) -> bool {
    const NAME_MAX_BYTES: usize = 255;
    const PATH_MAX_BYTES: usize = 4096;
    value.contains('\n')
        || value
            .split('/')
            .any(|component| component.len() > NAME_MAX_BYTES)
        || value.len() > PATH_MAX_BYTES
}

/// Отказ «путь-флаг получил не-путь»: имя флага, префикс offending-значения
/// и подсказка — агент мгновенно самопоправляется вместо дебага сырой
/// OS-ошибки open() («File name too long»).
pub(super) fn not_a_path_error(flag: &str, value: &str, hint: &str) -> anyhow::Error {
    // префикс, не весь аргумент: значения, пойманные детектором по длине,
    // иначе уносят в диагностику килобайты текста и топят подсказку
    const PREFIX_CHARS: usize = 100;
    let first_line = value.lines().next().unwrap_or_default();
    let shown: String = first_line.chars().take(PREFIX_CHARS).collect();
    anyhow::anyhow!(
        "{flag} ждёт путь к файлу, а передан, похоже, сам текст \
         («{shown}…»); {hint}"
    )
}

pub(super) fn read_body_input(
    md_file: Option<&str>,
    stdin: bool,
    attach_present: bool,
) -> Result<String> {
    if let Some(f) = md_file {
        if looks_like_inline_text(f) {
            return Err(not_a_path_error(
                "--md",
                f,
                "сам текст подавай через --stdin",
            ));
        }
        // честная причина не маскируется: подсказка сверху, OS-ошибка в caused by
        return std::fs::read_to_string(f).map_err(|err| {
            anyhow::Error::new(err).context(
                "--md ждёт путь к файлу; если хотел передать текст — подавай его через --stdin",
            )
        });
    }
    if stdin {
        return read_stdin();
    }
    if attach_present {
        // только аттачи, контента нет — валидный вызов
        return Ok(String::new());
    }
    anyhow::bail!("нужно --stdin или --md <файл>")
}

pub(super) fn read_stdin() -> Result<String> {
    use std::io::Read;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

pub(super) fn collect_text(stdin: bool, text: &[String]) -> Result<String> {
    if stdin {
        read_stdin()
    } else {
        Ok(text.join(" "))
    }
}

/// Подкоманды task/cron: токен текста, совпадающий с именем, — почти наверняка
/// перепутанный вызов, а не контент (факт ses_e7b5a366: «list» ушёл юзеру
/// комментом вместо чтения треда). Токены сравниваются целиком: «get:» или
/// «get-задача» — контент.
const TASK_SUB_NAMES: &[&str] = &[
    "create", "list", "get", "edit", "claim", "comment", "thread", "close", "body", "rm", "clean",
    "undo", "set", "pause", "resume",
];

/// Токен выглядит как флаг: `--…` всегда; одиночный дефис — только перед
/// буквой (`-n`, `-Prb`), чтобы не трогать markdown-буллеты («- пункт») и
/// отрицательные числа («-5»).
pub(super) fn looks_like_flag(token: &str) -> bool {
    if token.starts_with("--") {
        return true;
    }
    token
        .strip_prefix('-')
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_alphabetic())
}

/// Текст коммента + гвард до сети: подозрительный позиционный текст — отказ
/// с подсказкой вместо тихой отправки юзеру. Сама валидация —
/// validate_comment_text (тестируется на строках, stdin не нужен).
/// --md и позиционный текст одновременно — конфликт каналов, громкий отказ
/// до чтения файла и сети.
pub(super) fn guarded_comment_text(
    stdin: bool,
    md: Option<&str>,
    text: &[String],
) -> Result<String> {
    if md.is_some() && !text.is_empty() {
        bail!(
            "--md и позиционный текст одновременно — передай текст одним каналом \
             (--md <файл>, --stdin или позиционным аргументом)"
        );
    }
    let msg = if let Some(path) = md {
        std::fs::read_to_string(path).with_context(|| format!("--md {path}: файл не читается"))?
    } else {
        collect_text(stdin, text)?
    };
    validate_comment_text(stdin || md.is_some(), &msg)?;
    Ok(msg)
}

/// Пред-гвард позиционного текста коммента: выполняется в task_run ДО bail по
/// пустому --project, чтобы проглоченный trailing_var_arg'ом флаг
/// (`task comment <id> "текст" --project X`) не маскировался под «--project не
/// передан». Явные каналы (stdin, --md) — no-op: их контент здесь не читается,
/// валидирует ровно одна ветка ниже (двойное чтение stdin дало бы EOF →
/// ложный «пустой текст»). Позиционная валидация чистая и идемпотентна —
/// повтор в Comment-ветке безвреден.
pub(super) fn pre_guard_positional_comment(
    stdin: bool,
    md: Option<&str>,
    text: &[String],
) -> Result<()> {
    if stdin || md.is_some() {
        return Ok(());
    }
    validate_comment_text(false, &text.join(" "))
}

/// Гвард текста коммента: пустой — отказ во всех каналах; из позиционных
/// токенов отклоняются имена подкоманд и флагоподобные токены. Явный канал
/// (explicit — stdin или --md) не гвардится на подкоманды и флаги: явное
/// намерение, глотание туда невозможно.
pub(super) fn validate_comment_text(explicit: bool, msg: &str) -> Result<()> {
    if msg.trim().is_empty() {
        bail!(
            "пустой текст коммента — передай --stdin, --md <файл> или текст позиционным аргументом"
        );
    }
    if explicit {
        return Ok(());
    }
    let tokens: Vec<&str> = msg.split_whitespace().collect();
    if let Some(first) = tokens.first().filter(|t| TASK_SUB_NAMES.contains(t)) {
        bail!(
            "«{first}» — имя подкоманды task/cron, а не текст коммента \
             (читать комменты: task get/thread <id>; писать: task comment <id> \"текст\", \
             --stdin или --md <файл>)"
        );
    }
    if let Some(flag) = tokens.iter().find(|t| looks_like_flag(t)) {
        bail!(
            "токен «{flag}» похож на флаг: после текста коммента флаги глотаются как текст \
             (trailing_var_arg) — пиши флаги до <id>, используй --stdin или --md <файл>"
        );
    }
    Ok(())
}
