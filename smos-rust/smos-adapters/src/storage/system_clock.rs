//! `SystemClock` — production wall-clock backed by the `time` crate.

use smos_application::ports::Clock;
use smos_domain::Timestamp;

/// Wall-clock that reads the system UTC time. Inject a fake in tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now_utc()
    }
}
