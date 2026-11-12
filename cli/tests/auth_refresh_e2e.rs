//! E2E-пин 401-самолечения auth-контура (задача 20261111233714-96wqwp4,
//! PR #231): пост-путь с мусорным env-токеном должен самолечиться — один
//! 401 триггерит рефреш по walk-past-equal цепочке (fish vars → conf.json
//! → docker exec), повтор идёт со свежим токеном и команда отвечает
//! живыми данными. Probe-путь (`kernel ping`) — one-shot по контракту:
//! рефреша нет, мусорный env даёт честный 401 (пин дельты ревью: проба
//! самолечения ранбука идёт именно командой post-пути).
//!
//! Запуск: UWUWU_E2E=1 cargo test --test auth_refresh_e2e — нужно живое
//! ядро и живой носитель рефреша (fish var актуального токена либо docker
//! exec siyuan-demo). Без UWUWU_E2E тесты сразу зелёные (CI без ядра).
//! Read-only: рефрез читает носители, мутаций доски/вики нет; по ядру —
//! один недавний 401 на вызов (долбления нет: post с рефрешом лечится
//! со второй попытки).

use std::process::Command;

fn e2e_enabled() -> bool {
    std::env::var("UWUWU_E2E").as_deref() == Ok("1")
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_uwuwu-cli")
}

fn run_with_token(args: &[&str], token: Option<&str>) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    match token {
        Some(t) => {
            cmd.env("UWUWU_SIYUAN_TOKEN", t);
        }
        None => {
            cmd.env_remove("UWUWU_SIYUAN_TOKEN");
        }
    }
    let out = cmd.output().expect("spawn uwuwu-cli");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Контроль границы: штатный вызов без env-оверрайда зелёный (носители
/// живы). Красный здесь = сломано ядро/носители контура, а не рефреш —
/// диагностический якорь для остальных тестов файла.
#[test]
fn healthy_env_post_path_is_green() {
    if !e2e_enabled() {
        return;
    }
    let (code, stdout, stderr) = run_with_token(&["projects"], None);
    assert_eq!(
        code,
        Some(0),
        "штатный post-путь на живом контуре должен быть зелёным; stderr: {stderr}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "перечень проектов должен приходить в stdout, получено: {stdout:?}"
    );
}

/// Главный сценарий самолечения (ручной прогон задачи, шаг 2): мусорный
/// env-токен → 401 на первом post → walk-past-equal рефреш по живым
/// носителям → повтор со свежим токеном → живой ответ, exit 0, без
/// «Error». До фикса этот вызов ретраил 4×401 и падал.
#[test]
fn post_path_self_heals_on_stale_env_token() {
    if !e2e_enabled() {
        return;
    }
    let (code, stdout, stderr) = run_with_token(&["projects"], Some("garbage-deadbeef"));
    assert_eq!(
        code,
        Some(0),
        "мусорный env должен самолечиться через рефреш и дать exit 0; stderr: {stderr}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "после самолечения живой перечень приходит в stdout, получено: {stdout:?}"
    );
    let all = format!("{stdout}{stderr}");
    assert!(
        !all.contains("Error"),
        "самолечение должно проходить без ошибок в выводе; вывод: {all}"
    );
}

/// Контракт one-shot probe-пути: `kernel ping` рефреша не имеет, мусорный
/// env падает громким 401 «Auth failed» с exit 1 — ровно как это теперь
/// описывает ранбук ротации (шаг верификации самолечения — команды
/// post-пути, не kernel ping).
#[test]
fn probe_path_stays_oneshot_401() {
    if !e2e_enabled() {
        return;
    }
    let (code, stdout, stderr) = run_with_token(&["kernel", "ping"], Some("garbage-deadbeef"));
    assert_eq!(
        code,
        Some(1),
        "probe-путь с мусорным env должен падать с exit 1; stdout: {stdout} stderr: {stderr}"
    );
    let all = format!("{stdout}{stderr}");
    assert!(
        all.contains("401") && all.contains("Auth failed"),
        "probe-путь должен честно сообщать HTTP 401 Auth failed; вывод: {all}"
    );
}
