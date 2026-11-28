//! Проба квоты подписки провайдера моделей (#1477): адаптер лимит-гейта
//! сталл-детекта. Привязка к провайдеру — только этот модуль + `[quota]`
//! конфиг (требование владельца 23.11.2026, #1477: «запаковать за трейты»):
//! ядро конвейера знает трейт `QuotaProbe`, не провайдера. Парсер сверён
//! с фикстурой живой формы (`quota_fixture.json`); живой дрейф формы
//! не краснит тест — он виден как Err → гейт отдаёт старую механику.

use crate::config::QuotaCfg;
use crate::github::http_timeout;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uwuwu_usecase::{QuotaProbe, QuotaSnapshot, UsecaseError, UsecaseResult};

/// TTL кэша снимка: окно исчерпания живёт часами, гейт спрашивает
/// несколько строк за тик — один вызов на окно кэша, не на строку.
const CACHE_TTL: Duration = Duration::from_secs(300);
/// Таймаут одного вызова пробы (вызов идёт внутри тика: зависание — не
/// Err, ограничиваем на клиенте и на вызове; прецедент judge.rs).
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

pub fn quota_probe_from_cfg(cfg: &QuotaCfg) -> UsecaseResult<Arc<dyn QuotaProbe>> {
    let api_key = std::env::var(&cfg.api_key_env)
        .ok()
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            UsecaseError::State(format!(
                "env {} не задан или пуст: секция [quota] включена, ключ провайдера \
                 отсутствует (прецедент [notify]: секция без env — стартовый отказ, \
                 не тихий off)",
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
/// `remaining` в живом ответе null — факт окна несёт только процент;
/// резет-время переносится как есть, ядро его не вычисляет (пауза по
/// факту повторных проб — #1477).
pub struct ZaiQuotaProbe {
    http: reqwest::Client,
    url: String,
    api_key: String,
    call_timeout: Duration,
}

impl ZaiQuotaProbe {
    pub fn new(url: String, api_key: String) -> Self {
        Self::with_call_timeout(url, api_key, CALL_TIMEOUT)
    }

    /// Тест-шов таймаута (прецедент sticky-retry delayOf): реальный
    /// клиент с укороченным таймаутом против медленного локального
    /// сервера — зависание вида Err.
    pub(crate) fn with_call_timeout(url: String, api_key: String, call_timeout: Duration) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("uwuwu-server/", env!("CARGO_PKG_VERSION")))
                .timeout(http_timeout())
                .build()
                .expect("reqwest client"),
            url,
            api_key,
            call_timeout,
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
            .timeout(self.call_timeout)
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
/// number=5. Окно не найдено / success=false / percentage не число —
/// Err (громко: дрейф формы или чужой ключ не маскируются «окно живо» —
/// гейт отдаёт старую механику, warn виден в логе).
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
    let pct = window["percentage"].as_u64().ok_or_else(|| {
        UsecaseError::State("quota probe: percentage не число (дрейф формы)".into())
    })?;
    Ok(QuotaSnapshot {
        exhausted: pct >= 100,
        window_pct: Some(pct.min(255) as u8),
        reset_at_ms: window["nextResetTime"].as_u64(),
    })
}

/// Кэш снимка (глобальный факт подписки, не пер-сессионный): Err тоже
/// кэшируется — недоступный эндпоинт режет вызовы, а не создаёт шторм.
pub struct CachedQuotaProbe {
    inner: Arc<dyn QuotaProbe>,
    ttl: Duration,
    cell: Mutex<Option<(Instant, UsecaseResult<QuotaSnapshot>)>>,
}

impl CachedQuotaProbe {
    pub fn new(inner: Arc<dyn QuotaProbe>) -> Self {
        Self::with_ttl(inner, CACHE_TTL)
    }

    /// Тест-шов TTL: истечение кэша — на нём держится самовозобновление.
    pub(crate) fn with_ttl(inner: Arc<dyn QuotaProbe>, ttl: Duration) -> Self {
        Self {
            inner,
            ttl,
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
                Some((at, snap)) if at.elapsed() < self.ttl => Some(snap.clone()),
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
