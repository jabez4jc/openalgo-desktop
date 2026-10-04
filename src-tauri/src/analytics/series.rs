//! Time-series helpers shared by the history-driven tools: the fetch
//! window, the "last N trading dates" cap, the Strategy Builder's combined
//! premium and the intraday IV series.

use super::black76::{self as b76, Flag};
use super::py_round;
use chrono::{DateTime, Duration, NaiveDate};
use chrono_tz::Asia::Kolkata;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// A candle reduced to what the tools read: epoch seconds, close, OI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    pub time: i64,
    pub close: f64,
    pub oi: f64,
}

/// The IST calendar date of an epoch-seconds timestamp.
pub fn ist_date(time: i64) -> Option<NaiveDate> {
    DateTime::from_timestamp(time, 0).map(|t| t.with_timezone(&Kolkata).date_naive())
}

/// Web `_resolve_trading_window`: a generous calendar window ending today
/// (`max(2, days * 3 + 2)` days back) that holds at least `days` trading
/// dates in any market; callers then cap with [`cap_last_n_dates`].
pub fn trading_window(today: NaiveDate, days: i64) -> (NaiveDate, NaiveDate) {
    let back = (days.saturating_mul(3).saturating_add(2)).max(2);
    (today - Duration::days(back), today)
}

/// Widest explicit window a chart may ask for.
pub const MAX_WINDOW_DAYS: i64 = 400;

fn parse_ymd(v: &str, field: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(v, "%Y-%m-%d")
        .map_err(|_| format!("{} must be a date in YYYY-MM-DD form", field))
}

/// Web `_resolve_explicit_window`: `None` when no start date was named;
/// the end defaults to today. Errors are the web's messages.
pub fn explicit_window(
    start: Option<&str>,
    end: Option<&str>,
    today: NaiveDate,
) -> Result<Option<(NaiveDate, NaiveDate)>, String> {
    let Some(start) = start.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let s = parse_ymd(start, "start_date")?;
    let e = match end.filter(|e| !e.is_empty()) {
        Some(e) => parse_ymd(e, "end_date")?,
        None => today,
    };
    let earliest = NaiveDate::from_ymd_opt(2000, 1, 1).unwrap_or(NaiveDate::MIN);
    if s < earliest {
        return Err("start_date cannot be earlier than 2000-01-01".into());
    }
    if e < s {
        return Err("end_date cannot be earlier than start_date".into());
    }
    if (e - s).num_days() > MAX_WINDOW_DAYS {
        return Err(format!(
            "the window cannot be wider than {} days",
            MAX_WINDOW_DAYS
        ));
    }
    Ok(Some((s, e)))
}

/// Web `_cap_last_n_trading_dates`: keep the rows whose IST date is among
/// the last `n` distinct dates present. `n <= 0` keeps everything.
pub fn cap_last_n_dates<T>(series: Vec<T>, n: i64, time: impl Fn(&T) -> i64) -> Vec<T> {
    if series.is_empty() || n <= 0 {
        return series;
    }
    let dates: BTreeSet<NaiveDate> = series.iter().filter_map(|r| ist_date(time(r))).collect();
    let keep: BTreeSet<NaiveDate> = dates.into_iter().rev().take(n as usize).collect();
    series
        .into_iter()
        .filter(|r| ist_date(time(r)).is_some_and(|d| keep.contains(&d)))
        .collect()
}

/// Close by timestamp (a later duplicate wins, like a dict built in order).
pub fn closes(bars: &[Bar]) -> BTreeMap<i64, f64> {
    bars.iter().map(|b| (b.time, b.close)).collect()
}

/// Bars sorted by time (stable, so duplicates keep their order).
pub fn sorted(mut bars: Vec<Bar>) -> Vec<Bar> {
    bars.sort_by_key(|b| b.time);
    bars
}

/// One point of the combined-premium series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PremiumPoint {
    pub time: i64,
    pub net_premium: f64,
    pub combined_premium: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underlying: Option<f64>,
}

