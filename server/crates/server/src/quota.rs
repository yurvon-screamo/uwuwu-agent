//! Проба квоты подписки провайдера моделей (#1477): адаптер лимит-гейта
//! сталл-детекта. Привязка к провайдеру — только этот модуль + `[quota]`
//! конфиг (требование владельца 24.11.2026: «запаковать за трейты»):
//! ядро конвейера знает трейт `QuotaProbe`, не провайдера. Форма ответа
//! пинится фикстурой (`quota_fixture.json`) — дрейф формы краснит тест.

use crate::config::QuotaCfg;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uwuwu_usecase::{QuotaProbe, QuotaSnapshot, UsecaseError, UsecaseResult};

/// TTL кэша снимка: окно исчерпания живёт часами, гейт спрашивает
/// несколько строк за тик — один GET на окно кэша, не на строку.
const CACHE_TTL: Duration = Duration::from_secs(300);

pub fn quota_probe_from_cfg(cfg: &QuotaCfg) -> UsecaseResult<Arc<dyn QuotaProbe>> {
    let api_key = std::env::var(&cfg.api_key_env).map_err(|_| {
        UsecaseError::State(format!(
            "env {} не задан: секция [quota] включена, ключ провайдера отсутствует \
             (прецедент [notify]: секция без env — стартовый отказ, не тихий off)",
            cfg.api_key_env
        ))
    })?;
    let url = format!("{}{}", cfg.base_url.trim_end_matches('/'), cfg.path);
    Ok(Arc::new(CachedQuotaProbe::new(Arc::new(
        ZaiQuotaProbe::new(url, api_key),
    ))))
}

/// HTTP-клиент лимит-эндпоинта провайдера: GET bearer, парс 5h-окна
/// TOKENS_LIMIT (unit=3, number=5) → exhausted = percentage ≥ 100.
/// `remaining` у TOKENS_LIMIT в живом ответе отсутствует — факт окна
/// только процентом; резет-время переносится как есть, ядро его не
/// вычисляет (пауза по факту повторных проб — #1477).
pub struct ZaiQuotaProbe {
    http: reqwest::Client,
    url: String,
    api_key: String,
}

impl ZaiQuotaProbe {
    pub fn new(url: String, api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            url,
            api_key,
        }
    }
}

#[async_trait::async_trait]
impl QuotaProbe for ZaiQuotaProbe {
    async fn probe(&self) -> UsecaseResult<QuotaSnapshot> {
        let resp = self
            .http
            .get(&self.url)
            .bearer_auth(&self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| UsecaseError::State(format!("quota probe transport: {e}")))?;
        if !resp.status().is_success() {
            return Err(UsecaseError::State(format!(
                "quota probe status {}",
                resp.status()
            )));
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| UsecaseError::State(format!("quota probe body: {e}")))?;
        parse_quota_response(&json)
    }
}

/// Парс формы ответа: `{success, data:{limits:[{type, unit, number,
/// percentage, nextResetTime}]}}`. 5h-окно — TOKENS_LIMIT с unit=3,
/// number=5. Окно не найдено / success=false — Err (громко: дрейф формы
/// или чужой ключ не маскируются «окно живо» — гейт отдаёт старую
/// механику, warn виден в логе).
pub fn parse_quota_response(json: &serde_json::Value) -> UsecaseResult<QuotaSnapshot> {
    if json["success"].as_bool() != Some(true) {
        return Err(UsecaseError::State(
            "quota probe: success != true (чужой ключ или дрейф формы)".into(),
        ));
    }
    let limits = json["data"]["limits"]
        .as_array()
        .ok_or_else(|| UsecaseError::State("quota probe: limits не массив".into()))?;
    let window = limits
        .iter()
        .find(|l| l["type"] == "TOKENS_LIMIT" && l["unit"] == 3 && l["number"] == 5)
        .ok_or_else(|| UsecaseError::State("quota probe: 5h TOKENS_LIMIT не найден".into()))?;
    let pct = window["percentage"].as_u64();
    Ok(QuotaSnapshot {
        exhausted: pct.is_some_and(|p| p >= 100),
        window_pct: pct.map(|p| p.min(255) as u8),
        reset_at_ms: window["nextResetTime"].as_u64(),
    })
}

/// Кэш снимка (глобальный факт подписки, не пер-сессионный): Err тоже
/// кэшируется — недоступный эндпоинт не штормит warn-логом на каждую
/// строку гейта в тике.
pub struct CachedQuotaProbe {
    inner: Arc<dyn QuotaProbe>,
    cell: Mutex<Option<(Instant, UsecaseResult<QuotaSnapshot>)>>,
}

impl CachedQuotaProbe {
    pub fn new(inner: Arc<dyn QuotaProbe>) -> Self {
        Self {
            inner,
            cell: Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl QuotaProbe for CachedQuotaProbe {
    async fn probe(&self) -> UsecaseResult<QuotaSnapshot> {
        let fresh = {
            let cell = self.cell.lock().unwrap();
            match &*cell {
                Some((at, snap)) if at.elapsed() < CACHE_TTL => Some(snap.clone()),
                _ => None,
            }
        };
        if let Some(snap) = fresh {
            return snap;
        }
        let snap = self.inner.probe().await;
        *self.cell.lock().unwrap() = Some((Instant::now(), snap.clone()));
        snap
    }
}
