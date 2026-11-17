//! Классификация GitHub HTTP-отказов: rate limit vs permission —
//! громкий текст с reset-временем вместо голого «HTTP 403» (#1156).
//! Врезается в транспорты github/ и wiki/ до их сырых сообщений:
//! классифицированный 403/429 → Err, остальные статусы — None
//! (404/422-толерантные ветки коллбэков не затронуты).

use reqwest::header::HeaderMap;
use reqwest::StatusCode;
use std::fmt;

/// Класс отказа GitHub API (по заголовкам квоты/ретрая).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhFail {
    /// Core-квота исчерпана (`X-RateLimit-Remaining: 0`).
    Core {
        /// Epoch-секунды сброса окна (`X-RateLimit-Reset`), если отдал.
        reset_epoch: Option<i64>,
    },
    /// Secondary rate limit — abuse-детект (`Retry-After`).
    Secondary {
        /// Секунды до повтора (`Retry-After`).
        retry_after_secs: u64,
    },
    /// Настоящий отказ доступа: 403 без rate-limit признаков.
    Permission,
}

impl fmt::Display for GhFail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core { reset_epoch } => {
                write!(f, "GitHub rate limit (core): квота исчерпана")?;
                if let Some(epoch) = reset_epoch {
                    let now = chrono::Utc::now();
                    write!(f, ", reset {}", format_reset(*epoch, now))?;
                }
                write!(f, " — подожди или отложи ход")
            }
            Self::Secondary { retry_after_secs } => write!(
                f,
                "GitHub rate limit (secondary): Retry-After {retry_after_secs}s — бэкофф, не бей повторами"
            ),
            Self::Permission => write!(
                f,
                "HTTP 403 (не rate limit): проверь GH_TOKEN/права токена"
            ),
        }
    }
}

/// Классификация REST-отказа по статусу и заголовкам ответа.
/// Порядок веток — спецификация приоритета: сначала core
/// (`remaining: 0` — независимо от наличия `Retry-After`), затем
/// secondary, затем permission. Прочие статусы → None.
pub fn classify(status: StatusCode, headers: &HeaderMap) -> Option<GhFail> {
    let code = status.as_u16();
    if code != 403 && code != 429 {
        return None;
    }
    let header_num = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
    };
    if header_num("x-ratelimit-remaining").and_then(|v| v.parse::<u64>().ok()) == Some(0) {
        let reset_epoch = header_num("x-ratelimit-reset").and_then(|v| v.parse::<i64>().ok());
        return Some(GhFail::Core { reset_epoch });
    }
    if let Some(retry_after_secs) = header_num("retry-after").and_then(|v| v.parse::<u64>().ok()) {
        return Some(GhFail::Secondary { retry_after_secs });
    }
    if code == 403 {
        return Some(GhFail::Permission);
    }
    None
}

/// Классификация GraphQL-лимита внутри 200-ответа. Два живых формата
/// GitHub (#1243): легаси `type:"RATE_LIMITED"` и новый `type:"RATE_LIMIT",
/// code:"graphql_rate_limit"` — общий знаменатель type-подстроки
/// «RATE_LIMIT» покрывает оба; fallback — message про rate limit.
/// Заголовков квоты в теле нет — reset неизвестен.
pub fn classify_gql_errors(body: &serde_json::Value) -> Option<GhFail> {
    let errors = body.get("errors")?.as_array()?;
    let limited = errors.iter().any(|error| {
        let is_limited_type = error
            .get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.to_uppercase().contains("RATE_LIMIT"));
        let is_limited_message = error
            .get("message")
            .and_then(|m| m.as_str())
            .is_some_and(|m| m.to_ascii_lowercase().contains("rate limit"));
        is_limited_type || is_limited_message
    });
    limited.then_some(GhFail::Core { reset_epoch: None })
}

/// Reset-время для текста ошибки: абсолют UTC + относительное «через ~N мин».
pub fn format_reset(reset_epoch: i64, now: chrono::DateTime<chrono::Utc>) -> String {
    let reset = chrono::DateTime::from_timestamp(reset_epoch, 0);
    let Some(reset) = reset else {
        return format!("epoch {reset_epoch}");
    };
    let wait_min = (reset - now).num_minutes().max(0);
    format!(
        "{} UTC (через ~{wait_min} мин)",
        reset.format("%Y-%m-%d %H:%M")
    )
}
