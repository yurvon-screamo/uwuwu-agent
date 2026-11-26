//! E2E главного сценария #1477 (гейт UWUWU_E2E=1): живой uwuwu-server
//! (hub driver=memory, executor = mock_opencode, [quota] → локальный
//! лимит-эндпоинт) ставит лимитную тишину на паузу без пингов/эскалации
//! и возобновляет механику после спада процента — то, что тестировщик
//! гонял руками на стенде scratch/stand.
//!
//! Сценарий (главный путь UC-05, «лимитный простой не эскалируется»):
//!   1. boot с [quota] + ключом: quota.enabled в логе (громко);
//!   2. исчерпанное окно (percentage=100) + тишина сессии > мягкого
//!      гейта: stall.limit_paused на строку, tick.completed
//!      limit_paused=1, пингов/эскалаций нет, строка не паркуется;
//!   3. один громкий сигнал на окно (quota.limit_unnotified, cooldown —
//!      повторные тики не дублируют);
//!   4. спад процента (39): кэш снапшота истекает → проба меняет
//!      вердикт → старая механика возвращается (soft ping уходит).
//!
//! Мок эндпоинта валидирует запрос (GET-строка с путём из [quota]
//! и bearer из env) — контракт адаптер↔эндпоинт под тестом.
//!
//! Запуск: UWUWU_E2E=1 cargo test -p uwuwu-server --test quota_limit_pause_stand_e2e
//! Длительность ~6 мин: фаза возобновления ждёт истечения TTL кэша
//! пробы (5 мин) — резет-время ядро не вычисляет, пауза по факту
//! повторных проб (граница плана #1477).

mod common;

use common::stand::{e2e_enabled, log_tail, mcp_call, spawn_stand, StandOptions, BEARER};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Каноны стенда: имена событий и литералы лога/контракта — строковые
/// константы на модуле; конфиг, мок и ассерты ссылаются на одни и те же
/// байты (дрейф spelling красит компиляцию, а не молчит).
const EV_ENABLED: &str = "quota.enabled";
const EV_SPAWN: &str = "spawn.done";
const EV_TICK: &str = "tick.completed";
const EV_PAUSE: &str = "stall.limit_paused";
const EV_SOFT: &str = "stall.soft_pinged";
const EV_ESC: &str = "stall.escalated";
const EV_PING: &str = "stall.ping";
const EV_SIGNAL: &str = "quota.limit_unnotified";
const NEEDS: &str = "Needs attention";
/// Префикс заголовка хвоста комментов task_get (#1604): счётчики
/// «latest N of M» в заголовке переменные, гард ловит сам факт секции.
const COMM: &str = "--- comments (latest ";
const IN_PROGRESS: &str = "In Progress";
const PROJECT_STAND: &str = "stand";
const WIN_PCT: &str = "window_pct";
const TICK_FIELD: &str = "limit_paused";
const QUOTA_PATH: &str = "/api/monitor/usage/quota/limit";
const E2E_QUOTA_KEY: &str = "e2e-stand-key";
const KEY_ENV_STR: &str = "UWUWU_ZAI_API_KEY";

/// Стартовая строка запроса, который адаптер обязан послать: GET по
/// пути из [quota] (та же константа в конфиге стенда).
fn request_line() -> String {
    format!("GET {QUOTA_PATH} HTTP/1.1")
}

/// Ожидаемый заголовок авторизации: bearer из env-ключа секции.
fn auth_header() -> String {
    format!("Authorization: Bearer {E2E_QUOTA_KEY}")
}

