//! Тесты Gitea PR-шлюза (#1269): строгий парс `/pulls/<n>` (живой фрейм
//! foilcap#31 / PR #74), классификация probe-тела (state/merged/mergeable,
//! mergeable-отсутствие → Pending по симметрии #1232), merge-ответы PUT
//! (успех / конфликт / 404 / 401 / 5xx без тела — merge-пин ревью плана).

use crate::gitea::{classify_merge, classify_pr, parse_gitea_pr_url, GiteaPrGateway};
use serde_json::json;
use std::collections::HashMap;
use uwuwu_usecase::{PrGateway, PrState};

#[test]
fn parses_gitea_pr_urls() {
    // #1269: живой фрейм foilcap#31 / PR #74
    assert_eq!(
        parse_gitea_pr_url("https://git.l4a.ru/ek/ai-generator/pulls/74"),
        Some(("git.l4a.ru".into(), "ek".into(), "ai-generator".into(), 74))
    );
    assert_eq!(
        parse_gitea_pr_url("https://git.true-wash.com/a/b/pulls/9"),
        Some(("git.true-wash.com".into(), "a".into(), "b".into(), 9))
    );
    assert_eq!(
        parse_gitea_pr_url("https://git.l4a.ru/o/r/pulls/74/"),
        Some(("git.l4a.ru".into(), "o".into(), "r".into(), 74)),
        "трейлинг-слэш не рвёт форму"
    );
}

#[test]
fn rejects_non_pr_urls() {
    assert_eq!(parse_gitea_pr_url("https://github.com/a/b/pull/5"), None);
    assert_eq!(parse_gitea_pr_url("https://git.l4a.ru/o/r/issues/5"), None);
    assert_eq!(parse_gitea_pr_url("https://git.l4a.ru/o/r/pull/5"), None);
    assert_eq!(parse_gitea_pr_url("https://git.l4a.ru/o/r/pulls/abc"), None);
    assert_eq!(parse_gitea_pr_url("https://git.l4a.ru/o/r/pulls/"), None);
    assert_eq!(
        parse_gitea_pr_url("https://git.l4a.ru/o/r/pulls/5/files"),
        None
    );
    assert_eq!(parse_gitea_pr_url("мусор"), None);
}

#[test]
fn classifies_pr_body() {
    assert_eq!(classify_pr(&json!({ "merged": true })), PrState::Merged);
    assert_eq!(
        classify_pr(&json!({ "state": "closed", "merged": false })),
        PrState::Closed
    );
    assert_eq!(
        classify_pr(&json!({ "state": "open", "mergeable": true })),
        PrState::Open {
            mergeable: true,
            ci_green: true
        },
        "#447: репо без PR-CI — зелёный"
    );
    assert_eq!(
        classify_pr(&json!({ "state": "open", "mergeable": false })),
        PrState::Open {
            mergeable: false,
            ci_green: true
        }
    );
    assert_eq!(
        classify_pr(&json!({ "state": "open", "mergeable": null })),
        PrState::Pending,
        "инстанс ещё считает — симметрия GitHub #1232"
    );
    assert_eq!(
        classify_pr(&json!({ "state": "open" })),
        PrState::Pending,
        "mergeable отсутствует — pending, не ложный конфликт"
    );
}