/// Web `get_strategy_chart_data` merge: `net = sum(sign * close)` with sign
/// +1 for SELL and -1 for BUY; a timestamp counts only when every leg has a
/// close there. With an underlying series the walk follows its candles;
/// without one it walks the legs' common timestamps.
pub fn combined_premium(
    underlying: Option<&[Bar]>,
    legs: &[(f64, &BTreeMap<i64, f64>)],
) -> Vec<PremiumPoint> {
    let walk: Vec<(i64, Option<f64>)> = match underlying {
        Some(u) => u.iter().map(|b| (b.time, Some(b.close))).collect(),
        None => {
            let mut common: Option<BTreeSet<i64>> = None;
            for (_, prices) in legs {
                let keys: BTreeSet<i64> = prices.keys().copied().collect();
                common = Some(match common {
                    None => keys,
                    Some(c) => c.intersection(&keys).copied().collect(),
                });
            }
            common
                .unwrap_or_default()
                .into_iter()
                .map(|t| (t, None))
                .collect()
        }
    };
    let mut out = Vec::new();
    'ts: for (t, spot) in walk {
        let mut sum = 0.0;
        for (sign, prices) in legs {
            match prices.get(&t) {
                Some(p) => sum += sign * p,
                None => continue 'ts,
            }
        }
        out.push(PremiumPoint {
            time: t,
            net_premium: py_round(sum, 2),
            combined_premium: py_round(sum.abs(), 2),
            underlying: spot.map(|s| py_round(s, 2)),
        });
    }
    out
}

/// Credit, debit or flat from the entry net premium.
pub fn credit_tag(entry_net: f64) -> &'static str {
    if entry_net > 0.0 {
        "credit"
    } else if entry_net < 0.0 {
        "debit"
    } else {
        "flat"
    }
}

/// Web `calculate_time_to_expiry_at`: years from a candle to expiry (0 once
/// at or past it, never below 0.0001 before it).
pub fn years_to_expiry_at(candle: i64, expiry: i64) -> f64 {
    if expiry <= candle {
        return 0.0;
    }
    let years = (expiry - candle) as f64 / 86_400.0 / 365.0;
    years.max(0.0001)
}

/// One point of an option's intraday IV series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct IvPoint {
    pub time: i64,
    pub iv: Option<f64>,
    pub delta: Option<f64>,
    pub gamma: Option<f64>,
    pub theta: Option<f64>,
    pub vega: Option<f64>,
    pub option_price: f64,
    pub underlying_price: f64,
}

