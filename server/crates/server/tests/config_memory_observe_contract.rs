//! Контракт шапки `server/ops/config.memory.toml` (#1670, инцидент #1648).
//!
//! Два слоя:
//!   1. Детерминированный пин самой шапки: ручной стенд без мока описан
//!      только как observe, явная переменная в рецепте, warning про доминирование
//!      env и точные значения, WARN-события и отсылка live к `e2e.sh`.
//!      Любая регрессия — возврат опасного рецепта «полный цикл»/ручного
//!      live — ломает этот тест без живых процессов.
//!   2. E2E (`UWUWU_E2E=1`): дословный запуск по шапке из каталога `server/`
//!      с реальным конфигом (реальные checkout/env_root), без мока, observe
//!      явной переменной. Подтверждаем наблюдаемое поведение: `server.started`
//!      в Observe, сайдкары no-op, тики с `spawns: 0`, ноль мутаций исполнителя,
//!      MCP отвечает. State и логи — в tempdir, репо не замусорено.
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

#[test]
fn memory_stand_header_forces_observe_without_mock() {
    let cfg = fs::read_to_string(config_path()).expect("config.memory.toml читается");
    let head = header(&cfg);

    // Главное правило: observe явной переменной в рецепте запуска.
    assert!(
        head.contains("UWUWU_SERVER_MODE=observe"),
        "шапка обязана держать явную переменную observe в команде запуска"
    );
    assert!(
        head.contains("доминирует"),
        "шапка обязана предупреждать: env доминирует над mode в файле"
    );
    assert!(
        head.contains("точные") && head.contains("observe и live"),
        "шапка обязана называть точные значения переменной (иное — тихий live)"
    );

    // Подтверждение режима и ожидаемые WARN без мока.
    assert!(
        head.contains("server.started"),
        "шапка обязана велеть подтвердить режим строкой server.started"
    );
    assert!(
        head.contains("sessions.list_reachable_failed")
            && head.contains("executor.unavailable_unnotified"),
        "шапка обязана называть ожидаемые без мока WARN-события"
    );

    // Live отдан e2e.sh; ручного live и шага мока в шапке нет.
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

    // Входы запуска и оговорка про оверрайд URL.
    assert!(
        head.contains("GH_TOKEN") && head.contains("UWUWU_SERVER_STATE") && head.contains("PORT="),
        "в рецепте должны быть GH_TOKEN, state в scratch и свободный PORT"
    );
    assert!(
        head.contains("UWUWU_EXECUTOR_URL"),
        "шапка обязана упомянуть, что UWUWU_EXECUTOR_URL перекрывает URL конфига"
    );
}

/// Уборка tempdir и дочернего процесса даже при упавшем ассерте.
struct StandGuard {
    dir: PathBuf,
    child: Child,
}

impl Drop for StandGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

#[tokio::test]
async fn observe_stand_from_real_config_never_spawns() {
    if std::env::var("UWUWU_E2E").ok().as_deref() != Some("1") {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }

    let dir = std::env::temp_dir().join(format!("config-memory-observe-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("tempdir");
    let log_path = dir.join("server.log");
    let state_path = dir.join("state.json");
    let port = free_port();
    let bearer = "config-memory-observe-bearer";

    // Дословно по шапке: каталог server/, путь ops/config.memory.toml,
    // observe переменной, без мока (8099 не слушает), без пароля опенкода.
    let log = fs::File::create(&log_path).expect("лог открыт");
    let child = Command::new(env!("CARGO_BIN_EXE_uwuwu-server"))
        .current_dir(server_dir())
        .env_remove("UWUWU_OPENCODE_PASSWORD")
        .env_remove("UWUWU_EXECUTOR_URL")
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
    let stand = StandGuard {
        dir: dir.clone(),
        child,
    };

    let read_log = || fs::read_to_string(&log_path).unwrap_or_default();
    let bind = format!("\"event\":\"http.listening\",\"port\":{port}");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if read_log().contains(&bind) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Три тика по 10 c — достаточно, чтобы проявились все WARN из шапки.
    let deadline = Instant::now() + Duration::from_secs(75);
    loop {
        let ticks = read_log().matches("\"event\":\"tick.completed\"").count();
        if ticks >= 3 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "менее трёх тиков за 75 c\nлог:\n{}",
            read_log()
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
    let ticks = out.matches("\"event\":\"tick.completed\"").count();
    let zeros = out.matches("\"spawns\":0").count();
    assert!(
        ticks >= 3 && zeros >= ticks,
        "тики есть, но не все с spawns: 0 (ticks={ticks}, zeros={zeros})\nлог:\n{out}"
    );
    for forbidden in [
        "create_session",
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

    // MCP отвечает (главный пользовательский выход стенда).
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

    // Порт отпущен после остановки — тест не оставляет слушателя.
    drop(stand);
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "порт {port} остался занятым после остановки стенда"
    );
}
