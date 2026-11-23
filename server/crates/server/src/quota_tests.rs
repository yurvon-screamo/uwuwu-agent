//! Тесты пробы квоты (#1477): парс живой формы (фикстура — паттерн
//! executor_form_pin), граница исчерпания, честный Err на дрейф формы,
//! кэш (один зовн на окно, Err тоже кэшуется), off-конфиг.

use crate::config::QuotaCfg;
use crate::quota::{parse_quota_response, CachedQuotaProbe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use uwuwu_usecase::{QuotaProbe, QuotaSnapshot, UsecaseError, UsecaseResult};

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("../tests/fixtures/quota_fixture.json")).unwrap()
}

#[test]
fn parses_live_form_and_window_is_not_exhausted() {
    let snap = parse_quota_response(&fixture()).unwrap();
    assert_eq!(
        snap,
        QuotaSnapshot {
            exhausted: false,
            window_pct: Some(53),
            reset_at_ms: Some(1795366386852),
        },
        "живая форма: 5h TOKENS_LIMIT распознан, окно на середине"
    );
}

#[test]
fn exhausted_at_hundred_percent() {
    let mut json = fixture();
    json["data"]["limits"][1]["percentage"] = serde_json::json!(100);
    let snap = parse_quota_response(&json).unwrap();
    assert!(
        snap.exhausted,
        "percentage 100 — окно исчерпано, гейт ставит лимитную паузу"
    );
}

#[test]
fn not_exhausted_below_hundred() {
    let mut json = fixture();
    json["data"]["limits"][1]["percentage"] = serde_json::json!(99);
    assert!(
        !parse_quota_response(&json).unwrap().exhausted,
        "99% — ходы ещё идут, пауза не ставится"
    );
}

#[test]
fn missing_5h_window_is_loud_err_not_silent_ok() {
    let mut json = fixture();
    json["data"]["limits"][1]["number"] = serde_json::json!(7);
    assert!(
        parse_quota_response(&json).is_err(),
        "5h-окно исчезло из ответа (дрейф формы / другой план) — Err, гейт отдаёт старую механику"
    );
}

#[test]
fn success_false_is_err() {
    let mut json = fixture();
    json["success"] = serde_json::json!(false);
    assert!(
        parse_quota_response(&json).is_err(),
        "чужой ключ/отказ — Err, не маскируется «окном живо»"
    );
}

struct CountingProbe {
    calls: AtomicUsize,
    result: Result<QuotaSnapshot, UsecaseError>,
}

#[async_trait::async_trait]
impl QuotaProbe for CountingProbe {
    async fn probe(&self) -> UsecaseResult<QuotaSnapshot> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.clone()
    }
}

fn snapshot(exhausted: bool) -> QuotaSnapshot {
    QuotaSnapshot {
        exhausted,
        window_pct: Some(if exhausted { 100 } else { 53 }),
        reset_at_ms: None,
    }
}

#[tokio::test]
async fn cache_serves_snapshot_within_ttl_with_single_call() {
    let inner = Arc::new(CountingProbe {
        calls: AtomicUsize::new(0),
        result: Ok(snapshot(true)),
    });
    let cached = CachedQuotaProbe::new(inner.clone());
    let first = cached.probe().await.unwrap();
    let second = cached.probe().await.unwrap();
    assert_eq!(first, second, "кэш отдаёт тот же снимок");
    assert_eq!(
        inner.calls.load(Ordering::Relaxed),
        1,
        "один GET на окно кэша, не на строку гейта"
    );
}

#[tokio::test]
async fn cache_caches_err_to_not_storm_the_gate() {
    let inner = Arc::new(CountingProbe {
        calls: AtomicUsize::new(0),
        result: Err(UsecaseError::State("down (мок)".into())),
    });
    let cached = CachedQuotaProbe::new(inner.clone());
    assert!(cached.probe().await.is_err());
    assert!(cached.probe().await.is_err());
    assert_eq!(
        inner.calls.load(Ordering::Relaxed),
        1,
        "Err тоже кэшуется: недоступный эндпоинт не штормит гейт"
    );
}

#[test]
fn cfg_section_parses_with_default_path() {
    let cfg: QuotaCfg = serde_json::from_value(serde_json::json!({
        "base_url": "https://api.z.ai",
        "api_key_env": "UWUWU_ZAI_API_KEY"
    }))
    .unwrap();
    assert_eq!(cfg.base_url, "https://api.z.ai");
    assert_eq!(
        cfg.path, "/api/monitor/usage/quota/limit",
        "дефолт пути — форма api.z.ai"
    );
    assert_eq!(cfg.api_key_env, "UWUWU_ZAI_API_KEY");
}

#[tokio::test]
async fn missing_key_env_is_loud_startup_err() {
    let cfg = QuotaCfg {
        base_url: "https://api.z.ai".into(),
        path: "/api/monitor/usage/quota/limit".into(),
        api_key_env: "UWUWU_QUOTA_TEST_MISSING_KEY".into(),
    };
    let err = match crate::quota::quota_probe_from_cfg(&cfg) {
        Err(e) => e,
        Ok(_) => panic!("секция включена без ключа — старт должен отказать"),
    };
    assert!(
        err.to_string().contains("UWUWU_QUOTA_TEST_MISSING_KEY"),
        "секция включена, ключа нет — стартовый отказ с именем env, не тихий off"
    );
}
