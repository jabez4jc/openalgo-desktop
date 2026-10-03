//! Upstox request pacing (web `api/rate_limiter.py`).
//!
//! Upstox publishes three simultaneous rolling windows per category (per
//! second, per minute, per 30 minutes). The shared `common::ratelimit::Pacer`
//! only spaces requests, which would let the standard category run at its
//! per-second rate for a whole minute and blow the per-minute budget, so this
//! is a port of the web's `SlidingWindowLimiter.reserve`: every caller books
//! a send slot that satisfies all three windows, then sleeps until it,
//! outside the lock. State is the list of slots booked in the last 30
//! minutes, so it is bounded by the 30-minute cap plus callers in flight.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::time::Duration;
use tokio::time::Instant;

const HORIZON: Duration = Duration::from_secs(1800);

/// One rolling-window limiter.
#[derive(Debug)]
pub struct WindowLimiter {
    /// `(cap, span)` per window.
    windows: [(usize, Duration); 3],
    reserved: Mutex<VecDeque<Instant>>,
}

impl WindowLimiter {
    pub fn new(per_second: usize, per_minute: usize, per_30min: usize) -> Self {
        Self {
            windows: [
                (per_second.max(1), Duration::from_secs(1)),
                (per_minute.max(1), Duration::from_secs(60)),
                (per_30min.max(1), HORIZON),
            ],
            reserved: Mutex::new(VecDeque::new()),
        }
    }

    /// Web `ORDER_LIMITER`: 8/s, 475/min, 1900/30min (published 10/500/2000).
    pub fn order() -> Self {
        Self::new(8, 475, 1900)
    }

    /// Web `STANDARD_LIMITER`: 45/s, 475/min, 1900/30min (published 50/500/2000).
    pub fn standard() -> Self {
        Self::new(45, 475, 1900)
    }

    /// Book the next slot that keeps every window within its cap.
    fn reserve(&self) -> Instant {
        let now = Instant::now();
        let mut reserved = self.reserved.lock();
        while let Some(front) = reserved.front() {
            if now.duration_since(*front) >= HORIZON && *front <= now {
                reserved.pop_front();
            } else {
                break;
            }
        }
        let mut slot = now;
        for (cap, span) in self.windows {
            if reserved.len() >= cap {
                if let Some(at) = reserved.get(reserved.len() - cap) {
                    slot = slot.max(*at + span);
                }
            }
        }
        reserved.push_back(slot);
        slot
    }

    /// Wait for a send slot.
    pub async fn acquire(&self) {
        let slot = self.reserve();
        tokio::time::sleep_until(slot).await;
    }

    #[cfg(test)]
    pub fn booked(&self) -> usize {
        self.reserved.lock().len()
    }
}

/// Reactive retry budget for read endpoints (web `MAX_RETRIES`).
pub const MAX_RETRIES: u32 = 3;
/// Web `BASE_BACKOFF`: 1, 2, 4 s.
pub const BASE_BACKOFF: Duration = Duration::from_secs(1);
/// Upstox's rate-limit error code.
pub const RATE_LIMIT_CODE: &str = "UDAPI10005";

/// Delay before retry `attempt` (0-based): `Retry-After` when Upstox sends
/// it (floored at 50 ms), else 1, 2, 4 s.
pub fn retry_delay(retry_after: Option<&str>, attempt: u32) -> Duration {
    if let Some(secs) = retry_after.and_then(|v| v.trim().parse::<f64>().ok()) {
        if secs.is_finite() && secs >= 0.0 {
            return Duration::from_secs_f64(secs.clamp(0.05, 60.0));
        }
    }
    BASE_BACKOFF.saturating_mul(1u32 << attempt.min(6))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn per_second_window_spaces_a_burst() {
        let l = WindowLimiter::new(2, 100, 1000);
        let start = Instant::now();
        for _ in 0..5 {
            l.acquire().await;
        }
        // Slots: 0, 0, 1, 1, 2 seconds.
        assert_eq!(start.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn per_minute_window_binds_after_the_second_window() {
        let l = WindowLimiter::new(100, 3, 1000);
        let start = Instant::now();
        for _ in 0..4 {
            l.acquire().await;
        }
        assert_eq!(start.elapsed(), Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn old_slots_are_purged() {
        let l = WindowLimiter::new(10, 100, 1000);
        for _ in 0..5 {
            l.acquire().await;
        }
        tokio::time::sleep(HORIZON + Duration::from_secs(1)).await;
        l.acquire().await;
        assert_eq!(l.booked(), 1);
    }

    #[test]
    fn retry_delays() {
        assert_eq!(retry_delay(None, 0), Duration::from_secs(1));
        assert_eq!(retry_delay(None, 2), Duration::from_secs(4));
        assert_eq!(retry_delay(Some("0"), 0), Duration::from_millis(50));
        assert_eq!(retry_delay(Some("2.5"), 1), Duration::from_millis(2500));
        assert_eq!(retry_delay(Some("soon"), 1), Duration::from_secs(2));
    }
}
