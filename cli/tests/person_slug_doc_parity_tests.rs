//! Пин слага дока персоны в пользовательских поверхностях cli (задача 1047,
//! стадия тестирования). После hub-миграции фактический слаг —
//! `Experience/person/PERSON` (захардкожен в run.rs, пинен тестами
//! клон-стора); help и README месяцами называли до-hub-эру
//! `Experience/personal/PERSON`. Три текстовые правки привели их к слагу.
//!
//! Почему эти два носителя, а не исходники: help — исполняемый артефакт
//! (clap рендерит из док-комментария при каждом запуске), README —
//! пользовательская поверхность из постановки задачи. Doc-комментарий
//! внутри run.rs юзер не видит — он закрыт grep-приёмкой, тестом не пинится
//! (покрывается поведение, а не строки).
//!
//! Уровень: процесс на живом бинаре (как ansi_e2e) — юнит на рендер clap не
//! видит фактический вывод, который получает юзер; README — файловая
//! поверхность, дешёвый файловый пин.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_uwuwu-cli")
}

fn piped(args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(bin())
        .args(args)
        .output()
        .expect("spawn uwuwu-cli");
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn assert_honest_slug(out: &str, what: &str) {
    assert!(
        out.contains("Experience/person/PERSON"),
        "{what}: фактический слаг Experience/person/PERSON пропал из вывода; начало: {:?}",
        out.chars().take(200).collect::<String>()
    );
    assert!(
        !out.contains("personal/PERSON"),
        "{what}: легаси-слаг personal/PERSON вернулся в пользовательский вывод; начало: {:?}",
        out.chars().take(200).collect::<String>()
    );
    // hpath-легаси-формат (второй вид пути до hub-миграции) тоже не
    // возвращается.
    assert!(
        !out.contains("Experience:/personal"),
        "{what}: hpath-легаси Experience:/personal вернулся в пользовательский вывод; начало: {:?}",
        out.chars().take(200).collect::<String>()
    );
}

/// Главный пользовательский путь: агент читает `person --help`, чтобы понять,
/// где лежит док персоны, — help обязан называть фактический слаг.
#[test]
fn person_help_names_actual_slug() {
    let (code, out) = piped(&["person", "--help"]);
    assert_eq!(code, Some(0), "person --help — exit 0; вывод: {out:?}");
    assert_honest_slug(&out, "person --help");
}

/// Корневой help показывает ту же Person-формулировку (длинный листинг
/// сабкоманд) — второй проход той же поверхности.
#[test]
fn root_help_names_actual_slug() {
    let (code, out) = piped(&["--help"]);
    assert_eq!(code, Some(0), "--help — exit 0; вывод: {out:?}");
    assert_honest_slug(&out, "--help");
}

/// README — пользовательская документация из постановки задачи: строка
/// таблицы команд person обязана называть фактический слаг.
#[test]
fn readme_does_not_carry_legacy_slug() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let readme = std::fs::read_to_string(&path).expect("cli/README.md читается");
    assert_honest_slug(&readme, "cli/README.md");
}
