//! Clock-трейт: канон времени платформы — JST (Asia/Tokyo, UTC+9).
//! Пик-окна и расписания считаются по UTC (канон фабрики), даты — JST.

use chrono::{Datelike, Timelike, Utc};

/// Минуты суток UTC + день недели (1..=7, понедельник = 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcMinute {
    pub minute_of_day: u32,
    pub weekday: u32,
}

pub trait Clock: Send + Sync {
    fn utc_minute(&self) -> UtcMinute;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn utc_minute(&self) -> UtcMinute {
        let now = Utc::now();
        UtcMinute {
            minute_of_day: now.hour() * 60 + now.minute(),
            // chrono: Mon=0..Sun=6 → канон 1..=7
            weekday: now.weekday().number_from_monday(),
        }
    }
}

/// Попадание в пик-окно: будни, [start, end) минут UTC.
pub fn in_peak(now: UtcMinute, peak: &crate::config::Peak) -> bool {
    peak.enabled
        && now.weekday <= 5
        && now.minute_of_day >= peak.start_min
        && now.minute_of_day < peak.end_min
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Peak;

    fn peak() -> Peak {
        Peak {
            enabled: true,
            start_min: 360,
            end_min: 600,
        }
    }

    #[test]
    fn peak_window_matches_weekday_morning() {
        // будни 09:00 UTC — внутри
        assert!(in_peak(
            UtcMinute {
                minute_of_day: 540,
                weekday: 3
            },
            &peak()
        ));
        // до окна
        assert!(!in_peak(
            UtcMinute {
                minute_of_day: 100,
                weekday: 3
            },
            &peak()
        ));
        // после окна
        assert!(!in_peak(
            UtcMinute {
                minute_of_day: 700,
                weekday: 3
            },
            &peak()
        ));
        // выходной
        assert!(!in_peak(
            UtcMinute {
                minute_of_day: 540,
                weekday: 6
            },
            &peak()
        ));
    }

    #[test]
    fn peak_disabled_off() {
        let off = Peak {
            enabled: false,
            start_min: 0,
            end_min: 1440,
        };
        assert!(!in_peak(
            UtcMinute {
                minute_of_day: 540,
                weekday: 3
            },
            &off
        ));
    }
}
