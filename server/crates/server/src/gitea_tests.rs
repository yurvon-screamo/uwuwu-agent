//! Тесты Gitea PR-шлюза (#1269): строгий парс `/pulls/<n>` (живой фрейм
//! foilcap#31 / PR #74), классификация probe-тела (state/merged/mergeable,
//! mergeable-отсутствие → Pending по симметрии #1232), merge-ответы PUT
//! (успех / конфликт / 404 / 401 / 5xx без тела — merge-пин ревью плана).

use crate::gitea::{classify_merge, classify_pr, parse_gitea_pr_url};
use serde_json::json;
use uwuwu_usecase::PrState;

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
