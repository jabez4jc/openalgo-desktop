//! The daily broker-session boundary (03:00 IST by default).
//!
//! A broker token is fresh while it was issued at or after the most recent
//! boundary. Unlike the web's check (which only compares against *today's*
//! boundary and so keeps a token issued at 02:00 alive until 03:00 the next
//! day when checked between midnight and 03:00), this uses the most recent
//! boundary that has passed, which is when the broker actually expires it.

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;

/// The most recent boundary at or before `now`.
pub fn last_boundary(now: DateTime<Utc>, hour: u32, minute: u32) -> DateTime<Utc> {
    let ist = now.with_timezone(&Kolkata);
    let today = Kolkata
        .with_ymd_and_hms(
            ist.year(),
            ist.month(),
            ist.day(),
            hour.min(23),
            minute.min(59),
            0,
        )
        .single()
        .map(|d| d.with_timezone(&Utc))
        // IST has no DST, so the local time always exists; keep a safe value anyway.
        .unwrap_or(now);
    if today <= now {
        today
    } else {
        today - Duration::days(1)
    }
}

/// The next boundary strictly after `now`.
pub fn next_boundary(now: DateTime<Utc>, hour: u32, minute: u32) -> DateTime<Utc> {
    last_boundary(now, hour, minute) + Duration::days(1)
}

/// Whether something authenticated at `authenticated_at` is still valid.
pub fn is_fresh(
    authenticated_at: DateTime<Utc>,
    now: DateTime<Utc>,
    hour: u32,
    minute: u32,
) -> bool {
    authenticated_at >= last_boundary(now, hour, minute)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ist(y: i32, m: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Kolkata
            .with_ymd_and_hms(y, m, d, h, mi, 0)
            .single()
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn boundary_before_and_after_three() {
        assert_eq!(
            last_boundary(ist(2026, 10, 3, 10, 0), 3, 0),
            ist(2026, 10, 3, 3, 0)
        );
        assert_eq!(
            last_boundary(ist(2026, 10, 3, 2, 59), 3, 0),
            ist(2026, 10, 2, 3, 0)
        );
        assert_eq!(
            last_boundary(ist(2026, 10, 3, 3, 0), 3, 0),
            ist(2026, 10, 3, 3, 0)
        );
        assert_eq!(
            next_boundary(ist(2026, 10, 3, 10, 0), 3, 0),
            ist(2026, 10, 4, 3, 0)
        );
    }

    #[test]
    fn freshness_across_the_boundary() {
        let login = ist(2026, 10, 3, 9, 15);
        assert!(is_fresh(login, ist(2026, 10, 3, 23, 59), 3, 0));
        assert!(is_fresh(login, ist(2026, 10, 4, 2, 59), 3, 0));
        assert!(!is_fresh(login, ist(2026, 10, 4, 3, 0), 3, 0));
        // Issued between midnight and 03:00: expires at 03:00 the same morning.
        let late = ist(2026, 10, 4, 2, 0);
        assert!(is_fresh(late, ist(2026, 10, 4, 2, 30), 3, 0));
        assert!(!is_fresh(late, ist(2026, 10, 4, 3, 1), 3, 0));
    }

    #[test]
    fn configurable_time() {
        let login = ist(2026, 10, 3, 9, 0);
        assert!(is_fresh(login, ist(2026, 10, 4, 5, 29), 5, 30));
        assert!(!is_fresh(login, ist(2026, 10, 4, 5, 30), 5, 30));
    }
}
