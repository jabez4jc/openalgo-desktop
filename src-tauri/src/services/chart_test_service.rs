//! Candles for the standalone chart test page (web
//! `blueprints/chart_test.py`): the latest N trading days of 1m, 5m or 15m
//! bars, times shifted so a chart rendering UTC shows IST wall-clock.

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const IST_OFFSET_SECONDS: i64 = 19_800;

/// Supported interval -> trading days kept (anything else is 1m).
pub fn interval_days(interval: &str) -> (&'static str, usize) {
    match interval {
        "5m" => ("5m", 3),
        "15m" => ("15m", 9),
        _ => ("1m", 1),
    }
}

/// `YYYY-MM-DD` exactly.
pub fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// The fetch window and days to keep: one given date, or a generous
/// calendar window ending today.
pub fn window(today: NaiveDate, date: &str, keep: usize) -> (String, String, usize) {
    if is_date(date) {
        return (date.to_string(), date.to_string(), 1);
    }
    let start = today - Duration::days(keep as i64 * 2 + 5);
    (
        start.format("%Y-%m-%d").to_string(),
        today.format("%Y-%m-%d").to_string(),
        keep,
    )
}

fn f(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Null => Some(0.0),
        _ => None,
    }
}

/// The success body from the history rows.
pub fn candles(symbol: &str, exchange: &str, interval: &str, rows: &[Value], keep: usize) -> Value {
    let mut by_date: BTreeMap<String, Vec<(i64, &Value)>> = BTreeMap::new();
    for r in rows {
        let Some(ts) =
            f(r.get("timestamp")).filter(|_| r.get("timestamp").is_some_and(|t| !t.is_null()))
        else {
            continue;
        };
        let ts = ts.trunc() as i64;
        let Some(d) = Utc.timestamp_opt(ts, 0).single() else {
            continue;
        };
        let day = d.with_timezone(&Kolkata).format("%Y-%m-%d").to_string();
        by_date.entry(day).or_default().push((ts, r));
    }
    let empty = || {
        json!({"status": "success", "symbol": symbol, "exchange": exchange,
        "interval": interval, "date": null, "candles": []})
    };
    if by_date.is_empty() {
        return empty();
    }
    let dates: Vec<&String> = by_date.keys().collect();
    let selected: Vec<String> = dates[dates.len().saturating_sub(keep)..]
        .iter()
        .map(|s| (*s).clone())
        .collect();
    let mut day_rows: Vec<(i64, &Value)> = selected
        .iter()
        .flat_map(|d| by_date.get(d).cloned().unwrap_or_default())
        .collect();
    day_rows.sort_by_key(|x| x.0);
    let out: Vec<Value> = day_rows
        .iter()
        .filter_map(|(ts, r)| {
            Some(json!({
                "time": (ts + IST_OFFSET_SECONDS).div_euclid(60) * 60,
                "open": f(r.get("open"))?,
                "high": f(r.get("high"))?,
                "low": f(r.get("low"))?,
                "close": f(r.get("close"))?,
                "volume": f(r.get("volume")).unwrap_or(0.0),
            }))
        })
        .collect();
    json!({"status": "success", "symbol": symbol, "exchange": exchange, "interval": interval,
        "date": selected.last(), "candles": out})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_latest_days_and_shifts_to_ist() {
        let day = |d: u32, h: u32| {
            Kolkata
                .with_ymd_and_hms(2026, 10, d, h, 15, 0)
                .unwrap()
                .timestamp()
        };
        let rows = vec![
            json!({"timestamp": day(1, 9), "open": 1, "high": 2, "low": 0.5, "close": 1.5, "volume": 10}),
            json!({"timestamp": day(5, 9), "open": 1, "high": 2, "low": 0.5, "close": 1.5, "volume": null}),
        ];
        let v = candles("SBIN", "NSE", "1m", &rows, 1);
        assert_eq!(v["date"], "2026-10-05");
        assert_eq!(v["candles"].as_array().unwrap().len(), 1);
        assert_eq!(v["candles"][0]["time"], day(5, 9) + IST_OFFSET_SECONDS);
        assert_eq!(v["candles"][0]["volume"], 0.0);
        assert_eq!(v["candles"][0]["open"], 1.0);
        assert!(is_date("2026-10-05") && !is_date("2026-1-05"));
        let today = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        assert_eq!(window(today, "", 3).0, "2026-09-24");
        assert_eq!(interval_days("30m"), ("1m", 1));
        assert_eq!(candles("S", "E", "1m", &[], 1)["date"], Value::Null);
    }
}
