//! E2E основного сценария #1480 (гейт UWUWU_E2E=1): живой uwuwu-server
//! (hub driver=memory, executor = mock_opencode) файрит кроны — машинный
//! учёт ранов в state.json, без журнального носителя.
//!
//! Сценарий (главный путь UC-02, то, что тестировщик гонял руками на
//! стенде scratch/stand):
//!   1. файр: state-запись до спавна (last_fire + run-трек), сессия с
//!      тайтлом «⏰ …» на проект-исполнителе;
//!   2. завершение рана: сеттлмент снимает трек с сессии исполнителя
//!      (outcome succeeded → трек закрыт, отказов нет);
//!   3. `cron_list` показывает машинный учёт (last fire, fails);
//!   4. журнала-отчёта не существует: docs_ls cron — только README,
//!      ни один док не появился как отчётная сводка.
//!
//! Запуск: UWUWU_E2E=1 cargo test -p uwuwu-server --test cron_stand_e2e

mod common;

use common::stand::{e2e_enabled, log_tail, mcp_call, spawn_stand, StandOptions, BEARER};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

#[tokio::test]
async fn cron_fire_machine_accounting_without_journal() {
    if !e2e_enabled() {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }

    let dir = std::env::temp_dir().join(format!("cron-stand-e2e-{}", std::process::id()));

    let masters = json!({
        "cron/e2e-zayavitel/README": "---\ntitle: E2E заявитель\nschedule: 1m\nproject: stand\n---\n\nЗаявитель: находка → заявка. Журнала нет.\n",
        "cron/e2e-isp/README": "---\ntitle: E2E исполнитель\nschedule: 1m\nproject: stand\n---\n\nИсполнитель: тихий тик в целевом носителе.\n"
    });
    let masters_json = masters.to_string();
    let stand = spawn_stand(
        &dir,
        StandOptions {
            config_extra: "",
            pre_state: None,
            server_env: &[("UWUWU_MOCK_DOCS", &masters_json)],
            capture_sessions: false,
        },
    )
    .await;
    let http = reqwest::Client::new();

    // 1+2. Файр + сеттлмент: оба мастера с last_fire, треки сняты, отказов
    // нет (мок plain: ход завершается succeeded за step_ms, следующий тик
    // закрывает трек). Тик ~10 c; якорная семантика (#1517) сдвигает
    // первый файр на интервал после якоря — окно с запасом на три тика.
    let state_path = stand.dir.join("state.json");
    let poll_deadline = Instant::now() + Duration::from_secs(120);
    let state: Value = loop {
        assert!(
            Instant::now() < poll_deadline,
            "state не дожил до учётного вида\nstate: {:?}\n{}",
            std::fs::read_to_string(&state_path).unwrap_or_default(),
            log_tail(&stand.dir.join("server.log"))
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        let Ok(text) = std::fs::read_to_string(&state_path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let crons = &v["crons"];
        let z = &crons["e2e-zayavitel"];
        let i = &crons["e2e-isp"];
        let fired = |row: &Value| row["last_fire_ms"].as_u64().unwrap_or(0) > 0;
        let settled = |row: &Value| row.get("run").is_none();
        let clean = |row: &Value| row["fails"].as_u64().unwrap_or(0) == 0;
        if [z, i].iter().all(|r| fired(r) && settled(r) && clean(r)) {
            break v;
        }
    };
    assert!(
        state["crons"]["e2e-zayavitel"]["last_fire_ms"].is_u64(),
        "last_fire not machine-recorded"
    );

    // Сессии ранов на моке: тайтл «⏰ …» (UC-05), directory — чекаут проекта.
    let sessions: Value = http
        .get(format!("http://127.0.0.1:{}/api/session", stand.mock_port))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("mock sessions")
        .json()
        .await
        .expect("mock sessions JSON");
    let rows = sessions["data"].as_array().expect("session list");
    let cron_rows: Vec<&Value> = rows
        .iter()
        .filter(|s| {
            s["title"]
                .as_str()
                .map(|t| t.starts_with("⏰ "))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        cron_rows.len() >= 2,
        "ожидаем раны обоих мастеров, есть: {cron_rows:?}"
    );
    for s in &cron_rows {
        let dir_of = s["location"]["directory"].as_str().unwrap_or_default();
        assert_eq!(
            dir_of,
            format!("{}/checkout", dir.display()),
            "ран спавнится в checkout проекта"
        );
    }

    // 3. cron_list — машинный учёт наружу: last fire + fails, без «never».
    let list = mcp_call(&http, stand.port, BEARER, "cron_list", json!({})).await;
    assert!(
        list.contains("E2E заявитель") && list.contains("E2E исполнитель"),
        "{list}"
    );
    // Штамп живой по форме «· last fire YYYY-» — год не пинаем (стена часов
    // умирает ретроактивно), форма JST-штампа — пин машинного учёта.
    let stamp_at = list
        .find("· last fire ")
        .map(|i| &list[i + "· last fire ".len()..]);
    let stamp = stamp_at.unwrap_or_else(|| panic!("last fire не показан: {list}"));
    assert!(
        stamp.len() >= 5
            && stamp.as_bytes()[..4].iter().all(|b| b.is_ascii_digit())
            && stamp.as_bytes()[4] == b'-',
        "last fire — не штамп года: {list}"
    );
    assert!(list.contains("fails 0"), "fails не показан: {list}");
    assert!(
        !list.contains("never"),
        "после файра «never» недопустим: {list}"
    );

    // 4. Журнала нет: инвентарь /cron — только README-мастера, ни одного
    // отчётного дока (снесённый носитель не возрождается).
    let docs = mcp_call(
        &http,
        stand.port,
        BEARER,
        "docs_ls",
        json!({ "path": "cron" }),
    )
    .await;
    assert!(docs.contains("cron/e2e-zayavitel/README"), "{docs}");
    assert!(docs.contains("cron/e2e-isp/README"), "{docs}");
    assert!(
        !docs.contains("Журнал"),
        "журнальный носитель возродился: {docs}"
    );
    assert_eq!(
        docs.lines().filter(|l| !l.trim().is_empty()).count(),
        2,
        "в /cron появились посторонние доки: {docs}"
    );

    drop(stand);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn corrupt_state_restart_anchors_without_catchup() {
    // Рестарт с испорченным state (#1517): учёт, оставшийся от прошлого
    // прогона, не парсится → сервер стартует громко (улика рядом), кроны
    // ЯКОРЯТСЯ вместо догона прошлых плановых минут: спавнов нет, учёт
    // честный («never»). Прежнее поведение на пустом state — масс-файр.
    if !e2e_enabled() {
        println!("e2e выключен: UWUWU_E2E=1 для пробы живого стенда");
        return;
    }

    let dir = std::env::temp_dir().join(format!("cron-stand-e2e-corrupt-{}", std::process::id()));
    let masters = json!({
        "cron/e2e-zayavitel/README": "---\ntitle: E2E заявитель\nschedule: 1m\nproject: stand\n---\n\nЗаявитель: находка → заявка.\n",
        "cron/e2e-isp/README": "---\ntitle: E2E исполнитель\nschedule: 1m\nproject: stand\n---\n\nИсполнитель: тихий тик.\n"
    });
    let masters_json = masters.to_string();
    // Учёт прошлого прогона, испорченный между запусками (класс: гонка
    // записи/усечённый том). Интервальные кроны на пустом state прежде
    // файрились сразу — теперь якорь.
    let stand = spawn_stand(
        &dir,
        StandOptions {
            config_extra: "",
            pre_state: Some("{ corrupted accounting"),
            server_env: &[("UWUWU_MOCK_DOCS", &masters_json)],
            capture_sessions: false,
        },
    )
    .await;
    let http = reqwest::Client::new();

    // Улика: битый файл сохранён рядом со штампом, сервер стартовал громко.
    let mut artifact = None;
    for entry in std::fs::read_dir(&dir).expect("dir") {
        let name = entry.expect("entry").file_name();
        if name.to_string_lossy().starts_with("state.json.corrupt-") {
            artifact = Some(name);
        }
    }
    assert!(artifact.is_some(), "улики corrupt-state нет в {:?}", dir);
    let server_log = std::fs::read_to_string(stand.dir.join("server.log")).unwrap_or_default();
    assert!(
        server_log.contains("state.load_corrupt"),
        "старт с битым state должен быть громким (state.load_corrupt):\n{server_log}"
    );

    // Первый тик якорит оба крона: state.json перезаписан валидно, в
    // учёте якоря, last_fire нет; спавнов нет.
    let state_path = stand.dir.join("state.json");
    let poll_deadline = Instant::now() + Duration::from_secs(45);
    let state: Value = loop {
        assert!(
            Instant::now() < poll_deadline,
            "state не дожил до якорного вида\n{}",
            log_tail(&stand.dir.join("server.log"))
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        let Ok(text) = std::fs::read_to_string(&state_path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let crons = &v["crons"];
        let anchored = |row: &Value| {
            row["anchor_ms"].is_u64() && !row["last_fire_ms"].is_u64() && row.get("run").is_none()
        };
        let rows = [&crons["e2e-zayavitel"], &crons["e2e-isp"]];
        if rows.iter().all(|r| anchored(r)) {
            break v;
        }
    };
    assert!(
        state["crons"]["e2e-zayavitel"]["anchor_ms"].is_u64(),
        "якорь первой встречи не записан: {}",
        state
    );

    // Спавнов нет (якорь не файрит) и учёт наружу честный — «never».
    let sessions: Value = http
        .get(format!("http://127.0.0.1:{}/api/session", stand.mock_port))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("mock sessions")
        .json()
        .await
        .expect("mock sessions JSON");
    let rows = sessions["data"].as_array().expect("session list");
    assert!(
        !rows.iter().any(|s| s["title"]
            .as_str()
            .map(|t| t.starts_with("⏰ "))
            .unwrap_or(false)),
        "якорь спавнит раны — регрессия масс-догона: {rows:?}"
    );
    let list = mcp_call(&http, stand.port, BEARER, "cron_list", json!({})).await;
    assert!(
        list.contains("last fire never"),
        "после якоря учёт обязан показывать never: {list}"
    );

    drop(stand);
    let _ = std::fs::remove_dir_all(&dir);
}