#[test]
fn classifies_merge_responses() {
    assert!(classify_merge(201, "").is_ok());
    assert!(classify_merge(200, "").is_ok());
    let conflict = classify_merge(405, r#"{"message": "pull request has not been merged"}"#);
    assert!(conflict.is_err());
    assert!(conflict.unwrap_err().contains("not been merged"));
    assert!(classify_merge(404, "404 page not found").is_err());
    assert!(classify_merge(401, "unauthorized").is_err());
    assert!(
        classify_merge(500, "").unwrap_err().contains("HTTP 500"),
        "пустое тело — статус в сообщении"
    );
}

// ——— probe-шлюз без сети (детерминированные гарды деградационной матрицы) ———

#[tokio::test]
async fn probe_without_token_is_foreign_without_api_call() {
    // #1269, развилка В1: инстанс без записи в GITEA_TOKENS — Foreign без
    // API-проб (true-wash-класс тих, чужие инстансы не щупаются токеном);
    // сеть не зовётся — мапа токенов пуста, соединению не с чем идти
    let gw = GiteaPrGateway::new(HashMap::new());
    let state = gw
        .probe(
            "hub/o/r",
            Some("https://git.true-wash.com/a/b/pulls/9"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(state, PrState::Foreign, "без токена — Foreign, не Missing");
}

#[tokio::test]
async fn probe_unparsable_pr_url_is_missing() {
    // не-Gitea-URL/пустая ячейка у gitea-плеча — Missing (bounce честен);
    // диспетчер такие сюда не шлёт, но шлюз не паника и не выдумывает вендора
    let gw = GiteaPrGateway::new(HashMap::new());
    let state = gw
        .probe("hub/o/r", Some("https://git.l4a.ru/o/r/issues/5"), None)
        .await
        .unwrap();
    assert_eq!(state, PrState::Missing);
    let state = gw.probe("hub/o/r", None, None).await.unwrap();
    assert_eq!(state, PrState::Missing);
}

#[tokio::test]
async fn probe_transport_failure_is_err() {
    // транспорт-сбой (отказ соединения на мёртвом порту) — Err, как у
    // GitHub: это не вендор-вопрос, тик прокидывает его на весь проход
    // (tick.rs:660), а не глотает в Foreign
    let gw = GiteaPrGateway::new(HashMap::from([(
        "127.0.0.1:1".to_string(),
        "tok".to_string(),
    )]));
    let err = gw
        .probe("hub/o/r", Some("https://127.0.0.1:1/o/r/pulls/5"), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("transport"), "{err}");
}

// ——— живой e2e: реальный Gitea-инстанс (гвард UWUWU_E2E=1) ———
// Без UWUWU_E2E=1 сразу зелёный (гвард по культуре репо, образец —
// lib/qlty-review.test.ts). Живой прогон требует GITEA_TOKENS
// («host=token,...») с записью хоста git.l4a.ru.
#[tokio::test]
async fn live_probe_real_gitea_pr_is_not_missing() {
    if std::env::var("UWUWU_E2E").as_deref() != Ok("1") {
        return;
    }
    let raw = std::env::var("GITEA_TOKENS").unwrap_or_default();
    assert!(!raw.is_empty(), "UWUWU_E2E=1: нужен GITEA_TOKENS");
    let tokens: HashMap<String, String> = raw
        .split(',')
        .filter_map(|pair| pair.trim().split_once('='))
        .map(|(h, t)| (h.trim().to_string(), t.trim().to_string()))
        .collect();
    let gw = GiteaPrGateway::new(tokens);
    // #1269, красный тест на живом фрейме foilcap#31: открытый PR #74 на
    // git.l4a.ru исходно классифицировался Missing → ложный bounce; шлюз
    // обязан классифицировать его по факту (Open/Pending/Merged/Closed).
    // Сеть до инстанса флеймит (потери пакетов): транспортный Err ретраим
    // трижды — та же семантика, что у тика в проде (Err → ретрай следующим
    // тиком); не-транспортные исходы — факты, не ретраятся.
    let url = "https://git.l4a.ru/ek/ai-generator/pulls/74";
    let mut state = None;
    for attempt in 1..=3 {
        match gw.probe("unused/repo", Some(url), None).await {
            Ok(s) => {
                state = Some(s);
                break;
            }
            Err(e) if e.to_string().contains("transport") => {
                eprintln!("live probe попытка {attempt}: транспорт {e}");
            }
            Err(e) => panic!("живая проба PR #74, не-транспортная ошибка: {e}"),
        }
    }
    let state = state.expect("живая проба PR #74: транспорт не ответил за 3 попытки");
    assert!(
        !matches!(state, PrState::Missing | PrState::Foreign),
        "живой PR не Missing/Foreign, факт: {state:?}"
    );
    println!("live probe PR #74 вердикт: {state:?}");
    if let PrState::Open { ci_green, .. } = &state {
        assert!(ci_green, "#447: репо без PR-CI — зелёный");
    }
}
