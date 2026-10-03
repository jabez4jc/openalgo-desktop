//! The trading session boundary (web `sandbox/session_boundary.py`).
//!
//! The sandbox day runs from one session expiry (default 03:00 IST) to the
//! next: the order book and trade book show the current session, today's
//! realized P&L resets at the boundary, and an MIS position untouched since
//! the last boundary is a leftover that catch-up settles. All timestamps are
//! IST, so unlike the web there is no UTC conversion.

use super::clock::parse_hhmm;
use chrono::{Duration, NaiveDateTime, NaiveTime};

/// The default boundary, 03:00 IST.
pub fn default_session_expiry() -> NaiveTime {
    NaiveTime::from_hms_opt(3, 0, 0).unwrap_or(NaiveTime::MIN)
}

/// Parse a configured boundary. Anything malformed or out of range falls
/// back to 03:00 rather than failing (web: a bad `SESSION_EXPIRY_TIME`
/// silently skipped the MIS catch-up).
pub fn parse_session_expiry(s: &str) -> NaiveTime {
    parse_hhmm(s).unwrap_or_else(default_session_expiry)
}

/// The most recent boundary at or before `now` (IST wall clock).
pub fn last_session_expiry(now: NaiveDateTime, expiry: NaiveTime) -> NaiveDateTime {
    let today = now.date().and_time(expiry);
    if now >= today {
        today
    } else {
        today - Duration::days(1)
    }
}

/// Start of the current session (same instant as [`last_session_expiry`]).
pub fn session_start(now: NaiveDateTime, expiry: NaiveTime) -> NaiveDateTime {
    last_session_expiry(now, expiry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 8, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    // Ports of test/sandbox/test_catch_up_session_boundary.py (IST instead
    // of UTC: the desktop stores IST, so the boundary is the IST wall clock).
    #[test]
    fn test_last_session_expiry_utc_after_boundary_ist() {
        assert_eq!(
            last_session_expiry(at(18, 10, 0), default_session_expiry()),
            at(18, 3, 0)
        );
    }

    #[test]
    fn test_last_session_expiry_utc_before_boundary_ist() {
        assert_eq!(
            last_session_expiry(at(18, 1, 0), default_session_expiry()),
            at(17, 3, 0)
        );
    }

    #[test]
    fn test_malformed_session_expiry_falls_back_to_default() {
        for bad in ["", "abc", "3", "03:00:00", "25:00", "-1:00", "03:99"] {
            assert_eq!(
                parse_session_expiry(bad),
                default_session_expiry(),
                "'{bad}' must fall back to 03:00"
            );
        }
    }

    #[test]
    fn test_valid_session_expiry_is_not_swallowed_by_the_fallback() {
        let t = parse_session_expiry("09:15");
        assert_eq!(last_session_expiry(at(18, 10, 0), t), at(18, 9, 15));
    }

    #[test]
    fn exactly_at_the_boundary_starts_the_new_session() {
        assert_eq!(
            last_session_expiry(at(18, 3, 0), default_session_expiry()),
            at(18, 3, 0)
        );
    }
}
