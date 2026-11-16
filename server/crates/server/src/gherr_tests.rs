//! Тесты классификатора GitHub-отказов (#1156): классы 403/429 по
//! заголовкам (включая комбинированный кейс), формат reset-времени,
//! graphql-детект в 200-ответе.

use crate::gherr::{classify, classify_gql_errors, format_reset, GhFail};
use chrono::{Duration, TimeZone, Utc};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::StatusCode;
use serde_json::json;

fn hdr(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
    }
    map
}

#[test]
fn core_limit_when_remaining_is_zero() {
    let headers = hdr(&[
        ("X-RateLimit-Remaining", "0"),
        ("X-RateLimit-Reset", "1800000000"),
    ]);
    let got = classify(StatusCode::FORBIDDEN, &headers);
    assert_eq!(
        got,
        Some(GhFail::Core {
            reset_epoch: Some(1800000000)
        })
    );
}

#[test]
fn core_wins_when_retry_after_also_present() {
    let headers = hdr(&[("X-RateLimit-Remaining", "0"), ("Retry-After", "60")]);
    let got = classify(StatusCode::TOO_MANY_REQUESTS, &headers);
    assert_eq!(got, Some(GhFail::Core { reset_epoch: None }));
}

#[test]
fn secondary_when_only_retry_after_is_set() {
    let headers = hdr(&[("Retry-After", "45"), ("X-RateLimit-Remaining", "4200")]);
    let got = classify(StatusCode::FORBIDDEN, &headers);
    assert_eq!(
        got,
        Some(GhFail::Secondary {
            retry_after_secs: 45
        })
    );
}

#[test]
fn permission_on_bare_403() {
    let got = classify(StatusCode::FORBIDDEN, &hdr(&[]));
    assert_eq!(got, Some(GhFail::Permission));
}

#[test]
fn other_statuses_and_success_are_not_classified() {
    assert_eq!(classify(StatusCode::NOT_FOUND, &hdr(&[])), None);
    assert_eq!(
        classify(StatusCode::CONFLICT, &hdr(&[("Retry-After", "9")])),
        None
    );
    assert_eq!(
        classify(StatusCode::OK, &hdr(&[("X-RateLimit-Remaining", "0")])),
        None,
        "успешный ответ не ошибка даже с обнулённой квотой"
    );
    assert_eq!(
        classify(StatusCode::TOO_MANY_REQUESTS, &hdr(&[])),
        None,
        "bare 429 без rate-limit признаков не классифицируется (текущее поведение)"
    );
}

#[test]
fn core_message_carries_reset_time_and_wait_minutes() {
    let reset = Utc.timestamp_opt(1800000000, 0).single().expect("epoch");
    let now = reset - Duration::minutes(8);
    let text = format_reset(1800000000, now);
    assert!(
        text.contains("2027-01-15 08:00"),
        "absolute reset time: {text}"
    );
    assert!(text.contains("~8 мин"), "relative wait: {text}");
}

#[test]
fn core_message_is_loud_without_reset_header() {
    let text = GhFail::Core { reset_epoch: None }.to_string();
    assert!(text.contains("rate limit"), "class name present: {text}");
    assert!(
        text.contains("подожди или отложи ход"),
        "agent recipe: {text}"
    );
}

#[test]
fn secondary_message_names_retry_after_value() {
    let text = GhFail::Secondary {
        retry_after_secs: 45,
    }
    .to_string();
    assert!(text.contains("45"), "retry seconds present: {text}");
    assert!(text.contains("secondary"), "class name present: {text}");
}

#[test]
fn permission_message_suggests_token_check() {
    let text = GhFail::Permission.to_string();
    assert!(text.contains("403"), "status present: {text}");
    assert!(text.contains("GH_TOKEN"), "recipe present: {text}");
}

#[test]
fn graphql_rate_limited_errors_are_detected() {
    let body =
        json!({ "errors": [{ "type": "RATE_LIMITED", "message": "API rate limit exceeded" }] });
    assert_eq!(
        classify_gql_errors(&body),
        Some(GhFail::Core { reset_epoch: None })
    );
}

#[test]
fn graphql_unrelated_errors_and_success_are_not_classified() {
    let body =
        json!({ "errors": [{ "type": "NOT_FOUND", "message": "Could not resolve to an Issue" }] });
    assert_eq!(classify_gql_errors(&body), None);
    assert_eq!(classify_gql_errors(&json!({ "data": {} })), None);
}