/// Web `_calculate_iv_series`: at every timestamp both series share, the
/// Black-76 IV of the option close against the underlying close, and the
/// Greeks at that IV.
pub fn iv_series(
    option: &BTreeMap<i64, f64>,
    underlying: &BTreeMap<i64, f64>,
    strike: f64,
    expiry: i64,
    flag: Flag,
    r: f64,
) -> Vec<IvPoint> {
    option
        .iter()
        .filter_map(|(t, px)| underlying.get(t).map(|u| (*t, *px, *u)))
        .map(|(t, px, u)| {
            let years = years_to_expiry_at(t, expiry);
            let mut p = IvPoint {
                time: t,
                iv: None,
                delta: None,
                gamma: None,
                theta: None,
                vega: None,
                option_price: px,
                underlying_price: u,
            };
            if years > 0.0 && px > 0.0 && u > 0.0 {
                if let Ok(iv) = b76::implied_volatility(px, u, strike, r, years, flag) {
                    p.iv = Some(py_round(iv * 100.0, 2));
                    if iv > 0.0 {
                        let g = b76::greeks(flag, u, strike, years, r, iv);
                        p.delta = Some(py_round(g.delta, 4));
                        p.gamma = Some(py_round(g.gamma, 6));
                        p.theta = Some(py_round(g.theta, 4));
                        p.vega = Some(py_round(g.vega, 4));
                    }
                }
            }
            p
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(y: i32, m: u32, d: u32, h: u32, mi: u32) -> i64 {
        Kolkata
            .with_ymd_and_hms(y, m, d, h, mi, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Web `services/strategy_chart_service.py:59-62`: 1/3/5/10 days fetch
    /// 5/11/17/32 calendar days.
    #[test]
    fn trading_window_matches_the_web_buffer() {
        let today = day(2026, 10, 5);
        for (d, back) in [(1, 5), (3, 11), (5, 17), (10, 32), (0, 2), (-4, 2)] {
            assert_eq!(
                trading_window(today, d),
                (today - Duration::days(back), today)
            );
        }
    }

    #[test]
    fn explicit_window_rules() {
        let today = day(2026, 10, 5);
        assert_eq!(explicit_window(None, Some("2026-01-01"), today), Ok(None));
        assert_eq!(explicit_window(Some(""), None, today), Ok(None));
        assert_eq!(
            explicit_window(Some("2026-09-01"), None, today),
            Ok(Some((day(2026, 9, 1), today)))
        );
        assert_eq!(
            explicit_window(Some("2026-02-31"), None, today),
            Err("start_date must be a date in YYYY-MM-DD form".into())
        );
        assert_eq!(
            explicit_window(Some("2026-09-01"), Some("x"), today),
            Err("end_date must be a date in YYYY-MM-DD form".into())
        );
        assert_eq!(
            explicit_window(Some("1999-12-31"), Some("2000-01-02"), today),
            Err("start_date cannot be earlier than 2000-01-01".into())
        );
        assert_eq!(
            explicit_window(Some("2026-09-02"), Some("2026-09-01"), today),
            Err("end_date cannot be earlier than start_date".into())
        );
        assert_eq!(
            explicit_window(Some("2024-01-01"), Some("2026-01-01"), today),
            Err("the window cannot be wider than 400 days".into())
        );
    }

    #[test]
    fn cap_keeps_the_last_n_dates_with_data() {
        // Friday, then the next Monday and Tuesday; a 01:00 IST bar belongs
        // to its IST date even though it is the previous UTC day.
        let rows = vec![
            ts(2026, 10, 2, 15, 0),
            ts(2026, 10, 5, 1, 0),
            ts(2026, 10, 5, 10, 0),
            ts(2026, 10, 6, 10, 0),
        ];
        let kept = cap_last_n_dates(rows.clone(), 2, |t| *t);
        assert_eq!(kept, rows[1..].to_vec());
        assert_eq!(cap_last_n_dates(rows.clone(), 0, |t| *t), rows);
        assert_eq!(cap_last_n_dates(rows.clone(), 9, |t| *t), rows);
    }

    #[test]
    fn combined_premium_walks_underlying_and_drops_gaps() {
        let u = [
            Bar {
                time: 1,
                close: 100.0,
                oi: 0.0,
            },
            Bar {
                time: 2,
                close: 101.234,
                oi: 0.0,
            },
            Bar {
                time: 3,
                close: 102.0,
                oi: 0.0,
            },
        ];
        let ce: BTreeMap<i64, f64> = [(1, 50.0), (2, 45.5), (3, 40.0)].into();
        let pe: BTreeMap<i64, f64> = [(1, 30.0), (2, 35.25)].into();
        // Short straddle (both SELL): +ce +pe.
        let s = combined_premium(Some(&u), &[(1.0, &ce), (1.0, &pe)]);
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].net_premium, 80.75);
        assert_eq!(s[1].underlying, Some(101.23));
        // Bull call spread: buy (-1) ce, sell (+1) pe -> net -20 at t=1.
        let s = combined_premium(None, &[(-1.0, &ce), (1.0, &pe)]);
        assert_eq!(s.iter().map(|p| p.time).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(s[0].net_premium, -20.0);
        assert_eq!(s[0].combined_premium, 20.0);
        assert_eq!(s[0].underlying, None);
        assert_eq!(credit_tag(5.0), "credit");
        assert_eq!(credit_tag(-0.5), "debit");
        assert_eq!(credit_tag(0.0), "flat");
    }

    /// Web `services/iv_chart_service.py:524-560`, against the reference
    /// case pinned in `black76` (IV 17.98, delta 0.5272 at 3 days).
    #[test]
    fn iv_series_matches_the_black76_reference() {
        let expiry = ts(2026, 10, 8, 15, 30);
        let candle = expiry - 3 * 86_400;
        let opt: BTreeMap<i64, f64> = [(candle, 156.95), (candle + 60, 0.0)].into();
        let und: BTreeMap<i64, f64> = [(candle, 22421.95), (candle + 60, 22421.95)].into();
        let s = iv_series(&opt, &und, 22400.0, expiry, Flag::Call, 0.0);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].iv, Some(17.98));
        assert_eq!(s[0].delta, Some(0.5272));
        assert_eq!(s[0].gamma, Some(0.001089));
        assert_eq!(s[0].theta, Some(-24.241));
        assert_eq!(s[0].vega, Some(8.0907));
        // A zero close has no IV.
        assert_eq!(s[1].iv, None);
        // Past expiry: no IV.
        let s = iv_series(&opt, &und, 22400.0, candle, Flag::Call, 0.0);
        assert_eq!(s[0].iv, None);
        assert_eq!(years_to_expiry_at(0, 10), 0.0001);
    }
}