/// Локальный лимит-эндпоинт: форма — фикстура живого ответа
/// (fixtures/quota_fixture.json), percentage TOKENS_LIMIT — из атомара
/// (флип исчерпано/живо без рестарта). Запрос валидируется: стартовая
/// строка GET с путём из [quota] и bearer из env; чужой запрос получает
/// 401 и считается в bad — адаптер, потерявший bearer или путь, красит
/// тест контрактом адаптер↔эндпоинт, а не молчит.
async fn spawn_quota_mock(pct: Arc<AtomicU8>, bad: Arc<AtomicU8>) -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind quota mock");
    let port = listener.local_addr().expect("local_addr").port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                continue;
            };
            let pct = pct.clone();
            let bad = bad.clone();
            tokio::spawn(async move {
                // читаем до конца заголовков (один read может прийти с
                // частичным запросом — валидация по полным байтам)
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if String::from_utf8_lossy(&buf).contains("\r\n\r\n")
                                || buf.len() > 16 * 1024
                            {
                                break;
                            }
                        }
                    }
                }
                let req = String::from_utf8_lossy(&buf);
                let request_line_ok = req.lines().next().map(|l| l == request_line()) == Some(true);
                let bearer_ok = req.lines().any(|l| l.eq_ignore_ascii_case(&auth_header()));
                if !(request_line_ok && bearer_ok) {
                    bad.fetch_add(1, Ordering::Relaxed);
                    let resp = "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                    return;
                }
                let mut fixture: Value =
                    serde_json::from_str(include_str!("fixtures/quota_fixture.json"))
                        .expect("quota fixture");
                let p = pct.load(Ordering::Relaxed);
                for limit in fixture["data"]["limits"]
                    .as_array_mut()
                    .expect("limits array")
                {
                    if limit["unit"] == 3 && limit["number"] == 5 {
                        limit["percentage"] = json!(p);
                    }
                }
                let body = serde_json::to_string(&fixture).expect("fixture to string");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            });
        }
    });
    port
}

/// Событие в JSON-логе сервера: последняя строка, где `fields.event`
/// равен искомому (None — события не было).
fn last_event(log: &Path, event: &str) -> Option<Value> {
    let text = std::fs::read_to_string(log).ok()?;
    text.lines().rev().find_map(|line| {
        let v: Value = serde_json::from_str(line).ok()?;
        (v["fields"]["event"] == event).then_some(v)
    })
}

fn count_event(log: &Path, needle: &str) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(needle))
        .count()
}

