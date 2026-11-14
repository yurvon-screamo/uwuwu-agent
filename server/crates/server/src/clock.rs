//! Системные часы: реализация порта Clock (UTC-минута + эпоха).

use chrono::{Datelike, Timelike, Utc};
use uwuwu_domain::UtcMinute;
use uwuwu_usecase::Clock;

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

    fn now_ms(&self) -> u64 {
        Utc::now().timestamp_millis() as u64
    }
}
