//! Intraday P&L tracker (web `blueprints/pnltracker.py`
//! `/pnltracker/api/pnl`): the portfolio's mark-to-market through the day,
//! rebuilt from today's trades, open and carried-forward positions and
//! 1-minute candles, with the peak, trough and drawdown series.
//!
//! The web builds this with pandas; this is the same algorithm over plain
//! vectors, quirks included (a BUY against a short opens a new window, the
//! realized P&L of a closed window replaces the column after its exit).

use super::core::round2;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::{Asia::Kolkata, Tz};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;

type Ist = DateTime<Tz>;

/// Web `parse_trade_timestamp`: the broker formats it knows, a time of day
/// (today, IST), epoch seconds, or ISO 8601.
pub fn parse_trade_timestamp(v: &Value, today: NaiveDate) -> Option<Ist> {
    match v {
        Value::Number(n) => {
            let secs = n.as_f64()?;
            return Utc
                .timestamp_opt(secs.trunc() as i64, 0)
                .single()
                .map(|t| t.with_timezone(&Kolkata));
        }
        Value::String(_) => {}
        _ => return None,
    }
    let s = v.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    let local = |n: NaiveDateTime| Kolkata.from_local_datetime(&n).single();
    for fmt in [
        "%d-%b-%Y %H:%M:%S",
        "%H:%M:%S %d-%m-%Y",
        "%d-%m-%Y %H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
            return local(n);
        }
    }
    if s.contains(':') && !s.contains(' ') {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() >= 2 && parts[0].len() <= 2 {
            let h = parts[0].parse::<u32>().ok()?;
            let m = parts[1].parse::<u32>().ok()?;
            let sec = parts
                .get(2)
                .map(|x| x.parse::<u32>())
                .transpose()
                .ok()?
                .unwrap_or(0);
            if let Some(t) = NaiveTime::from_hms_opt(h, m, sec) {
                return local(today.and_time(t));
            }
        }
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Kolkata));
    }
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
            return local(n);
        }
    }
    None
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

/// An open position as the tracker reads it.
#[derive(Debug, Clone, Copy, Default)]
pub struct PositionNow {
    pub quantity: f64,
    pub average_price: f64,
    pub pnl: f64,
}

