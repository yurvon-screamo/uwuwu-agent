//! Контракт шапки `server/ops/config.memory.toml` (#1670, инцидент #1648).
//!
//! Три слоя:
//!   1. Детерминированный пин самой шапки. observe проверяется именно в блоке
//!      команды запуска, а не по всей шапке: то же литерал живёт в строке-прозе,
//!      и поиск по всей шапке не ловит исчезновение переменной из рецепта.
//!   2. E2E без мока (`UWUWU_E2E=1`): дословный запуск по шапке из `server/`
//!      с реальным конфигом. Доказывает observe в `server.started`, отсутствие
//!      сайдкаров, read-only контакт с исполнителем и MCP-ответ. `spawns: 0`
//!      здесь необходимое, но недостаточное условие: без исполнителя спавн
//!      удерживается слепым сканом ещё до observe-гейта — доказательство гейта
//!      в третьем слое.
//!   3. E2E с живым моком через `UWUWU_EXECUTOR_URL`: `spawns_allowed: 1`
//!      при `spawns: 0` и `session_created: 0` у мока. В live те же входы дают
//!      настоящий спавн, так что ассерты действительно могут упасть.
//!
//! Значения конфига (`mode = "live"` в файле, порты, пути) НЕ пинятся:
//! по слову владельца 26.11 они оставлены как есть, флип дефолта — отдельное
//! решение. Тест защищает именно рецепт шапки, не конфиг-значения.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn server_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("каталог server/ резолвится")
}

fn config_path() -> PathBuf {
    server_dir().join("ops/config.memory.toml")
}

