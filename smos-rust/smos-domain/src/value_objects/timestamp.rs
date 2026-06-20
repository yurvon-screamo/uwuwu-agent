//! `Timestamp` — UTC instant wrapper around `time::OffsetDateTime`.

use crate::error::DomainError;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// UTC timestamp used everywhere in the domain.
///
/// Wrapping `OffsetDateTime` keeps the public surface small (we only ever need
/// "now", unix conversions, and ordering) and lets us swap the underlying crate
/// later without touching call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(OffsetDateTime);

impl Timestamp {
    pub fn now_utc() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    pub fn from_unix_secs(secs: i64) -> Result<Self, DomainError> {
        match OffsetDateTime::from_unix_timestamp(secs) {
            Ok(odt) => Ok(Self(odt)),
            Err(_) => Err(DomainError::InvalidTimestamp(format!(
                "unix_secs out of range: {secs}"
            ))),
        }
    }

    pub fn from_unix_millis(ms: i64) -> Result<Self, DomainError> {
        let secs = ms.div_euclid(1000);
        let nanos = (ms.rem_euclid(1000)) as u32 * 1_000_000;
        match OffsetDateTime::from_unix_timestamp_nanos(
            (secs as i128) * 1_000_000_000 + nanos as i128,
        ) {
            Ok(odt) => Ok(Self(odt)),
            Err(_) => Err(DomainError::InvalidTimestamp(format!(
                "unix_millis out of range: {ms}"
            ))),
        }
    }

    pub fn as_unix_secs(&self) -> i64 {
        self.0.unix_timestamp()
    }

    pub fn as_unix_millis(&self) -> i64 {
        self.0.unix_timestamp_nanos() as i64 / 1_000_000
    }

    pub fn as_offset_date_time(&self) -> OffsetDateTime {
        self.0
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_utc_returns_reasonable_year() {
        let ts = Timestamp::now_utc();
        assert!(ts.as_offset_date_time().year() >= 2026);
    }

    #[test]
    fn from_unix_secs_roundtrips() {
        let ts = Timestamp::from_unix_secs(1_700_000_000).unwrap();
        assert_eq!(ts.as_unix_secs(), 1_700_000_000);
    }

    #[test]
    fn from_unix_millis_roundtrips() {
        let ts = Timestamp::from_unix_millis(1_700_000_012).unwrap();
        assert_eq!(ts.as_unix_millis(), 1_700_000_012);
    }

    #[test]
    fn from_unix_secs_and_millis_agree() {
        let secs = 1_234_567_890i64;
        let from_s = Timestamp::from_unix_secs(secs).unwrap();
        let from_ms = Timestamp::from_unix_millis(secs * 1000).unwrap();
        assert_eq!(from_s.as_unix_secs(), from_ms.as_unix_secs());
    }

    #[test]
    fn ordering_works() {
        let earlier = Timestamp::from_unix_secs(1000).unwrap();
        let later = Timestamp::from_unix_secs(2000).unwrap();
        assert!(earlier < later);
    }

    #[test]
    fn serde_roundtrip_preserves_value() {
        let ts = Timestamp::from_unix_secs(1_700_000_000).unwrap();
        let json = serde_json::to_string(&ts).unwrap();
        let back: Timestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(ts, back);
    }
}
