//! A wall clock behind a trait so rate-limit and cache logic is testable without sleeping
//! (upstream reads `Clock.currentTimeMillis`, which `TestClock` replaces in tests).

use parking_lot::Mutex;
use std::sync::Arc;

pub trait Clock: Send + Sync + 'static {
    /// Milliseconds since the Unix epoch.
    fn now_millis(&self) -> i64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_millis(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

pub fn system_clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// A clock tests move by hand.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: Mutex<i64>,
}

impl ManualClock {
    pub fn new(now_millis: i64) -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(now_millis),
        })
    }
    pub fn set(&self, now_millis: i64) {
        *self.now.lock() = now_millis;
    }
    pub fn advance(&self, millis: i64) {
        *self.now.lock() += millis;
    }
}

impl Clock for ManualClock {
    fn now_millis(&self) -> i64 {
        *self.now.lock()
    }
}
