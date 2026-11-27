//! E2E главного сценария #1693 (гейт UWUWU_E2E=1): gone-метки переживают
//! проход — живой uwuwu-server (hub driver=memory, executor = mock_opencode)
//! за два прохода env-gc доезжает до хвоста кандидатов за батч-капом.
//! Проводка `env_gc_loop` (один `EnvGcGone` на процесс) юнит-тестами через
//! `sweep_envs` не видна: мутант «контейнер на каждый проход» красит только
//! этот уровень (второй проход снова несёт первые 20, хвост не доезжает).
//!
//! Сценарий:
//!   1. 21 строка Done, закрыты 3 суток назад (кандидаты при
//!      `env_gc_days = 1`, батч-кап 20), 21-я — за капом первого прохода;
//!   2. проход 1: `candidates=21, gone=0, batch=20, swept=20`;
//!   3. проход 2: `candidates=1, gone=20, batch=1, swept=1`, вторая
//!      shell-команда мока несёт только 21-ю строку;
//!   4. третий проход молчит: ни `env.gc.completed`, ни shell-вызовов.
//!
//! Запуск: UWUWU_E2E=1 cargo test -p uwuwu-server --test env_gc_stand_e2e
//! Длительность ~25 с: два прохода при poll_sec = 10 плюс окно тишины
//! третьего прохода.

mod common;

use common::stand::{e2e_enabled, log_tail, spawn_stand, StandOptions};
use serde_json::Value;
use std::time::{Duration, Instant};

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Поля событий `event` из JSON-строк лога (tracing JSON formatter).
fn events(log: &str, event: &str) -> Vec<Value> {
    log.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["fields"]["event"].as_str() == Some(event))
        .map(|v| v["fields"].clone())
        .collect()
}

/// Строки батча — токены shell-команды после литерала sweep.
fn batch_rows(command: &str) -> Vec<&str> {
    command
        .split_whitespace()
        .skip_while(|t| *t != "sweep")
        .skip(1)
        .collect()
}

#[tokio::test]
async fn gone_marks_survive_passes_and_tail_reaches_the_script() {
    if !e2e_enabled() {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }
    let dir = std::env::temp_dir().join(format!("env-gc-stand-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // 21 строка закрыта 3 суток назад (кандидаты при env_gc_days = 1;
    // батч-кап 20), 21-я — за капом первого прохода
    let seed: String = (3001..=3021u32)
        .map(|n| format!("stand:{n}:Done:low:3"))
        .collect::<Vec<_>>()
        .join(";");
    let stand = spawn_stand(
        &dir,
        StandOptions {
            config_extra: "sweep_days = 7\nenv_gc_days = 1\nenv_gc_observe = false\n\n",
            pre_state: None,
            server_env: &[("UWUWU_MOCK_SEED", seed.as_str())],
            capture_sessions: false,
        },
    )
    .await;
    let server_log = stand.dir.join("server.log");
    let mock_log = stand.dir.join("mock.log");

    // Act: два прохода (poll_sec = 10)
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let done = events(&read(&server_log), "env.gc.completed");
        if done.len() >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "второй проход env-gc не пришёл за 60с\n{}",
            log_tail(&server_log)
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Assert: второй проход несёт хвост, а не снова первые 20
    let completed = events(&read(&server_log), "env.gc.completed");
    let first = &completed[0];
    let second = &completed[1];
    assert_eq!(first["candidates"], 21, "проход 1: {first}");
    assert_eq!(first["gone"], 0, "проход 1: {first}");
    assert_eq!(first["batch"], 20, "проход 1: {first}");
    assert_eq!(first["swept"], 20, "проход 1: {first}");
    assert_eq!(second["candidates"], 1, "проход 2: {second}");
    assert_eq!(second["gone"], 20, "проход 2: {second}");
    assert_eq!(second["batch"], 1, "проход 2: {second}");
    assert_eq!(second["swept"], 1, "проход 2: {second}");

    let shells = events(&read(&mock_log), "mock.shell");
    let commands: Vec<&str> = shells
        .iter()
        .filter_map(|v| v["command"].as_str())
        .collect();
    assert_eq!(commands.len(), 2, "shell-вызовы: {commands:?}");
    assert_eq!(batch_rows(commands[0]).len(), 20);
    assert_eq!(
        batch_rows(commands[1]),
        ["3021"],
        "второй батч — только хвост"
    );

    // Третий проход молчит: помеченные строки не диспатчатся повторно
    tokio::time::sleep(Duration::from_secs(14)).await;
    assert_eq!(
        events(&read(&server_log), "env.gc.completed").len(),
        2,
        "третий проход не должен диспатчить\n{}",
        log_tail(&server_log)
    );
    assert_eq!(events(&read(&mock_log), "mock.shell").len(), 2);

    drop(stand);
    let _ = std::fs::remove_dir_all(&dir);
}
