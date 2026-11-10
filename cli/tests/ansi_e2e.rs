//! Пин «ни одного байта ANSI из uwuwu-cli» (задача 20261110104429-ml41jc8):
//! цвета выпилены глобально и физически — clap собран без фичи `color`,
//! tracing застрахован `.with_ansi(false)`; флаг `--no-color` не заводится.
//!
//! Уровень процесса, не String: раскраску применяет anstream при печати в
//! stdout/stderr, поэтому юнит на рендер clap возврат фичи color не ловит.
//! Пин — живой бинарь в псевдо-TTY (`script -qec`, util-linux, Linux-only)
//! и в пайпе: если кто-то вернёт `color` в clap-фичи, help и ошибки снова
//! раскрасятся в TTY и тесты упадут. Главная поверхность до изменения:
//! help (19 строк с ESC-байтом) и ошибки (3).
//!
//! Гейтнутый тест (`UWUWU_E2E=1`) докручивает живой вывод команды (doctor)
//! на живом ядре — то, что за пределами help/err.

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_uwuwu-cli")
}

/// piped-прогон (TTY-детект anstream видит не-TTY): stdout+stderr по отдельности.
fn piped(args: &[&str]) -> (Option<i32>, String, String) {
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

fn assert_no_ansi(out: &str, what: &str) {
    assert!(
        !out.contains('\x1b'),
        "{what}: в выводе найден ANSI ESC-байт — цвета вернулись в cli; начало вывода: {:?}",
        out.chars().take(120).collect::<String>()
    );
}

/// piped-пин: help в пайпе монохромный и содержательный (было чисто до
/// изменения — пин не даёт классу piping-вывода деградировать).
#[test]
fn piped_help_is_monochrome() {
    let (code, stdout, stderr) = piped(&["--help"]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("Usage:"),
        "help в пайпе должен содержать Usage:; получено: {stdout:?}"
    );
    assert_no_ansi(&stdout, "piped help stdout");
    assert_no_ansi(&stderr, "piped help stderr");
}

/// piped-пин: ошибка clap в пайпе монохромная и внятная.
#[test]
fn piped_error_is_monochrome() {
    let (code, stdout, stderr) = piped(&["definitely-not-a-command-xyz"]);
    assert_eq!(code, Some(2), "неизвестная подкоманда — exit 2");
    assert!(
        stderr.contains("error:"),
        "ошибка clap идёт в stderr и содержит 'error:'; получено: {stderr:?}"
    );
    assert_no_ansi(&stdout, "piped error stdout");
    assert_no_ansi(&stderr, "piped error stderr");
}

/// Псевдо-TTY прогон: `script -qec <cmd> /dev/null` — anstream видит TTY;
/// это единственный способ поймать раскраску, которую String-рендер не несёт.
#[cfg(target_os = "linux")]
mod tty {
    use std::process::Command;

    use super::{assert_no_ansi, bin};

    /// stdout+stderr слиты в один поток (typescript-механика script).
    fn tty(args: &str) -> (Option<i32>, String) {
        let out = Command::new("script")
            .args(["-qec", &format!("{} {}", bin(), args), "/dev/null"])
            .output()
            .expect("spawn `script` (util-linux) — TTY-пин требует его на Linux");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    /// Главный путь: help в TTY монохромный и читаемый (до изменения —
    /// 19 строк с ESC-байтом).
    #[test]
    fn help_in_tty_is_monochrome() {
        let (code, out) = tty("--help");
        assert_eq!(code, Some(0), "help в TTY — exit 0");
        assert!(
            out.contains("Usage:"),
            "help в TTY должен содержать Usage:; получено: {out:?}"
        );
        assert_no_ansi(&out, "help в TTY");
    }

    /// Ошибка clap в TTY монохромная и внятная (до изменения — 3 строки с ESC).
    #[test]
    fn clap_error_in_tty_is_monochrome() {
        let (code, out) = tty("definitely-not-a-command-xyz");
        assert_eq!(code, Some(2), "неизвестная подкоманда — exit 2");
        assert!(
            out.contains("error:"),
            "ошибка clap в TTY должна содержать 'error:'; получено: {out:?}"
        );
        assert_no_ansi(&out, "ошибка clap в TTY");
    }

    /// Живая команда в TTY на живом ядре (гейт UWUWU_E2E=1): вывод doctor
    /// монохромный и непустой. Exit-код не пинится — зависит от состояния
    /// ядра (0 чисто / 1 деградация), пин — отсутствие ANSI.
    #[test]
    fn doctor_in_tty_is_monochrome() {
        if std::env::var("UWUWU_E2E").as_deref() != Ok("1") {
            return;
        }
        let (_code, out) = tty("doctor");
        assert!(
            !out.trim().is_empty(),
            "doctor в TTY должен что-то выводить (пустая проба — обманка)"
        );
        assert_no_ansi(&out, "doctor в TTY (живое ядро)");
    }
}
