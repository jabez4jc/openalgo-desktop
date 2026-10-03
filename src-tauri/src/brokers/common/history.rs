//! History helpers: request chunking and candle normalisation.
//!
//! Brokers cap the date range of one history call (Kite: 60 days intraday,
//! 2000 days daily). `chunks` splits an inclusive `[start, end]` range into
//! inclusive sub-ranges of at most `max_days`, exactly like the web's
//! `current_end = min(current_start + chunk_days - 1, end)` loop.

use crate::brokers::types::Candle;
use chrono::NaiveDate;

/// Inclusive date ranges of at most `max_days` days covering `[start, end]`.
pub fn chunks(start: NaiveDate, end: NaiveDate, max_days: i64) -> Vec<(NaiveDate, NaiveDate)> {
    let step = max_days.max(1);
    let mut out = Vec::new();
    let mut cur = start;
    while cur <= end {
        let chunk_end = (cur + chrono::Duration::days(step - 1)).min(end);
        out.push((cur, chunk_end));
        match chunk_end.succ_opt() {
            Some(next) => cur = next,
            None => break,
        }
    }
    out
}

/// Seconds to add to a UTC-midnight daily candle so it lands on the IST
/// session date (the web's `+ pd.Timedelta(hours=5, minutes=30)` for `D`).
pub const IST_OFFSET_SECS: i64 = 5 * 3600 + 30 * 60;

/// Sort by timestamp and drop duplicate timestamps (first wins), as the
/// web's `sort_values("timestamp").drop_duplicates(subset=["timestamp"])`.
pub fn sort_dedupe(mut candles: Vec<Candle>) -> Vec<Candle> {
    candles.sort_by_key(|c| c.timestamp);
    candles.dedup_by_key(|c| c.timestamp);
    candles
}

/// Parse an ISO-8601 timestamp with offset (`2024-03-28T09:15:00+0530` or
/// `+05:30`) to epoch seconds.
pub fn parse_iso_epoch(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z")
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(s))
        .ok()
        .map(|d| d.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn chunks_cover_range_without_overlap() {
        let c = chunks(d(2024, 1, 1), d(2024, 3, 31), 60);
        assert_eq!(
            c,
            vec![
                (d(2024, 1, 1), d(2024, 2, 29)),
                (d(2024, 3, 1), d(2024, 3, 31))
            ]
        );
        assert_eq!(chunks(d(2024, 1, 1), d(2024, 1, 1), 60).len(), 1);
        assert!(chunks(d(2024, 2, 1), d(2024, 1, 1), 60).is_empty());
        // 2000-day daily chunks: 10 years -> 2 calls.
        assert_eq!(chunks(d(2015, 1, 1), d(2024, 12, 31), 2000).len(), 2);
    }

    #[test]
    fn chunk_invariants_hold_for_many_ranges() {
        for len in 0..400 {
            for max in [1, 7, 25, 60, 300] {
                let s = d(2023, 6, 1);
                let e = s + chrono::Duration::days(len);
                let c = chunks(s, e, max);
                assert_eq!(c.first().unwrap().0, s);
                assert_eq!(c.last().unwrap().1, e);
                for w in c.windows(2) {
                    assert_eq!(w[0].1.succ_opt().unwrap(), w[1].0);
                }
                for (a, b) in &c {
                    assert!((*b - *a).num_days() < max);
                }
            }
        }
    }

    #[test]
    fn sorts_and_dedupes() {
        let c = |t| Candle {
            timestamp: t,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: t as f64,
            volume: 0,
            oi: 0,
        };
        let out = sort_dedupe(vec![c(3), c(1), c(3), c(2)]);
        let ts: Vec<i64> = out.iter().map(|c| c.timestamp).collect();
        assert_eq!(ts, [1, 2, 3]);
    }

    #[test]
    fn iso_timestamps() {
        assert_eq!(
            parse_iso_epoch("2024-03-28T09:15:00+0530"),
            Some(1711597500)
        );
        assert_eq!(
            parse_iso_epoch("2024-03-28T09:15:00+05:30"),
            Some(1711597500)
        );
        assert_eq!(parse_iso_epoch("nope"), None);
    }
}