/// Комментарии в начале файла — до первой не-`#` строки (`mode = "live"`).
fn header(cfg: &str) -> String {
    cfg.lines()
        .take_while(|line| line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Блок команды запуска: строка с `cargo run --bin uwuwu-server` и её
/// продолжения с `\`. Проза шапки (где тот же литерал стоит как правило)
/// в блок не входит — регрессия команды не должна прикрываться прозой.
fn launch_block(head: &str) -> String {
    let lines: Vec<&str> = head.lines().collect();
    let end = lines
        .iter()
        .position(|line| line.contains("cargo run --bin uwuwu-server"))
        .expect("в шапке должна быть команда запуска сервера");
    let mut start = end;
    while start > 0 && lines[start - 1].trim_end().ends_with('\\') {
        start -= 1;
    }
    lines[start..=end].join("\n")
}

#[test]
fn memory_stand_header_forces_observe_without_mock() {
    let cfg = fs::read_to_string(config_path()).expect("config.memory.toml читается");
    let head = header(&cfg);
    // Шапка обещает «Ручного live по этой шапке нет» — значит в ней ровно
    // одна команда запуска сервера. Второй блок без переменной не должен
    // прятаться за проверенным первым.
    assert_eq!(
        head.matches("cargo run --bin uwuwu-server").count(),
        1,
        "в шапке должна быть ровно одна команда запуска сервера:\n{head}"
    );
    let cmd = launch_block(&head);

    // Главное правило: ровно одно присваивание UWUWU_SERVER_MODE в команде,
    // и оно — точное `=observe`. Пробелы, регистр и отсутствие переменной
    // дают тихий live с реальными checkout/env_root (#1648).
    let mode: Vec<&str> = cmd
        .split_whitespace()
        .filter(|token| token.starts_with("UWUWU_SERVER_MODE"))
        .collect();
    assert_eq!(
        mode,
        vec!["UWUWU_SERVER_MODE=observe"],
        "в команде запуска должна быть ровно одна переменная UWUWU_SERVER_MODE=observe (без пробелов и с нужным регистром); команда:\n{cmd}"
    );

    // Входы запуска — тоже в команде, а не где-то в прозе.
    assert!(
        head.contains("Запуск из каталога server/"),
        "шапка обязана называть рабочий каталог server/ (корень репо не несёт Cargo.toml)"
    );
    assert!(
        cmd.contains("UWUWU_SERVER_CONFIG=ops/config.memory.toml")
            && cmd.contains("GH_TOKEN=$GH_TOKEN")
            && cmd.contains("UWUWU_SERVER_STATE=")
            && cmd.contains("PORT="),
        "в команде должны быть путь конфига, GH_TOKEN, state в scratch и свободный PORT:\n{cmd}"
    );

    // Предупреждения и ожидаемые события — в прозе шапки.
    assert!(
        head.contains("доминирует"),
        "шапка обязана предупреждать: env доминирует над mode в файле"
    );
    assert!(
        head.contains("точные") && head.contains("observe и live"),
        "шапка обязана называть точные значения переменной (иное — тихий live)"
    );
    assert!(
        head.contains("server.started"),
        "шапка обязана велеть подтвердить режим строкой server.started"
    );
    assert!(
        head.contains("sessions.list_reachable_failed")
            && head.contains("executor.unavailable_unnotified"),
        "шапка обязана называть ожидаемые без мока WARN-события"
    );
    assert!(
        head.contains("ops/stand/e2e.sh"),
        "шапка обязана отсылать live к e2e.sh"
    );
    assert!(
        !head.contains("полный цикл конвейера"),
        "заголовок старой шапки обещал полный цикл — рецепт без мока его не даёт"
    );
    assert!(
        !head.contains("UWUWU_SERVER_MODE=live") && !head.contains("UWUWU_OPENCODE_PASSWORD"),
        "ручного live с паролем опенкода в шапке быть не должно (#1648)"
    );
    assert!(
        head.contains("UWUWU_EXECUTOR_URL"),
        "шапка обязана упомянуть, что UWUWU_EXECUTOR_URL перекрывает URL конфига"
    );
}

/// Уборка tempdir и дочерних процессов даже при упавшем ассерте.
/// Оба поля опциональны: гард строится сразу после первого спавна, до любых
/// ожиданий и ассертов, — иначе паника в этом окне оставляет процесс живым.
struct StandGuard {
    dir: PathBuf,
    server: Option<Child>,
    mock: Option<Child>,
}

impl Drop for StandGuard {
    fn drop(&mut self) {
        for child in [&mut self.server, &mut self.mock].into_iter().flatten() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind :0")
        .local_addr()
        .expect("local_addr")
        .port()
}

/// Режим без мока: что реально доказано — observe, тишина сайдкаров,
/// read-only контакт и MCP. `spawns: 0` здесь держится ещё и слепым сканом,
/// поэтому гейт проверяется отдельным тестом с доступным исполнителем.
#[tokio::test]
async fn observe_stand_from_real_config_is_read_only_and_quiet() {
    if std::env::var("UWUWU_E2E").ok().as_deref() != Some("1") {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }
    assert!(
        TcpStream::connect(("127.0.0.1", 8099)).is_err(),
        "порт 8099 из конфига занят — мок снаружи ломает предусловие «без мока»"
    );

    let dir = std::env::temp_dir().join(format!("config-memory-observe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("tempdir");
    let log_path = dir.join("server.log");
    let state_path = dir.join("state.json");
    let port = free_port();
    let bearer = "config-memory-observe-bearer";

    // Дословно по шапке: каталог server/, путь ops/config.memory.toml,
    // observe переменной, без мока, без пароля опенкода. Уровень лога
    // выставляем явно: унаследованный RUST_LOG=warn убрал бы info-события.
    let log = fs::File::create(&log_path).expect("лог открыт");
    let child = Command::new(env!("CARGO_BIN_EXE_uwuwu-server"))
        .current_dir(server_dir())
        .env_remove("UWUWU_OPENCODE_PASSWORD")
        .env_remove("UWUWU_EXECUTOR_URL")
        .env("RUST_LOG", "info")
        .env("UWUWU_SERVER_CONFIG", "ops/config.memory.toml")
        .env("UWUWU_SERVER_MODE", "observe")
        .env("UWUWU_SERVER_BEARER", bearer)
        .env("GH_TOKEN", "dummy-e2e-token")
        .env("UWUWU_SERVER_STATE", &state_path)
        .env("PORT", port.to_string())
        .env("UWUWU_MOCK_SEED", "1148:Design")
        .stdout(Stdio::from(log.try_clone().expect("лог clone")))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("сервер спавнен");
    let mut stand = StandGuard {
        dir: dir.clone(),
        server: Some(child),
        mock: None,
    };

    let read_log = || fs::read_to_string(&log_path).unwrap_or_default();
    let bind = format!("\"event\":\"http.listening\",\"port\":{port}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !read_log().contains(&bind) {
        assert!(
            stand
                .server
                .as_mut()
                .expect("сервер спавнен")
                .try_wait()
                .expect("статус ребёнка")
                .is_none(),
            "сервер умер до http.listening\nлог:\n{}",
            read_log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        read_log().contains(&bind),
        "http.listening port={port} не появился за 30 c\nлог:\n{}",
        read_log()
    );

    // Три тика — порог executor.rs:13 для executor.unavailable_unnotified;
    // само событие пишется после возврата тика, поэтому ждём и его явно.
    let deadline = Instant::now() + Duration::from_secs(75);
    loop {
        let out = read_log();
        if out.matches("\"event\":\"tick.completed\"").count() >= 3
            && out.contains("executor.unavailable_unnotified")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "менее трёх тиков или нет executor.unavailable_unnotified\nлог:\n{out}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }

    let out = read_log();
    assert!(
        out.contains("\"event\":\"server.started\"") && out.contains("\"mode\":\"Observe\""),
        "сервер не подтвердил observe в server.started\nлог:\n{out}"
    );
    assert!(
        out.contains("\"event\":\"sidecars.observe_mode\""),
        "в observe сайдкары не должны запускаться\nлог:\n{out}"
    );
    // В live на тех же входах сайдкары тикают — это и есть различающий ассерт.
    for sidecar in [
        "bot.scan",
        "analys.list_degraded",
        "analyst_review.list_degraded",
        "dreamer.list_degraded",
        "cron.list_sessions_failed",
        "bot.compact_scan_failed",
    ] {
        assert!(
            !out.contains(sidecar),
            "событие сайдкара {sidecar} в observe недопустимо\nлог:\n{out}"
        );
    }
    let ticks = out.matches("\"event\":\"tick.completed\"").count();
    let zeros = out.matches("\"spawns\":0").count();
    assert!(
        ticks >= 3 && zeros >= ticks,
        "тики есть, но не все с spawns: 0 (ticks={ticks}, zeros={zeros})\nлог:\n{out}"
    );
    // Успешный спавн сервер пишет как spawn.done и счётчиком spawns:N;
    // create_session в его логе — только тексты отказов, как сигнал мёртв.
    for forbidden in [
        "spawn.done",
        "sidecar.tick_failed",
        "spawn.budget_exhausted",
        "env.gc",
    ] {
        assert!(
            !out.contains(forbidden),
            "событие {forbidden} в observe недопустимо\nлог:\n{out}"
        );
    }
    assert!(
        out.contains("sessions.list_reachable_failed")
            && out.contains("executor.unavailable_unnotified"),
        "ожидаемые без мока WARN не появились\nлог:\n{out}"
    );

    let http = reqwest::Client::new();
    let resp = http
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Authorization", format!("Bearer {bearer}"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "config-memory-probe", "version": "1" }
            }
        }))
        .send()
        .await
        .expect("MCP-запрос ушёл");
    let body = resp.text().await.expect("MCP-ответ прочитан");
    assert!(
        body.contains("\"serverInfo\"") && !body.contains("\"error\""),
        "MCP initialize не ответил: {body}"
    );

    drop(stand);
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "порт {port} остался занятым после остановки стенда"
    );
}

/// Observe-гейт при доступном исполнителе: в live те же входы дают настоящий
/// спавн, так что `spawns: 0` и `session_created: 0` здесь — не пустой ассерт.
#[tokio::test]
async fn observe_gate_holds_with_reachable_mock() {
    if std::env::var("UWUWU_E2E").ok().as_deref() != Some("1") {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }

    let dir = std::env::temp_dir().join(format!("config-memory-mock-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("tempdir");
    let server_log = dir.join("server.log");
    let mock_log = dir.join("mock.log");
    let state_path = dir.join("state.json");
    let port = free_port();
    let mock_port = free_port();
    let bearer = "config-memory-mock-bearer";

    let mock_out = fs::File::create(&mock_log).expect("лог мока открыт");
    let mock = Command::new(env!("CARGO_BIN_EXE_mock_opencode"))
        .env("RUST_LOG", "info")
        .env("MOCK_MODE", "agent")
        .env("MOCK_MCP_TOKEN", bearer)
        .env("MOCK_PORT", mock_port.to_string())
        .stdout(Stdio::from(mock_out.try_clone().expect("лог мока clone")))
        .stderr(Stdio::from(mock_out))
        .spawn()
        .expect("мок спавнен");
    // Гард строится сразу после спавна, до любых ожиданий и ассертов:
    // паника в окне готовности не оставит живой процесс и tempdir.
    let mut stand = StandGuard {
        dir: dir.clone(),
        server: None,
        mock: Some(mock),
    };
    // Готовность по TCP, а не по логу: уровень логирования задан тестом,
    // но сам сигнал от внешнего RUST_LOG не зависит.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && TcpStream::connect(("127.0.0.1", mock_port)).is_err() {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        TcpStream::connect(("127.0.0.1", mock_port)).is_ok(),
        "мок не слушает {mock_port} за 20 c\nлог:\n{}",
        fs::read_to_string(&mock_log).unwrap_or_default()
    );

    // UWUWU_EXECUTOR_URL перекрывает 8099 из конфига — исполнитель доступен,
    // слепой скан снимается, гейт observe получает шанс сработать.
    let log = fs::File::create(&server_log).expect("лог сервера открыт");
    let child = Command::new(env!("CARGO_BIN_EXE_uwuwu-server"))
        .current_dir(server_dir())
        .env_remove("UWUWU_OPENCODE_PASSWORD")
        .env("RUST_LOG", "info")
        .env("UWUWU_SERVER_CONFIG", "ops/config.memory.toml")
        .env("UWUWU_SERVER_MODE", "observe")
        .env("UWUWU_SERVER_BEARER", bearer)
        .env("GH_TOKEN", "dummy-e2e-token")
        .env("UWUWU_SERVER_STATE", &state_path)
        .env("PORT", port.to_string())
        .env(
            "UWUWU_EXECUTOR_URL",
            format!("http://127.0.0.1:{mock_port}"),
        )
        .env("UWUWU_MOCK_SEED", "1148:Design")
        .stdout(Stdio::from(log.try_clone().expect("лог clone")))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("сервер спавнен");
    stand.server = Some(child);

    let read_server = || fs::read_to_string(&server_log).unwrap_or_default();
    let read_mock = || fs::read_to_string(&mock_log).unwrap_or_default();
    let bind = format!("\"event\":\"http.listening\",\"port\":{port}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !read_server().contains(&bind) {
        assert!(
            stand
                .server
                .as_mut()
                .expect("сервер спавнен")
                .try_wait()
                .expect("статус ребёнка")
                .is_none(),
            "сервер умер до http.listening\nлог мока:\n{}\nлог сервера:\n{}",
            read_mock(),
            read_server()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        read_mock().contains("mock.started") && read_server().contains(&bind),
        "мок и сервер не поднялись\nлог мока:\n{}\nлог сервера:\n{}",
        read_mock(),
        read_server()
    );

    // Ждём тик, на котором гейт реально разрешил спавн (mock доступен).
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        let out = read_server();
        if out.contains("\"spawns_allowed\":1") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "за 40 c не было тика с spawns_allowed:1 — гейт не дошел до исполнителя\nлог мока:\n{}\nлог сервера:\n{out}",
            read_mock()
        );
        std::thread::sleep(Duration::from_millis(500));
    }

    let out = read_server();
    let mock_out = read_mock();
    assert!(
        out.contains("\"mode\":\"Observe\"") && out.contains("\"event\":\"sidecars.observe_mode\""),
        "стенд не в observe\nлог:\n{out}"
    );
    let spawn_values: Vec<u64> = out
        .match_indices("\"spawns\":")
        .filter_map(|(i, _)| {
            out[i + "\"spawns\":".len()..]
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|digits| digits.parse().ok())
        })
        .collect();
    assert!(
        !spawn_values.is_empty() && spawn_values.iter().all(|value| *value == 0),
        "observe при доступном моке разрешил спавн: {spawn_values:?}\nлог:\n{out}"
    );
    assert!(
        !out.contains("spawn.done") && !mock_out.contains("session_created"),
        "observe вызвал создание сессии (spawn.done у сервера или session_created у мока)\nлог мока:\n{mock_out}\nлог сервера:\n{out}"
    );
    assert!(
        !out.contains("sessions.list_reachable_failed"),
        "мок доступен — WARN недоступности не должен появиться\nлог:\n{out}"
    );

    drop(stand);
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err()
            && TcpStream::connect(("127.0.0.1", mock_port)).is_err(),
        "порты стенда остались занятыми после остановки"
    );
}