#[tokio::test]
async fn limit_pause_holds_row_then_releases_when_window_drains() {
    if !e2e_enabled() {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }

    let dir = std::env::temp_dir().join(format!("quota-stand-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let pct = Arc::new(AtomicU8::new(100)); // окно исчерпано
    let bad = Arc::new(AtomicU8::new(0)); // 401-ответы мока: нарушение контракта
    let quota_port = spawn_quota_mock(pct.clone(), bad.clone()).await;
    let quota_url = format!("http://127.0.0.1:{quota_port}");
    let quota_config = format!(
        "[quota]\nbase_url = \"{quota_url}\"\npath = \"{QUOTA_PATH}\"\napi_key_env = \"{KEY_ENV_STR}\"\n\n"
    );
    let stand = spawn_stand(
        &dir,
        StandOptions {
            config_extra: &quota_config,
            pre_state: None,
            server_env: &[
                (KEY_ENV_STR, E2E_QUOTA_KEY),
                ("UWUWU_MOCK_SEED", "stand:903:In Progress"),
            ],
        },
    )
    .await;
    let http = reqwest::Client::new();
    let server_log = stand.dir.join("server.log");

    // 1. Громкое включение пробы: секция + ключ — quota.enabled в логе.
    assert!(
        count_event(&server_log, EV_ENABLED) >= 1,
        "boot без квота-события — проба молча не включается\n{}",
        log_tail(&server_log)
    );

    // 2. Тишина: спавн ждёт дев-сессию на строке 903; сессию
    // «состариваем» за мягкий гейт (15 мин) без реальных минут.
    let deadline = Instant::now() + Duration::from_secs(30);
    let session = loop {
        let text = std::fs::read_to_string(&server_log).unwrap_or_default();
        match text.lines().find_map(|l| {
            let v: Value = serde_json::from_str(l).ok()?;
            (v["fields"]["event"] == EV_SPAWN).then(|| v["fields"]["session"].clone())
        }) {
            Some(s) if !s.is_null() => break s.as_str().expect("session id").to_string(),
            _ => {
                assert!(
                    Instant::now() < deadline,
                    "дев-спавн строки 903 не дожил\n{}",
                    log_tail(&server_log)
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    };
    let age: reqwest::Response = http
        .post(format!(
            "http://127.0.0.1:{}/api/session/{}/age",
            stand.mock_port, session
        ))
        .json(&json!({ "ms": 20 * 60 * 1000 })) // 20 мин > мягкого гейта 15 мин
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("age session");
    assert!(age.status().is_success(), "ручка age мока отказала");

    // 3. Исчерпанное окно: пауза на строку, счётчик тика, пингов/
    // эскалаций нет — строка держится без ручного разбора.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let paused = last_event(&server_log, EV_PAUSE);
        let tick_ok = last_event(&server_log, EV_TICK)
            .map(|t| t["fields"][TICK_FIELD].as_u64() == Some(1))
            .unwrap_or(false);
        if paused.is_some() && tick_ok {
            break;
        }
        assert_eq!(
            bad.load(Ordering::Relaxed),
            0,
            "мок ответил 401: адаптер ходит не тем GET/не с bearer — контракт адаптер↔эндпоинт нарушен\n{}",
            log_tail(&server_log)
        );
        assert!(
            Instant::now() < deadline,
            "лимитная пауза не наступила на исчерпанном окне\n{}",
            log_tail(&server_log)
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let paused = last_event(&server_log, EV_PAUSE).expect("паузное событие");
    assert_eq!(paused["fields"]["number"], 903, "пауза адресована строке");
    assert_eq!(
        paused["fields"][WIN_PCT], "100",
        "атрибуты — значениями (процент окна в логе)"
    );
    // пингов и эскалаций на паузе нет: мягкий, жёсткий, эскалация
    let text = std::fs::read_to_string(&server_log).unwrap_or_default();
    assert!(
        !text.contains(EV_SOFT)
            && !text.contains(EV_ESC)
            && !text.contains(&format!("{EV_PING}\"")),
        "на исчерпанном окне пинг/эскалация невозможны"
    );

    // 4. Один громкий сигнал на окно: ждём два тика ПОСЛЕ паузы по
    // событию (не слипом) — сигнал прозвучал на первом тике, повторные
    // тики его не дублируют (cooldown ≥ окна).
    let ticks_at_pause = count_event(&server_log, EV_TICK);
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if count_event(&server_log, EV_TICK) >= ticks_at_pause + 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "пауза не пережила два тика\n{}",
            log_tail(&server_log)
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(
        count_event(&server_log, EV_SIGNAL) == 1,
        "громкий лог должен прозвучать один раз на окно ([notify] не задан)"
    );

    // 5. Строка не паркуется: статус жив, парковки нет, комменты не
    // появились.
    let row = mcp_call(
        &http,
        stand.port,
        BEARER,
        "task_get",
        json!({ "number": 903, "project": PROJECT_STAND }),
    )
    .await;
    assert!(
        row.contains(IN_PROGRESS) && !row.contains(NEEDS),
        "лимитная пауза не меняет статус строки: {row}"
    );
    assert!(
        !row.contains(COMM),
        "пауза не пишет комментариев в строку: {row}"
    );

    // 6. Спад процента → самовозобновление: кэш снапшота истекает (TTL
    // 5 мин) → проба меняет вердикт → старая механика (мягкий пинг).
    pct.store(39, Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(360); // TTL + тики
    let mut resumed = false;
    while Instant::now() < deadline {
        if last_event(&server_log, EV_SOFT).is_some() {
            resumed = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    assert!(
        resumed,
        "после спада процента механика не возобновилась\n{}",
        log_tail(&server_log)
    );
    // «ровно один» честно: счёт сверяется после ещё одного тика за
    // возобновлением (soft-интервал 30 мин — второй пинг в окно не
    // попадает), а не в момент первого появления события.
    let ticks_at_resume = count_event(&server_log, EV_TICK);
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if count_event(&server_log, EV_TICK) > ticks_at_resume {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "после возобновления тик не дожил\n{}",
            log_tail(&server_log)
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(
        count_event(&server_log, EV_SIGNAL),
        1,
        "нового окна нет — сигнал не дублируется"
    );
    assert_eq!(
        count_event(&server_log, EV_SOFT),
        1,
        "мягкий пинг ушёл ровно один раз за прогон"
    );
    assert_eq!(
        bad.load(Ordering::Relaxed),
        0,
        "мок ответил 401: адаптер ходит не тем GET/не с bearer — контракт адаптер↔эндпоинт нарушен"
    );

    drop(stand);
    let _ = std::fs::remove_dir_all(&dir);
}
