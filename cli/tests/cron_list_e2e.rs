//! E2E-контракт `cron list` на живом ядре SiYuan (регрессия бага
//! 20261110044120): инвентарь мастеров собирается с колонкой «Запланировано»,
//! LAST RUN не пуст при живых ран-карточках доски. Спавн идёт с изолированным
//! UWUWU_CRON_STATE_DIR — пин ловит именно доску-источник (раны видны всем
//! машинам), а не локальный state тест-машины (фоллэк покрыт интеграционными
//! тестами cron_tests).
//!
//! Запуск: UWUWU_E2E=1 cargo test --test cron_list_e2e (нужно живое ядро:
//! UWUWU_SIYUAN_URL/TOKEN). Без UWUWU_E2E тест сразу зелёный. Read-only:
//! доска и state не мутируются. Живая доска меняется — если у мастеров нет
//! ран-карточек (кроны остановлены и почищены), тест уходит в skip, не падение.

use std::process::Command;

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").as_deref() == Ok("1")
}

/// LAST RUN — последний столбец таблицы: «YYYY-MM-DD HH:MM» (опц. + хвост
/// сессии). Ищем пару (дата, время) с конца строки; planned-колонка мастеров
/// на живой доске в UI-локали ядра («11 ноября 2026 г. 01:30») пары не даёт.
fn last_run_present(line: &str) -> bool {
    let mut tokens: Vec<&str> = line.split_whitespace().collect();
    while tokens.len() >= 2 {
        let time = tokens[tokens.len() - 1];
        let date = tokens[tokens.len() - 2];
        let date_ok = date.len() == 10
            && date.as_bytes()[4] == b'-'
            && date.as_bytes()[7] == b'-'
            && date.bytes().all(|b| b.is_ascii_digit() || b == b'-');
        let time_ok = time.len() == 5
            && time.as_bytes()[2] == b':'
            && time.bytes().all(|b| b.is_ascii_digit() || b == b':');
        if date_ok && time_ok {
            return true;
        }
        // не дата-время: хвост сессии — снимаем и пробуем раньше
        tokens.pop();
    }
    false
}

/// Тайтл мастера из строки `task list` с ран-карточкой («⏰ <мастер> · <дата>»,
/// task list обрезает длинные тайтлы): префикс до «·» (или весь хвост, если
/// дату съела обрезка), укороченный до 20 символов — помещается в колонку
/// ЗАГОЛОВОК cron list (truncate 26) и отличим от других мастеров.
fn run_owner_prefix(task_list_line: &str) -> String {
    let tail = task_list_line.split('⏰').nth(1).unwrap_or("");
    let owner = tail.rsplit_once("·").map(|(o, _)| o).unwrap_or(tail);
    owner.trim().chars().take(20).collect()
}

#[test]
fn cron_list_e2e_last_run_from_board_and_planned_column() {
    if !e2e_enabled() {
        return;
    }
    let out = Command::new(env!("CARGO_BIN_EXE_uwuwu-cli"))
        .args(["cron", "list"])
        .env("UWUWU_CRON_STATE_DIR", "/nonexistent-cron-e2e")
        .output()
        .expect("спавн uwuwu-cli");
    assert!(
        out.status.success(),
        "cron list завершился с ошибкой: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty(), "пустой вывод cron list");
    let header = lines[0];
    assert!(
        header.contains("ЗАПЛАНИРОВАНО"),
        "нет колонки «Запланировано»: {header}"
    );
    assert!(
        header.contains("LAST RUN"),
        "нет колонки LAST RUN: {header}"
    );
    let data = &lines[1..];
    if data.is_empty() {
        eprintln!("skip: крон-мастеров на доске нет");
        return;
    }
    // Главный пин бага 20261110044120: при живых ран-карточках LAST RUN непуст
    // хотя бы у одного мастера, даже без state демона (доска — единый источник).
    let any_last = data.iter().any(|l| last_run_present(l));
    if !any_last {
        let board = Command::new(env!("CARGO_BIN_EXE_uwuwu-cli"))
            .args(["task", "list", "--project", "uwuwu"])
            .output()
            .expect("спавн task list");
        let board_stdout = String::from_utf8_lossy(&board.stdout);
        let live_runs = board_stdout
            .lines()
            .filter(|l| l.contains("⏰"))
            .any(|l| stdout.contains(&run_owner_prefix(l)));
        if !live_runs {
            eprintln!("skip: у живых мастеров нет ран-карточек (кроны остановлены/почищены)");
            return;
        }
    }
    assert!(
        any_last,
        "LAST RUN пуст при живых ран-карточках — регрессия 20261110044120:\n{stdout}"
    );
}
