//! E2E #1273: TOML-ключ `port` доезжает до HTTP-бинда живого сервера.
//!
//! Юнит `config_toml_port_key_reaches_port` проверяет парсинг → `port()`;
//! этот тест — связку «конфиг-файл → бинарь → TCP listening», которую
//! стенды не гоняют (порты стендов передаются env-ом). Спавн uwuwu-server
//! с memory-конфигом: порт берётся у ОС (bind :0 → отпустить → отдать
//! конфигу), PORT у ребёнка снят — проверяется именно TOML-путь, без
//! оглядки на глобальное окружение раннера.

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Свободный порт: ОС выдаёт ephemeral на bind :0, слушатель тут же
/// отпущен — к моменту бинда ребёнка порт почти наверняка свободен.
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind :0")
        .local_addr()
        .expect("local_addr")
        .port()
}

/// Минимальный memory-конфиг + логи ребёнка — всё в tempdir (сервер
/// кладёт lancedb рядом с cwd, мусор в тестовом каталоге не нужен).
fn spawn_server(port: u16) -> (Child, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("uwuwu_port_bind_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tempdir создан");
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"mode = "observe"
poll_sec = 3600
port = {port}

[hub]
repo = "a/b"
driver = "memory"

[executor]
name = "kisa"
url = "http://127.0.0.1:1"

[policy]
cup = 1
peak = {{ enabled = false, start_min = 0, end_min = 0 }}

[[project]]
name = "p"
board = "B"
repo = "a/c"
checkout = "/tmp/co"
env_root = "/tmp/env"
"#,
        ),
    )
    .expect("конфиг записан");
    let log = dir.join("server.log");
    let child = Command::new(env!("CARGO_BIN_EXE_uwuwu-server"))
        .current_dir(&dir)
        .env_remove("PORT")
        .env("UWUWU_SERVER_CONFIG", &cfg)
        .env("GH_TOKEN", "test")
        .env("UWUWU_SERVER_BEARER", "test")
        .env("UWUWU_OPENCODE_PASSWORD", "mockpass")
        .stdout(Stdio::from(
            std::fs::File::create(&log).expect("лог открыт"),
        ))
        .stderr(Stdio::null())
        .spawn()
        .expect("сервер спавнен");
    (child, log, dir)
}

#[test]
fn toml_port_key_reaches_http_bind() {
    let port = free_port();
    let (mut child, log, dir) = spawn_server(port);

    // Подтверждение бинда в логе — событие http.listening с нашим портом
    // (JSON-лог сервера, оба поля одного tracing-события). Греп закрывает
    // TOCTOU-окно free_port: чужой слушатель на отпущенном ephemeral-порту
    // дал бы зелёный connect без нашей строки в логе.
    let bind_line = format!("\"event\":\"http.listening\",\"port\":{port}");
    let logged = || std::fs::read_to_string(&log).is_ok_and(|l| l.contains(&bind_line));

    // TCP-connect на TOML-порт до успеха, смерти ребёнка или таймаута.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut bound = false;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            bound = true;
            break;
        }
        if child.try_wait().expect("статус ребёнка").is_some() {
            break; // умер раньше бинда — assert ниже покажет лог
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Строка http.listening может отставать от успешного connect на
    // миллисекунды — дать логу дозаписаться до kill (2с максимум).
    if bound {
        let mut logged_now = logged();
        let write_deadline = Instant::now() + Duration::from_secs(2);
        while !logged_now && Instant::now() < write_deadline {
            if child.try_wait().map_or(true, |s| s.is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
            logged_now = logged();
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    // Лог читается до очистки каталога: после remove_dir_all файла нет,
    // и диагностика падения была бы всегда пустой (Low 1 тест-ревью).
    let log_contents = std::fs::read_to_string(&log).unwrap_or_default();
    std::fs::remove_dir_all(&dir).ok(); // lancedb/логи — мусор за собой

    assert!(
        bound && log_contents.contains(&bind_line),
        "сервер не подтвердил http.listening port={port} (bound={bound}); лог:\n{}",
        log_contents
    );
}
