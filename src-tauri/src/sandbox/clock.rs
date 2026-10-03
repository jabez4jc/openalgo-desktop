//! Time for the sandbox: the crate's injected [`Clock`] read in IST.
//!
//! Every sandbox timestamp is naive IST wall-clock text, so session boundaries
//! and square-off times compare without the web's UTC column quirk.

pub use crate::clock::{Clock, ManualClock, SystemClock};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use chrono_tz::Tz;

/// The exchange timezone.
pub const IST: Tz = Kolkata;

/// Stored format for order, trade, position and funds timestamps.
pub const TS_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.6f";
/// Format the web returns for order and trade timestamps.
pub const TS_SECONDS: &str = "%Y-%m-%d %H:%M:%S";
/// GTT timestamps, Python `isoformat()` of a naive datetime with microseconds.
pub const GTT_TS_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.6f";

/// Current IST wall-clock time.
pub fn now_ist(clock: &dyn Clock) -> NaiveDateTime {
    clock.now().with_timezone(&IST).naive_local()
}

/// Aware IST time.
pub fn now_ist_aware(clock: &dyn Clock) -> DateTime<Tz> {
    clock.now().with_timezone(&IST)
}

/// Stored text of an IST timestamp.
pub fn ts(dt: NaiveDateTime) -> String {
    dt.format(TS_FORMAT).to_string()
}

/// Stored text of a GTT timestamp.
pub fn gtt_ts(dt: NaiveDateTime) -> String {
    dt.format(GTT_TS_FORMAT).to_string()
}

/// Parse a stored timestamp (either sandbox format, with or without
/// fractional seconds, `T` or space separated).
pub fn parse_ts(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt);
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
}

/// A stored timestamp as the web prints it (`YYYY-MM-DD HH:MM:SS`).
pub fn display_seconds(s: &str) -> String {
    parse_ts(s)
        .map(|dt| dt.format(TS_SECONDS).to_string())
        .unwrap_or_default()
}

/// The UTC instant of an IST wall-clock time (ambiguity cannot occur: IST
/// has no DST).
pub fn ist_to_utc(dt: NaiveDateTime) -> DateTime<Utc> {
    match IST.from_local_datetime(&dt) {
        chrono::LocalResult::Single(t) => t.with_timezone(&Utc),
        chrono::LocalResult::Ambiguous(t, _) => t.with_timezone(&Utc),
        chrono::LocalResult::None => Utc.from_utc_datetime(&dt),
    }
}

/// Set a [`ManualClock`] to an IST wall-clock time given as
/// `YYYY-MM-DD HH:MM:SS`. Panics on a malformed literal (test helper).
pub fn set_ist(clock: &ManualClock, ist: &str) {
    let dt = NaiveDateTime::parse_from_str(ist, TS_SECONDS)
        .unwrap_or_else(|_| panic!("bad IST literal {ist}"));
    clock.set(ist_to_utc(dt));
}

/// A [`ManualClock`] at an IST wall-clock time (test helper).
pub fn manual_clock_at(ist: &str) -> std::sync::Arc<ManualClock> {
    let clock = ManualClock::new(Utc::now());
    set_ist(&clock, ist);
    clock
}

/// Parse `HH:MM` (both parts in range), as the web's config validation does.
pub fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let (h, m) = s.trim().split_once(':')?;
    if h.is_empty() || m.is_empty() || m.contains(':') {
        return None;
    }
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    NaiveTime::from_hms_opt(h, m, 0)
}
