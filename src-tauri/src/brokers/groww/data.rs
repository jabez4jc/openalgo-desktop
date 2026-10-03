//! Quotes, multiquotes, depth and history (web `api/data.py`).

use super::mapping::{groww_exchange, groww_segment, SEGMENT_FNO};
use super::{groww_error, Category, GrowwCore, TIMEFRAME_MAP};
use crate::brokers::common::history::{chunks, sort_dedupe};
use crate::brokers::common::streaming::round2;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{Datelike, NaiveDate, TimeZone, Timelike};
use reqwest::Method;
use serde_json::Value;
use std::collections::HashMap;

/// Instruments per `/v1/live-data/ohlc` call ("up to 50").
pub const OHLC_BATCH: usize = 50;
/// Times a batch is retried after dropping a symbol Groww calls invalid.
pub const INVALID_SYMBOL_RETRIES: usize = 5;
/// Consecutive rate-limit refusals that end the F&O quote overlay.
pub const MAX_CONSECUTIVE_429: usize = 4;

fn ist() -> chrono_tz::Tz {
    chrono_tz::Asia::Kolkata
}

/// Number from a JSON value (number or numeric string), 0 otherwise.
pub fn to_f64(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Python `a or b or c` over numeric keys: the first non-zero value.
fn first_num(v: &Value, keys: &[&str]) -> f64 {
    keys.iter()
        .map(|k| to_f64(v.get(*k)))
        .find(|x| *x != 0.0)
        .unwrap_or(0.0)
}

/// Groww `ohlc`: a dict, or a non-JSON string such as
/// `"{open: 149.50,high: 150.50,low: 148.50,close: 149.50}"` (quirk 9.14).
pub fn parse_ohlc(v: Option<&Value>) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    match v {
        Some(Value::Object(m)) => {
            for (k, x) in m {
                out.insert(k.clone(), to_f64(Some(x)));
            }
        }
        Some(Value::String(s)) => {
            for part in s.trim().trim_matches(|c| c == '{' || c == '}').split(',') {
                let kv: Vec<&str> = part.split(':').collect();
                if kv.len() == 2 {
                    if let Ok(x) = kv[1].trim().parse::<f64>() {
                        out.insert(kv[0].trim().trim_matches('"').to_string(), x);
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn depth_side(p: &Value, side: &str) -> Vec<DepthLevel> {
    p.pointer(&format!("/depth/{}", side))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|l| DepthLevel {
                    price: to_f64(l.get("price")),
                    quantity: to_f64(l.get("quantity")) as i64,
                    orders: to_f64(l.get("orders")) as i64,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn is_derivative(exchange: &str) -> bool {
    matches!(exchange, "NFO" | "BFO")
}

/// `/v1/live-data/quote` payload -> web quote (`prev_close` = `ohlc.close`;
/// OI only for derivatives; bid/ask fall back to the top of the book).
pub fn to_quote(key: &QuoteKey, p: &Value) -> Quote {
    let ohlc = parse_ohlc(p.get("ohlc"));
    let o = |k: &str| ohlc.get(k).copied().unwrap_or(0.0);
    let buy = depth_side(p, "buy");
    let sell = depth_side(p, "sell");
    let mut bid = first_num(p, &["bid_price", "bid", "best_bid_price"]);
    let mut ask = first_num(
        p,
        &["offer_price", "ask", "best_offer_price", "best_ask_price"],
    );
    let mut bid_qty = first_num(p, &["bid_quantity", "bid_size", "best_bid_quantity"]);
    let mut ask_qty = first_num(
        p,
        &[
            "offer_quantity",
            "ask_quantity",
            "ask_size",
            "offer_size",
            "best_offer_quantity",
        ],
    );
    if bid == 0.0 {
        if let Some(l) = buy.first() {
            bid = l.price;
            if bid_qty == 0.0 {
                bid_qty = l.quantity as f64;
            }
        }
    }
    if ask == 0.0 {
        if let Some(l) = sell.first() {
            ask = l.price;
            if ask_qty == 0.0 {
                ask_qty = l.quantity as f64;
            }
        }
    }
    let ltp = to_f64(p.get("last_price"));
    let close = o("close");
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        open: o("open"),
        high: o("high"),
        low: o("low"),
        close,
        volume: first_num(p, &["volume", "total_volume", "traded_volume"]) as i64,
        bid,
        ask,
        bid_qty: bid_qty as i64,
        ask_qty: ask_qty as i64,
        oi: if is_derivative(&key.exchange) {
            first_num(p, &["open_interest", "oi"]) as i64
        } else {
            0
        },
        change: to_f64(p.get("day_change")),
        change_percent: to_f64(p.get("day_change_perc")),
        timestamp: match p.get("last_trade_time") {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
    };
    if q.change == 0.0 && close > 0.0 && ltp > 0.0 {
        q.change = round2(ltp - close);
        q.change_percent = round2((ltp - close) / close * 100.0);
    }
    q
}

/// Web `get_depth`: five levels per side, padded with zero levels.
pub fn to_depth(key: &QuoteKey, p: &Value) -> MarketDepth {
    let ohlc = parse_ohlc(p.get("ohlc"));
    let o = |k: &str| ohlc.get(k).copied().unwrap_or(0.0);
    let pad = |mut v: Vec<DepthLevel>| {
        v.truncate(5);
        v.resize(5, DepthLevel::default());
        v.into_iter()
            .map(|l| DepthLevel { orders: 0, ..l })
            .collect::<Vec<_>>()
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: pad(depth_side(p, "buy")),
        asks: pad(depth_side(p, "sell")),
        ltp: to_f64(p.get("last_price")),
        ltq: to_f64(p.get("last_trade_quantity")) as i64,
        open: o("open"),
        high: o("high"),
        low: o("low"),
        prev_close: o("close"),
        volume: first_num(p, &["volume", "total_volume", "traded_volume"]) as i64,
        oi: if is_derivative(&key.exchange) {
            first_num(p, &["open_interest", "oi"]) as i64
        } else {
            0
        },
        total_buy_qty: to_f64(p.get("total_buy_quantity")) as i64,
        total_sell_qty: to_f64(p.get("total_sell_quantity")) as i64,
    }
}

/// Web `_convert_openalgo_to_groww_derivative_symbol` (last resort when the
/// master has no row): `SBIN30SEP25FUT` -> `SBIN25SEPFUT`,
/// `SBIN30SEP25800CE` -> `SBIN25SEP800CE`.
pub fn derivative_symbol_fallback(symbol: &str) -> String {
    let alpha_end = symbol
        .char_indices()
        .find(|(_, c)| !c.is_ascii_uppercase())
        .map(|(i, _)| i)
        .unwrap_or(symbol.len());
    let (base, rest) = symbol.split_at(alpha_end);
    if base.is_empty() || rest.len() < 7 {
        return symbol.to_string();
    }
    let (dd, mon, yy, tail) = (&rest[0..2], &rest[2..5], &rest[5..7], &rest[7..]);
    let ok = dd.bytes().all(|b| b.is_ascii_digit())
        && mon.bytes().all(|b| b.is_ascii_uppercase())
        && yy.bytes().all(|b| b.is_ascii_digit());
    if !ok {
        return symbol.to_string();
    }
    if tail == "FUT" {
        return format!("{}{}{}FUT", base, yy, mon);
    }
    for opt in ["CE", "PE"] {
        if let Some(strike) = tail.strip_suffix(opt) {
            if !strike.is_empty() && strike.bytes().all(|b| b.is_ascii_digit()) {
                return format!("{}{}{}{}{}", base, yy, mon, strike, opt);
            }
        }
    }
    symbol.to_string()
}

/// Groww trading symbol for an OpenAlgo instrument.
pub fn trading_symbol(core: &GrowwCore, key: &QuoteKey) -> String {
    match core.symbols.br_symbol(&key.symbol, &key.exchange) {
        Some(s) => s,
        None if is_derivative(&key.exchange) => derivative_symbol_fallback(&key.symbol),
        None => key.symbol.clone(),
    }
}

/// `exchange`, `segment` and `trading_symbol` query for one instrument.
/// The web sent `BSE_INDEX` to NSE (default branch); it goes to BSE here.
fn quote_query(core: &GrowwCore, key: &QuoteKey) -> String {
    format!(
        "exchange={}&segment={}&trading_symbol={}",
        groww_exchange(&key.exchange),
        groww_segment(&key.exchange),
        urlencoding::encode(&trading_symbol(core, key))
    )
}

async fn fetch_quote(core: &GrowwCore, auth: &AuthToken, key: &QuoteKey) -> Result<Value> {
    core.call(
        Method::GET,
        &format!("/v1/live-data/quote?{}", quote_query(core, key)),
        auth,
        None,
        Category::Live,
    )
    .await
}

pub async fn get_quote(core: &GrowwCore, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let p = fetch_quote(core, auth, key).await?;
    Ok(to_quote(key, &p))
}

pub async fn get_market_depth(
    core: &GrowwCore,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let p = fetch_quote(core, auth, key).await?;
    Ok(to_depth(key, &p))
}

/// One `/v1/live-data/ohlc` entry: a dict, an OHLC string, or a bare LTP.
/// `ltp` and the previous close are both read from `close`, like the web.
pub fn quote_from_ohlc(key: &QuoteKey, v: &Value) -> Quote {
    let (open, high, low, close) = match v {
        Value::Number(_) => {
            let x = to_f64(Some(v));
            (0.0, 0.0, 0.0, x)
        }
        other => {
            let o = parse_ohlc(Some(other));
            let g = |k: &str| o.get(k).copied().unwrap_or(0.0);
            (g("open"), g("high"), g("low"), g("close"))
        }
    };
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: close,
        open,
        high,
        low,
        close,
        ..Default::default()
    }
}

/// `Invalid trading symbol: XYZ` in a Groww error -> `XYZ`.
pub fn invalid_symbol(text: &str) -> Option<String> {
    const MARK: &str = "Invalid trading symbol: ";
    let i = text.find(MARK)? + MARK.len();
    let s: String = text[i..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '&' | '-'))
        .collect();
    (!s.is_empty()).then_some(s)
}

/// `NSE_SBIN` / `BSE_RELIANCE` key used by the OHLC endpoint.
pub fn exchange_symbol(core: &GrowwCore, key: &QuoteKey) -> String {
    format!(
        "{}_{}",
        groww_exchange(&key.exchange),
        trading_symbol(core, key)
    )
}

/// OHLC for one segment's instruments, dropping symbols Groww reports as
/// invalid and retrying (at most `INVALID_SYMBOL_RETRIES` times).
async fn ohlc_batch(
    core: &GrowwCore,
    auth: &AuthToken,
    segment: &str,
    mut wanted: Vec<String>,
    out: &mut HashMap<String, std::result::Result<Value, String>>,
) -> Result<()> {
    for _ in 0..=INVALID_SYMBOL_RETRIES {
        if wanted.is_empty() {
            return Ok(());
        }
        let path = format!(
            "/v1/live-data/ohlc?segment={}&exchange_symbols={}",
            segment,
            urlencoding::encode(&wanted.join(","))
        );
        let r = core
            .send(Method::GET, &path, auth, None, Category::Ohlc, false)
            .await?;
        if r.is_success() {
            for es in &wanted {
                match r.payload().get(es) {
                    Some(v) if !v.is_null() => {
                        out.insert(es.clone(), Ok(v.clone()));
                    }
                    _ => {
                        out.insert(es.clone(), Err("No quote data available".into()));
                    }
                }
            }
            return Ok(());
        }
        let text = r.body.to_string();
        match invalid_symbol(&text) {
            Some(bad) => {
                let before = wanted.len();
                wanted.retain(|es| {
                    let drop = es.split_once('_').map(|(_, s)| s) == Some(bad.as_str());
                    if drop {
                        out.insert(es.clone(), Err("Invalid trading symbol in Groww".into()));
                    }
                    !drop
                });
                if wanted.len() == before {
                    break;
                }
            }
            None => {
                let msg = match groww_error(&r) {
                    AppError::Broker(m) => m,
                    e => e.client_message(),
                };
                for es in &wanted {
                    out.insert(es.clone(), Err(msg.clone()));
                }
                return Ok(());
            }
        }
    }
    for es in wanted {
        out.entry(es)
            .or_insert_with(|| Err("Groww refused the quote request.".into()));
    }
    Ok(())
}

/// Web `get_multiquotes`: OHLC batches of 50 per segment, then for F&O a
/// paced per-symbol quote overlay for bid/ask/volume/OI that stops after
/// four consecutive rate-limit refusals.
pub async fn get_multiquotes(
    core: &GrowwCore,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let es: Vec<String> = keys.iter().map(|k| exchange_symbol(core, k)).collect();
    let mut data: HashMap<String, std::result::Result<Value, String>> = HashMap::new();
    for batch in (0..keys.len()).collect::<Vec<_>>().chunks(OHLC_BATCH) {
        for segment in ["CASH", SEGMENT_FNO] {
            let mut wanted: Vec<String> = batch
                .iter()
                .filter(|i| groww_segment(&keys[**i].exchange) == segment)
                .map(|i| es[*i].clone())
                .filter(|e| !data.contains_key(e))
                .collect();
            wanted.dedup();
            ohlc_batch(core, auth, segment, wanted, &mut data).await?;
        }
    }
    let mut out: Vec<QuoteResult> = keys
        .iter()
        .zip(&es)
        .map(|(k, e)| match data.get(e) {
            Some(Ok(v)) => QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: Some(quote_from_ohlc(k, v)),
                error: None,
            },
            Some(Err(m)) => QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: None,
                error: Some(m.clone()),
            },
            None => QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: None,
                error: Some("No quote data available".into()),
            },
        })
        .collect();
    // F&O overlay.
    let mut consecutive = 0;
    for r in out.iter_mut().filter(|r| is_derivative(&r.exchange)) {
        let Some(base) = r.data.as_mut() else {
            continue;
        };
        if consecutive >= MAX_CONSECUTIVE_429 {
            break;
        }
        let key = QuoteKey::new(r.exchange.clone(), r.symbol.clone());
        match fetch_quote(core, auth, &key).await {
            Ok(p) => {
                consecutive = 0;
                let full = to_quote(&key, &p);
                base.bid = full.bid;
                base.ask = full.ask;
                base.bid_qty = full.bid_qty;
                base.ask_qty = full.ask_qty;
                base.volume = full.volume;
                base.oi = full.oi;
            }
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => {
                let m = e.client_message();
                if m.contains("limiting requests") || m.contains("429") || m.contains("Rate limit")
                {
                    consecutive += 1;
                } else {
                    consecutive = 0;
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Groww `interval_in_minutes` for an OpenAlgo interval.
pub fn interval_minutes(interval: &str) -> Result<u32> {
    TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == interval)
        .and_then(|(_, v)| v.parse().ok())
        .ok_or_else(|| {
            let list: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Interval {} is not supported by Groww. Use one of: {}.",
                interval,
                list.join(", ")
            ))
        })
}

/// Days per request (web `get_history` chunking).
pub fn chunk_days(minutes: u32) -> i64 {
    match minutes {
        m if m >= 10080 => 300,
        m if m >= 1440 => 100,
        m if m >= 60 => 15,
        m if m >= 5 => 7,
        _ => 3,
    }
}

/// History `(exchange, segment)` (web `:187-195`).
pub fn history_exchange_segment(exchange: &str) -> Result<(&'static str, &'static str)> {
    match exchange {
        "NSE" | "NSE_INDEX" => Ok(("NSE", "CASH")),
        "BSE" | "BSE_INDEX" => Ok(("BSE", "CASH")),
        "NFO" => Ok(("NSE", "FNO")),
        "BFO" => Ok(("BSE", "FNO")),
        other => Err(AppError::Validation(format!(
            "Groww has no history for {} instruments.",
            other
        ))),
    }
}

/// One raw candle `[ts, o, h, l, c, v]` or `{timestamp, open, ...}`;
/// millisecond timestamps (after 2100-01-01 in seconds) become seconds.
pub fn raw_candle(v: &Value) -> Option<Candle> {
    let (ts, o, h, l, c, vol) = match v {
        Value::Array(a) => (
            to_f64(a.first()),
            to_f64(a.get(1)),
            to_f64(a.get(2)),
            to_f64(a.get(3)),
            to_f64(a.get(4)),
            to_f64(a.get(5)),
        ),
        Value::Object(_) => (
            to_f64(v.get("timestamp")),
            to_f64(v.get("open")),
            to_f64(v.get("high")),
            to_f64(v.get("low")),
            to_f64(v.get("close")),
            to_f64(v.get("volume")),
        ),
        _ => return None,
    };
    if ts <= 0.0 {
        return None;
    }
    let ts = if ts > 4_102_444_800.0 {
        ts / 1000.0
    } else {
        ts
    };
    Some(Candle {
        timestamp: ts as i64,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: vol as i64,
        oi: 0,
    })
}

fn ist_date(ts: i64) -> Option<NaiveDate> {
    ist().timestamp_opt(ts, 0).single().map(|d| d.date_naive())
}

fn midnight_utc(d: NaiveDate) -> i64 {
    d.and_hms_opt(0, 0, 0)
        .map(|n| n.and_utc().timestamp())
        .unwrap_or(0)
}

/// Candle processing per interval (web `:461-639`, `:714-838`):
/// * daily: stamped at midnight UTC of the IST trading date (quirk 9.11);
/// * weekly: daily candles bucketed into Monday-start weeks, stamped at
///   09:15 IST of that Monday;
/// * intraday: candles outside 09:15-15:30 IST are dropped.
pub fn process_candles(raw: Vec<Candle>, minutes: u32) -> Vec<Candle> {
    if minutes >= 1440 {
        let mut daily: Vec<Candle> = raw
            .into_iter()
            .filter_map(|c| {
                Some(Candle {
                    timestamp: midnight_utc(ist_date(c.timestamp)?),
                    ..c
                })
            })
            .collect();
        daily = sort_dedupe(daily);
        if minutes < 10080 {
            return daily;
        }
        let mut weeks: Vec<(NaiveDate, Candle)> = Vec::new();
        for c in daily {
            let Some(day) =
                chrono::DateTime::from_timestamp(c.timestamp, 0).map(|d| d.date_naive())
            else {
                continue;
            };
            let monday =
                day - chrono::Duration::days(i64::from(day.weekday().num_days_from_monday()));
            match weeks.last_mut() {
                Some((m, w)) if *m == monday => {
                    w.high = w.high.max(c.high);
                    w.low = w.low.min(c.low);
                    w.close = c.close;
                    w.volume += c.volume;
                }
                _ => weeks.push((monday, c)),
            }
        }
        return weeks
            .into_iter()
            .filter_map(|(m, mut w)| {
                let open = ist()
                    .from_local_datetime(&m.and_hms_opt(9, 15, 0)?)
                    .single()?;
                w.timestamp = open.timestamp();
                Some(w)
            })
            .collect();
    }
    let open = 9 * 60 + 15;
    let close = 15 * 60 + 30;
    sort_dedupe(
        raw.into_iter()
            .filter(|c| {
                ist()
                    .timestamp_opt(c.timestamp, 0)
                    .single()
                    .map(|d| {
                        let m = d.hour() * 60 + d.minute();
                        (open..=close).contains(&m)
                    })
                    .unwrap_or(false)
            })
            .collect(),
    )
}

pub async fn get_history(
    core: &GrowwCore,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let minutes = interval_minutes(&req.interval)?;
    let (exchange, segment) = history_exchange_segment(&req.key.exchange)?;
    let symbol = trading_symbol(core, &req.key);
    let mut raw = Vec::new();
    for (from, to) in chunks(req.start, req.end, chunk_days(minutes)) {
        let path = format!(
            "/v1/historical/candle/range?exchange={}&segment={}&trading_symbol={}&start_time={}&end_time={}&interval_in_minutes={}",
            exchange,
            segment,
            urlencoding::encode(&symbol),
            urlencoding::encode(&format!("{} 09:15:00", from.format("%Y-%m-%d"))),
            urlencoding::encode(&format!("{} 15:30:00", to.format("%Y-%m-%d"))),
            minutes
        );
        let r = core
            .send(Method::GET, &path, auth, None, Category::Other, false)
            .await?;
        if !r.is_success() {
            tracing::warn!(
                "Groww history chunk {}..{} skipped: {}",
                from,
                to,
                r.error_message()
            );
            continue;
        }
        if let Some(rows) = r.payload().get("candles").and_then(Value::as_array) {
            raw.extend(rows.iter().filter_map(raw_candle));
        }
    }
    Ok(process_candles(raw, minutes))
}
