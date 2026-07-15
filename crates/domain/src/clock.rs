use chrono::{DateTime, TimeDelta, Utc};
use std::sync::{Arc, RwLock};

pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Debug, Clone)]
pub struct FakeClock {
    now: Arc<RwLock<DateTime<Utc>>>,
}

impl FakeClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(RwLock::new(now)),
        }
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.write().expect("fake clock lock poisoned") = now;
    }

    pub fn advance(&self, delta: TimeDelta) {
        let mut now = self.now.write().expect("fake clock lock poisoned");
        *now += delta;
    }
}

impl Clock for FakeClock {
    fn now_utc(&self) -> DateTime<Utc> {
        *self.now.read().expect("fake clock lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn fake_clock_can_move_without_waiting() {
        let clock = FakeClock::new(Utc.with_ymd_and_hms(2026, 7, 14, 2, 0, 0).unwrap());

        clock.advance(TimeDelta::minutes(30));

        assert_eq!(
            clock.now_utc(),
            Utc.with_ymd_and_hms(2026, 7, 14, 2, 30, 0).unwrap()
        );
    }
}