/// Position book rows keyed `SYMBOL_EXCHANGE`, in book order.
pub fn positions_from(rows: &[Value]) -> Vec<(String, PositionNow)> {
    rows.iter()
        .map(|p| {
            let key = format!("{}_{}", text(p.get("symbol")), text(p.get("exchange")));
            let all = (
                num(p.get("quantity")),
                num(p.get("average_price")),
                num(p.get("ltp")),
                num(p.get("pnl")),
            );
            let pos = match all {
                (Some(q), Some(a), Some(_), Some(pl)) => PositionNow {
                    quantity: q,
                    average_price: a,
                    pnl: pl,
                },
                _ => PositionNow::default(),
            };
            (key, pos)
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Trade {
    action: String,
    qty: f64,
    price: f64,
    time: Option<Ist>,
}

#[derive(Debug, Clone)]
struct Window {
    start: Option<Ist>,
    end: Option<Ist>,
    qty: f64,
    price: f64,
    buy: bool,
    exit_price: Option<f64>,
}

/// A candle series: (epoch seconds, close).
pub type Candles = Vec<(i64, f64)>;

/// The portfolio frame: time -> column -> value (outer join, NaN = absent).
#[derive(Default)]
struct Frame {
    cols: Vec<String>,
    rows: BTreeMap<i64, HashMap<String, f64>>,
}

impl Frame {
    fn has(&self, c: &str) -> bool {
        self.cols.iter().any(|x| x == c)
    }

    /// pandas `join` refuses an overlapping column.
    fn join(&mut self, col: &str, series: &[(i64, f64)]) -> bool {
        if self.has(col) {
            tracing::warn!(
                "P&L tracker: {} already tracked, skipping a second series",
                col
            );
            return false;
        }
        self.cols.push(col.to_string());
        for (t, v) in series {
            self.rows.entry(*t).or_default().insert(col.to_string(), *v);
        }
        true
    }

    fn drop(&mut self, col: &str) {
        self.cols.retain(|c| c != col);
        for r in self.rows.values_mut() {
            r.remove(col);
        }
    }

    /// `ffill().fillna(0)` then the row sums.
    fn totals(&self) -> Vec<(i64, f64)> {
        let mut last: HashMap<&str, f64> = HashMap::new();
        self.rows
            .iter()
            .map(|(t, r)| {
                let mut sum = 0.0;
                for c in &self.cols {
                    if let Some(v) = r.get(c) {
                        last.insert(c, *v);
                    }
                    sum += last.get(c.as_str()).copied().unwrap_or(0.0);
                }
                (*t, sum)
            })
            .collect()
    }
}

fn ist_of(ts: i64) -> Option<Ist> {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|t| t.with_timezone(&Kolkata))
}

fn at_915(t: Ist) -> Option<Ist> {
    Kolkata
        .from_local_datetime(&t.date_naive().and_hms_opt(9, 15, 0)?)
        .single()
}

fn zero() -> Value {
    json!({"status": "success", "data": {
        "current_mtm": 0, "max_mtm": 0, "max_mtm_time": null, "min_mtm": 0,
        "min_mtm_time": null, "max_drawdown": 0, "pnl_series": [], "drawdown_series": [],
    }})
}

/// Candles from market open (09:15 of the first candle's day) to now.
fn from_open(c: &Candles, now: Ist) -> Vec<(i64, f64)> {
    let Some(open) = c.first().and_then(|(t, _)| ist_of(*t)).and_then(at_915) else {
        return Vec::new();
    };
    c.iter()
        .filter(|(t, _)| ist_of(*t).is_some_and(|x| x >= open && x <= now))
        .copied()
        .collect()
}

/// The tracker body. `trades` and `positions` are the trade and position
/// book rows; `history(symbol, exchange, date)` returns 1-minute candles
/// (`None` when the broker had none).
pub async fn compute<F, Fut>(
    trades: &[Value],
    positions: &[Value],
    now_utc: DateTime<Utc>,
    history: F,
) -> Value
where
    F: Fn(String, String, NaiveDate) -> Fut,
    Fut: Future<Output = Option<Candles>>,
{
    let now = now_utc.with_timezone(&Kolkata);
    let today = now.date_naive();
    let current = positions_from(positions);
    if trades.is_empty() && current.is_empty() {
        return zero();
    }
    let stamp = |t: &Value| {
        ["timestamp", "fill_timestamp", "fill_time"]
            .iter()
            .find_map(|k| t.get(*k).filter(|v| !v.is_null() && *v != "").cloned())
    };
    let mut first: Option<Ist> = None;
    for t in trades {
        if let Some(tt) = stamp(t).and_then(|v| parse_trade_timestamp(&v, today)) {
            if first.is_none_or(|f| tt < f) {
                first = Some(tt);
            }
        }
    }
    let first = first.unwrap_or_else(|| at_915(now).unwrap_or(now));
    let date = first.date_naive();

    // Trades grouped by SYMBOL_EXCHANGE, first-seen order.
    let mut groups: Vec<(String, String, String, Vec<Trade>)> = Vec::new();
    for t in trades {
        let (sym, ex) = (text(t.get("symbol")), text(t.get("exchange")));
        if sym.is_empty() || ex.is_empty() {
            continue;
        }
        let key = format!("{}_{}", sym, ex);
        let price = num(t.get("average_price")).unwrap_or(0.0);
        let mut qty = num(t.get("quantity")).unwrap_or(0.0);
        if qty == 0.0 && price > 0.0 {
            let value = num(t.get("trade_value")).unwrap_or(0.0);
            if value == price {
                qty = 1.0;
            } else if value > 0.0 {
                qty = value / price;
            }
        }
        let trade = Trade {
            action: text(t.get("action")),
            qty,
            price,
            time: stamp(t).and_then(|v| parse_trade_timestamp(&v, today)),
        };
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.3.push(trade),
            None => groups.push((key, sym, ex, vec![trade])),
        }
    }

    let mut frame = Frame::default();
    for (_, symbol, exchange, list) in groups.iter_mut() {
        list.sort_by_key(|t| t.time);
        let mut net = 0.0;
        let mut windows: Vec<Window> = Vec::new();
        for t in list.iter() {
            if t.qty <= 0.0 {
                continue;
            }
            if t.action == "BUY" {
                windows.push(Window {
                    start: t.time,
                    end: None,
                    qty: t.qty,
                    price: t.price,
                    buy: true,
                    exit_price: None,
                });
                net += t.qty;
            } else if net > 0.0 {
                let mut remaining = t.qty;
                let mut i = 0;
                while i < windows.len() {
                    if windows[i].buy && windows[i].end.is_none() && remaining > 0.0 {
                        let close = windows[i].qty.min(remaining);
                        if close == windows[i].qty {
                            windows[i].end = t.time;
                            windows[i].exit_price = Some(t.price);
                        } else {
                            windows[i].qty -= close;
                            let mut closed = windows[i].clone();
                            closed.qty = close;
                            closed.end = t.time;
                            closed.exit_price = Some(t.price);
                            windows.push(closed);
                        }
                        remaining -= close;
                    }
                    i += 1;
                }
                net -= t.qty;
            } else {
                windows.push(Window {
                    start: t.time,
                    end: None,
                    qty: t.qty,
                    price: t.price,
                    buy: false,
                    exit_price: None,
                });
                net -= t.qty;
            }
        }
        let Some(candles) = history(symbol.clone(), exchange.clone(), date)
            .await
            .filter(|c| !c.is_empty())
        else {
            continue;
        };
        let bars: Vec<(Ist, i64, f64)> = candles
            .iter()
            .filter_map(|(t, c)| ist_of(*t).map(|i| (i, *t, *c)))
            .filter(|(i, _, _)| *i >= first && *i <= now)
            .collect();
        let mut pnl = vec![0.0f64; bars.len()];
        let mut realized = 0.0;
        windows.sort_by_key(|w| w.start);
        for w in &windows {
            let Some(start) = w.start else {
                continue;
            };
            let end = w.end.unwrap_or(now);
            let in_window: Vec<usize> = (0..bars.len())
                .filter(|i| bars[*i].0 >= start && bars[*i].0 <= end)
                .collect();
            let closed = w.end.is_some() && w.exit_price.is_some();
            if in_window.is_empty() && !closed {
                continue;
            }
            for i in &in_window {
                let px = bars[*i].2;
                pnl[*i] += if w.buy {
                    (px - w.price) * w.qty
                } else {
                    (w.price - px) * w.qty
                };
            }
            if let (true, Some(exit)) = (closed, w.exit_price) {
                realized += if w.buy {
                    (exit - w.price) * w.qty
                } else {
                    (w.price - exit) * w.qty
                };
            }
            if let Some(e) = w.end {
                let future: Vec<usize> = (0..bars.len()).filter(|i| bars[*i].0 > e).collect();
                if !future.is_empty() {
                    for i in future {
                        pnl[i] = realized;
                    }
                } else if realized != 0.0 {
                    if let Some(last) = pnl.last_mut() {
                        *last = realized;
                    }
                }
            }
        }
        let series: Vec<(i64, f64)> = bars.iter().zip(&pnl).map(|(b, p)| (b.1, *p)).collect();
        frame.join(&format!("{}_pnl", symbol), &series);
    }

    // Carried-forward positions.
    let mut carried = false;
    for (key, pos) in &current {
        let Some((symbol, exchange)) = key.rsplit_once('_') else {
            continue;
        };
        let col = format!("{}_pnl", symbol);
        let traded = groups.iter().find(|g| &g.0 == key);
        if pos.quantity != 0.0 && traded.is_none() {
            let Some(c) = history(symbol.into(), exchange.into(), date)
                .await
                .filter(|c| !c.is_empty())
            else {
                continue;
            };
            let series: Vec<(i64, f64)> = from_open(&c, now)
                .into_iter()
                .map(|(t, px)| {
                    let v = if pos.quantity > 0.0 {
                        (px - pos.average_price) * pos.quantity
                    } else {
                        (pos.average_price - px) * pos.quantity.abs()
                    };
                    (t, v)
                })
                .collect();
            if frame.join(&col, &series) {
                carried = true;
            }
        } else if pos.quantity == 0.0 && pos.pnl != 0.0 {
            let Some(list) = traded.map(|g| &g.3) else {
                continue;
            };
            if list.is_empty() {
                continue;
            }
            let buys = list.iter().any(|t| t.action == "BUY");
            let sells = list.iter().any(|t| t.action == "SELL");
            if buys && sells {
                continue;
            }
            let was_long = if list.iter().all(|t| t.action == "SELL") {
                true
            } else if list.iter().all(|t| t.action == "BUY") {
                false
            } else {
                continue;
            };
            let total_qty: f64 = list.iter().map(|t| t.qty).sum();
            if total_qty == 0.0 {
                continue;
            }
            let exit = list.iter().map(|t| t.price * t.qty).sum::<f64>() / total_qty;
            let entry = if was_long {
                exit - pos.pnl / total_qty
            } else {
                exit + pos.pnl / total_qty
            };
            let close_time = list.last().and_then(|t| t.time);
            let Some(c) = history(symbol.into(), exchange.into(), date)
                .await
                .filter(|c| !c.is_empty())
            else {
                continue;
            };
            let series: Vec<(i64, f64)> = from_open(&c, now)
                .into_iter()
                .map(|(t, px)| {
                    let before = close_time.is_none_or(|ct| ist_of(t).is_some_and(|x| x <= ct));
                    let v = if before {
                        if was_long {
                            (px - entry) * total_qty
                        } else {
                            (entry - px) * total_qty
                        }
                    } else {
                        pos.pnl
                    };
                    (t, v)
                })
                .collect();
            if frame.has(&col) {
                frame.drop(&col);
            }
            if frame.join(&col, &series) {
                carried = true;
            }
        }
    }

    let totals: Vec<(i64, f64)> = if frame.cols.is_empty() && !current.is_empty() {
        let mut frame2 = Frame::default();
        for (key, pos) in &current {
            let Some((symbol, exchange)) = key.rsplit_once('_') else {
                continue;
            };
            if pos.quantity == 0.0 {
                continue;
            }
            let Some(c) = history(symbol.into(), exchange.into(), date)
                .await
                .filter(|c| !c.is_empty())
            else {
                continue;
            };
            let series: Vec<(i64, f64)> = from_open(&c, now)
                .into_iter()
                .map(|(t, px)| {
                    let v = if pos.quantity > 0.0 {
                        (px - pos.average_price) * pos.quantity
                    } else {
                        (pos.average_price - px) * pos.quantity.abs()
                    };
                    (t, v)
                })
                .collect();
            frame2.join(&format!("{}_pnl", symbol), &series);
        }
        if frame2.cols.is_empty() {
            // A flat line at the current P&L from 09:00 to now.
            let total: f64 = current.iter().map(|(_, p)| p.pnl).sum();
            let start = Kolkata
                .from_local_datetime(&today.and_hms_opt(9, 0, 0).unwrap_or_default())
                .single()
                .unwrap_or(now);
            let end = if now <= start {
                start + chrono::Duration::minutes(1)
            } else {
                now
            };
            let mut out = Vec::new();
            let mut t = start;
            while t <= end {
                out.push((t.timestamp(), total));
                t += chrono::Duration::minutes(1);
            }
            out
        } else {
            frame2.totals()
        }
    } else if !frame.cols.is_empty() {
        if !trades.is_empty() && !carried {
            if let Some(open) = at_915(first) {
                if first > open {
                    let mut t = open;
                    let mut pre = Vec::new();
                    while t <= first {
                        pre.push(t.timestamp());
                        t += chrono::Duration::minutes(1);
                    }
                    pre.pop();
                    for ts in pre {
                        let row = frame.rows.entry(ts).or_default();
                        for c in &frame.cols {
                            row.entry(c.clone()).or_insert(0.0);
                        }
                    }
                }
            }
        }
        frame.totals()
    } else {
        return zero();
    };
    if totals.is_empty() {
        return zero();
    }

    let mut peak = f64::NEG_INFINITY;
    let mut pnl_series = Vec::with_capacity(totals.len());
    let mut dd_series = Vec::with_capacity(totals.len());
    let (mut max, mut min, mut max_dd) = (f64::NEG_INFINITY, f64::INFINITY, f64::INFINITY);
    let (mut max_t, mut min_t) = (totals[0].0, totals[0].0);
    for (t, v) in &totals {
        peak = peak.max(*v);
        let dd = v - peak;
        if *v > max {
            max = *v;
            max_t = *t;
        }
        if *v < min {
            min = *v;
            min_t = *t;
        }
        max_dd = max_dd.min(dd);
        pnl_series.push(json!({"time": t * 1000, "value": round2(*v)}));
        dd_series.push(json!({"time": t * 1000, "value": round2(dd)}));
    }
    let hm = |t: i64| ist_of(t).map(|x| x.format("%H:%M").to_string());
    let latest = totals.last().map(|x| x.1).unwrap_or(0.0);
    json!({"status": "success", "data": {
        "current_mtm": round2(latest),
        "max_mtm": round2(max),
        "max_mtm_time": hm(max_t),
        "min_mtm": round2(min),
        "min_mtm_time": hm(min_t),
        "max_drawdown": round2(max_dd),
        "pnl_series": pnl_series,
        "drawdown_series": dd_series,
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ist(h: u32, m: u32) -> i64 {
        Kolkata
            .with_ymd_and_hms(2026, 10, 5, h, m, 0)
            .unwrap()
            .timestamp()
    }

    #[test]
    fn timestamps_parse_in_the_broker_formats() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        let want = Kolkata.with_ymd_and_hms(2025, 12, 17, 10, 54, 3).unwrap();
        assert_eq!(
            parse_trade_timestamp(&json!("17-Dec-2025 10:54:03"), d),
            Some(want)
        );
        assert_eq!(
            parse_trade_timestamp(&json!("10:54:03 17-12-2025"), d),
            Some(want)
        );
        assert_eq!(
            parse_trade_timestamp(&json!("10:30:52"), d)
                .unwrap()
                .date_naive(),
            d
        );
        assert_eq!(
            parse_trade_timestamp(&json!(want.timestamp()), d),
            Some(want)
        );
        assert_eq!(parse_trade_timestamp(&json!("junk"), d), None);
    }

    #[tokio::test]
    async fn round_trip_mtm_then_realized() {
        let now = Kolkata
            .with_ymd_and_hms(2026, 10, 5, 9, 25, 0)
            .unwrap()
            .with_timezone(&Utc);
        let trades = vec![
            json!({"symbol": "SBIN", "exchange": "NSE", "action": "BUY", "quantity": 10,
                   "average_price": 100.0, "timestamp": "2026-10-05 09:17:00"}),
            json!({"symbol": "SBIN", "exchange": "NSE", "action": "SELL", "quantity": 10,
                   "average_price": 103.0, "timestamp": "2026-10-05 09:20:00"}),
        ];
        let candles: Candles = (17..=24)
            .map(|m| (ist(9, m), 100.0 + f64::from(m - 17)))
            .collect();
        let v = compute(&trades, &[], now, |_, _, _| {
            let c = candles.clone();
            async move { Some(c) }
        })
        .await;
        let d = &v["data"];
        // 09:15 and 09:16 are zero-filled before the first trade.
        assert_eq!(d["pnl_series"][0]["time"], ist(9, 15) * 1000);
        assert_eq!(d["pnl_series"][0]["value"], 0.0);
        // 09:20 mark: (103-100)*10; afterwards realized 30.
        assert_eq!(d["current_mtm"], 30.0);
        assert_eq!(d["max_mtm"], 30.0);
        assert_eq!(d["max_drawdown"], 0.0);
    }

    #[tokio::test]
    async fn nothing_to_track_is_zero() {
        let v = compute(&[], &[], Utc::now(), |_, _, _| async { None }).await;
        assert_eq!(v["data"]["current_mtm"], 0);
        assert_eq!(v["data"]["pnl_series"], json!([]));
    }

    #[tokio::test]
    async fn position_without_history_is_a_flat_line() {
        let now = Kolkata
            .with_ymd_and_hms(2026, 10, 5, 9, 5, 0)
            .unwrap()
            .with_timezone(&Utc);
        let pos = vec![json!({"symbol": "SBIN", "exchange": "NSE", "quantity": 5,
            "average_price": "100.00", "ltp": 101.0, "pnl": 5.0})];
        let v = compute(&[], &pos, now, |_, _, _| async { None }).await;
        assert_eq!(v["data"]["pnl_series"].as_array().unwrap().len(), 6);
        assert_eq!(v["data"]["current_mtm"], 5.0);
    }
}
