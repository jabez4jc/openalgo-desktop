//! Injected clock. Session expiry, OAuth state expiry and anything else that
//! depends on wall time reads it from here so tests are deterministic.

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use std::sync::Arc;

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Settable clock for tests.
#[derive(Debug)]
pub struct ManualClock {
    now: RwLock<DateTime<Utc>>,
}

impl ManualClock {
    pub fn new(now: DateTime<Utc>) -> Arc<Self> {
        Arc::new(Self {
            now: RwLock::new(now),
        })
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.write() = now;
    }

    pub fn advance(&self, by: chrono::Duration) {
        let mut n = self.now.write();
        *n += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.read()
    }
}
