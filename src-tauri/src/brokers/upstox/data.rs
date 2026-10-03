//! Quotes, multiquotes, depth and history (web `api/data.py`).
//!
//! * Quotes and depth: `GET /v3/market-quote/quotes?instrument_key=..`; the
//!   answer is keyed `EXCHANGE:TRADING_SYMBOL`, so entries are matched on
//!   their inner `instrument_token`. `prev_close` is `prev_close_price`, never
//!   the live `ohlc.close`.
//! * `GLOBAL_INDICATOR|..` keys are LTP-only: `GET /v2/market-quote/ltp`,
//!   one key per call (their outer key collides as `GLOBAL_INDICATOR:null`).
//! * History: v3 candles, path order `{to}/{from}`, chunked per interval,
//!   the intraday endpoint for a chunk ending today, and a synthetic daily
//!   candle from quotes when today's bar is not out yet.

use super::{Category, UpstoxBroker, TIMEFRAME_MAP};
use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::history::{chunks, sort_dedupe};
use crate::brokers::common::streaming::round2;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

/// `/v3/market-quote/quotes` accepts at most 500 keys (UDAPI100042).
pub const FULL_QUOTE_BATCH: usize = 500;
const INDICATOR_PREFIX: &str = "GLOBAL_INDICATOR|";

const KNOWN_EXCHANGES: &[&str] = &[
    "NSE",
    "BSE",
    "NFO",
    "BFO",
    "CDS",
    "MCX",
    "NSE_INDEX",
    "BSE_INDEX",
    "MCX_INDEX",
    "NSE_EQ",
    "NSE_FO",
    "BSE_EQ",
    "BSE_FO",
    "MCX_FO",
    "NSE_CD",
];

/// Today's date in IST.
pub fn today_ist() -> NaiveDate {
    Utc::now().with_timezone(&Kolkata).date_naive()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpstoxOhlc {
    #[serde(deserialize_with = "f64_lenient")]
    pub open: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub high: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub low: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub close: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpstoxLevel {
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub orders: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpstoxDepth {
    pub buy: Vec<UpstoxLevel>,
    pub sell: Vec<UpstoxLevel>,
}

/// One `/v3/market-quote/quotes` entry.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UpstoxQuote {
    #[serde(deserialize_with = "string_lenient")]
    pub instrument_token: String,
    #[serde(deserialize_with = "string_lenient")]
    pub timestamp: String,
    #[serde(deserialize_with = "f64_lenient")]
    pub last_price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub last_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub volume: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub oi: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub prev_close_price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub total_buy_quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub total_sell_quantity: i64,
    pub ohlc: Option<UpstoxOhlc>,
    pub depth: Option<UpstoxDepth>,
}

/// web `_quote_from_full_v3`.
pub fn to_quote(key: &QuoteKey, q: &UpstoxQuote) -> Quote {
    let ohlc = q.ohlc.clone().unwrap_or_default();
    let depth = q.depth.clone().unwrap_or_default();
    let bid = depth.buy.first().cloned().unwrap_or_default();
    let ask = depth.sell.first().cloned().unwrap_or_default();
    let mut quote = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: q.last_price,
        open: ohlc.open,
        high: ohlc.high,
        low: ohlc.low,
        close: q.prev_close_price,
        volume: q.volume,
        bid: bid.price,
        ask: ask.price,
        bid_qty: bid.quantity,
        ask_qty: ask.quantity,
        oi: q.oi,
        change: 0.0,
        change_percent: 0.0,
        timestamp: q.timestamp.clone(),
    };
    if quote.close > 0.0 {
        quote.change = round2(quote.ltp - quote.close);
        quote.change_percent = round2((quote.ltp - quote.close) / quote.close * 100.0);
    }
    quote
}

/// web `get_depth`, padded / truncated to five levels per side.
pub fn to_depth(key: &QuoteKey, q: &UpstoxQuote) -> MarketDepth {
    let ohlc = q.ohlc.clone().unwrap_or_default();
    let d = q.depth.clone().unwrap_or_default();
    let pad = |side: &[UpstoxLevel]| -> Vec<DepthLevel> {
        (0..5)
            .map(|i| {
                side.get(i)
                    .map(|l| DepthLevel {
                        price: l.price,
                        quantity: l.quantity,
                        orders: l.orders,
                    })
                    .unwrap_or_default()
            })
            .collect()
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: pad(&d.buy),
        asks: pad(&d.sell),
        ltp: q.last_price,
        ltq: q.last_quantity,
        open: ohlc.open,
        high: ohlc.high,
        low: ohlc.low,
        prev_close: q.prev_close_price,
        volume: q.volume,
        oi: q.oi,
        total_buy_qty: q.total_buy_quantity,
        total_sell_qty: q.total_sell_quantity,
    }
}

/// Entry of a quotes answer whose inner `instrument_token` is `key`.
pub fn find_by_token(data: &Value, key: &str) -> Option<UpstoxQuote> {
    data.as_object()?
        .values()
        .find(|v| v.get("instrument_token").and_then(Value::as_str) == Some(key))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Reversed-argument guard (web `get_quotes`): a symbol that is an exchange
/// code next to an exchange that is not is swapped back.
pub fn normalise_key(key: &QuoteKey) -> QuoteKey {
    if KNOWN_EXCHANGES.contains(&key.symbol.as_str())
        && !KNOWN_EXCHANGES.contains(&key.exchange.as_str())
    {
        tracing::warn!("Quote request had symbol and exchange reversed; corrected");
        QuoteKey::new(key.symbol.clone(), key.exchange.clone())
    } else {
        key.clone()
    }
}

/// Instrument key with the web's index fallbacks: `NSE`/`BSE`/`MCX` retry
/// on the `_INDEX` exchange, `*_INDEX` retries on the base exchange.
pub fn instrument_key(b: &UpstoxBroker, symbol: &str, exchange: &str) -> Option<String> {
    let r = b.resolver();
    if let Some(t) = r.token(symbol, exchange) {
        return Some(t);
    }
    if matches!(exchange, "NSE" | "BSE" | "MCX") {
        if let Some(t) = r.token(symbol, &format!("{}_INDEX", exchange)) {
            return Some(t);
        }
    }
    if let Some(base) = exchange.strip_suffix("_INDEX") {
        if let Some(t) = r.token(symbol, base) {
            return Some(t);
        }
    }
    None
}

fn not_found(key: &QuoteKey) -> AppError {
    AppError::Validation(format!(
        "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
        key.symbol, key.exchange
    ))
}

async fn fetch_full(b: &UpstoxBroker, auth: &AuthToken, keys: &[String]) -> Result<Value> {
    let joined = keys.join(",");
    let url = b.api(&format!(
        "/v3/market-quote/quotes?instrument_key={}",
        urlencoding::encode(&joined)
    ));
    b.call(Method::GET, &url, auth, None, Category::Standard)
        .await
}

/// LTP of one `GLOBAL_INDICATOR|..` key (web `_get_indicator_ltp`).
async fn indicator_ltp(b: &UpstoxBroker, auth: &AuthToken, ik: &str) -> Result<f64> {
    let url = b.api(&format!(
        "/v2/market-quote/ltp?instrument_key={}",
        urlencoding::encode(ik)
    ));
    let data = b
        .call(Method::GET, &url, auth, None, Category::Standard)
        .await?;
    Ok(data
        .as_object()
        .and_then(|m| {
            m.values()
                .find(|v| v.get("instrument_token").and_then(Value::as_str) == Some(ik))
        })
        .and_then(|v| v.get("last_price"))
        .and_then(|p| match p {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .unwrap_or(0.0))
}

fn indicator_quote(key: &QuoteKey, ltp: f64) -> Quote {
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp,
        ..Default::default()
    }
}

pub async fn get_quote(b: &UpstoxBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let key = normalise_key(key);
    let ik = instrument_key(b, &key.symbol, &key.exchange).ok_or_else(|| not_found(&key))?;
    if ik.starts_with(INDICATOR_PREFIX) {
        return Ok(indicator_quote(&key, indicator_ltp(b, auth, &ik).await?));
    }
    let data = fetch_full(b, auth, std::slice::from_ref(&ik)).await?;
    let q = find_by_token(&data, &ik).ok_or_else(|| {
        AppError::Broker(format!(
            "Upstox returned no quote for {} {}.",
            key.exchange, key.symbol
        ))
    })?;
    Ok(to_quote(&key, &q))
}

pub async fn get_multiquotes(
    b: &UpstoxBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let resolved: Vec<(QuoteKey, Option<String>)> = keys
        .iter()
        .map(|k| (k.clone(), instrument_key(b, &k.symbol, &k.exchange)))
        .collect();
    let mut wanted: Vec<String> = resolved
        .iter()
        .filter_map(|(_, ik)| ik.clone())
        .filter(|ik| !ik.starts_with(INDICATOR_PREFIX))
        .collect();
    wanted.sort();
    wanted.dedup();
    let mut quotes: HashMap<String, UpstoxQuote> = HashMap::new();
    for batch in wanted.chunks(FULL_QUOTE_BATCH) {
        let data = fetch_full(b, auth, batch).await?;
        if let Some(m) = data.as_object() {
            for v in m.values() {
                if let Ok(q) = serde_json::from_value::<UpstoxQuote>(v.clone()) {
                    if !q.instrument_token.is_empty() {
                        quotes.insert(q.instrument_token.clone(), q);
                    }
                }
            }
        }
    }
    let mut out = Vec::with_capacity(resolved.len());
    for (k, ik) in resolved {
        let result = match ik {
            None => QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: None,
                error: Some(format!("No token found for {} on {}", k.symbol, k.exchange)),
            },
            // LTP-only feeds, one call each (their batched outer keys collide).
            Some(ik) if ik.starts_with(INDICATOR_PREFIX) => {
                match indicator_ltp(b, auth, &ik).await {
                    Ok(ltp) => QuoteResult {
                        data: Some(indicator_quote(&k, ltp)),
                        symbol: k.symbol.clone(),
                        exchange: k.exchange.clone(),
                        error: None,
                    },
                    Err(e) => QuoteResult {
                        symbol: k.symbol.clone(),
                        exchange: k.exchange.clone(),
                        data: None,
                        error: Some(e.client_message()),
                    },
                }
            }
            Some(ik) => match quotes.get(&ik) {
                Some(q) => QuoteResult {
                    data: Some(to_quote(&k, q)),
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    error: None,
                },
                None => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some("No quote data available".into()),
                },
            },
        };
        out.push(result);
    }
    Ok(out)
}

pub async fn get_market_depth(
    b: &UpstoxBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let key = normalise_key(key);
    let ik = instrument_key(b, &key.symbol, &key.exchange).ok_or_else(|| not_found(&key))?;
    let data = fetch_full(b, auth, std::slice::from_ref(&ik)).await?;
    let q = find_by_token(&data, &ik).ok_or_else(|| {
        AppError::Broker(format!(
            "Upstox returned no market depth for {} {}.",
            key.exchange, key.symbol
        ))
    })?;
    Ok(to_depth(&key, &q))
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// `(unit, interval)` for an OpenAlgo interval key.
pub fn upstox_interval(interval: &str) -> Result<(&'static str, u32)> {
    TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == interval)
        .and_then(|(_, v)| {
            let (unit, n) = v.split_once('/')?;
            Some((unit, n.parse().ok()?))
        })
        .ok_or_else(|| {
            let list: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Interval {} is not supported by Upstox. Use one of: {}.",
                interval,
                list.join(", ")
            ))
        })
}

/// Calendar days per request (web `chunk_limits`); unknown pairs use 30.
pub fn chunk_days(unit: &str, interval: u32) -> i64 {
    match (unit, interval) {
        ("minutes", 1 | 2 | 3 | 5 | 10 | 15) => 30,
        ("minutes", 30 | 60) => 90,
        ("hours", 1..=4) => 90,
        ("days", 1) => 3650,
        ("weeks", 1) | ("months", 1) => 7300,
        _ => 30,
    }
}

/// Timestamp of a raw candle as a tz-aware time. Strings are ISO 8601 with
/// offset; numbers are epoch milliseconds (the intraday filter rewrites them
/// that way), read as UTC.
pub fn candle_time(v: &Value) -> Option<DateTime<FixedOffset>> {
    match v {
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .or_else(|_| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z"))
            .ok(),
        Value::Number(n) => {
            let ms = n.as_f64()?;
            let utc = FixedOffset::east_opt(0)?;
            utc.timestamp_millis_opt(ms as i64).single()
        }
        _ => None,
    }
}

/// Epoch milliseconds of UTC midnight of `d` (pandas reads the web's naive
/// dates as UTC).
fn utc_midnight_ms(d: NaiveDate) -> i64 {
    d.and_hms_opt(0, 0, 0)
        .map(|t| t.and_utc().timestamp_millis())
        .unwrap_or(0)
}

/// web `_filter_candles_by_date`: keep candles in `[start, end + 1 day)`
/// (UTC midnights), rewriting the timestamp to epoch milliseconds.
pub fn filter_by_date(
    candles: Vec<Vec<Value>>,
    start: NaiveDate,
    end: NaiveDate,
) -> Vec<Vec<Value>> {
    let lo = utc_midnight_ms(start) as f64;
    let hi = utc_midnight_ms(end + Duration::days(1)) as f64;
    candles
        .into_iter()
        .filter_map(|mut c| {
            let ts = match c.first()? {
                Value::String(_) => candle_time(c.first()?)?.timestamp_millis() as f64,
                Value::Number(n) => {
                    let v = n.as_f64()?;
                    if v < 1e12 {
                        v * 1000.0
                    } else {
                        v
                    }
                }
                _ => return None,
            };
            if lo <= ts && ts < hi {
                c[0] = serde_json::json!(ts);
                Some(c)
            } else {
                None
            }
        })
        .collect()
}

fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Raw candles -> candles. Daily bars keep only their calendar date (in
/// the candle's own offset, IST) and land on UTC midnight of it, as the
/// web's `interval == "D"` normalisation does; everything else is the
/// instant in epoch seconds.
pub fn parse_candles(rows: &[Vec<Value>], daily: bool) -> Vec<Candle> {
    rows.iter()
        .filter_map(|r| {
            let t = candle_time(r.first()?)?;
            let timestamp = if daily {
                utc_midnight_ms(t.date_naive()) / 1000
            } else {
                t.timestamp()
            };
            Some(Candle {
                timestamp,
                open: num(r.get(1)),
                high: num(r.get(2)),
                low: num(r.get(3)),
                close: num(r.get(4)),
                volume: num(r.get(5)) as i64,
                oi: num(r.get(6)) as i64,
            })
        })
        .collect()
}

/// Whether a raw candle falls on `day` (its own offset's calendar date).
fn is_on(c: &[Value], day: NaiveDate) -> bool {
    c.first()
        .and_then(candle_time)
        .is_some_and(|t| t.date_naive() == day)
}

/// The synthetic daily bar for today from a quote (web: IST midnight, open /
/// high / low falling back to LTP only when absent), or `None` when the
/// quote is empty or identical to the last bar (a stale quote).
pub fn today_candle(q: &Quote, today: NaiveDate, existing: &[Vec<Value>]) -> Option<Vec<Value>> {
    if q.ltp <= 0.0 {
        return None;
    }
    if let Some(last) = existing.iter().max_by_key(|c| {
        c.first()
            .and_then(candle_time)
            .map(|t| t.timestamp_millis())
    }) {
        let same = num(last.get(1)) == q.open
            && num(last.get(2)) == q.high
            && num(last.get(3)) == q.low
            && num(last.get(4)) == q.ltp
            && num(last.get(5)) as i64 == q.volume;
        if same {
            tracing::warn!("Upstox quote matches the last daily candle; skipping today's bar");
            return None;
        }
    }
    let midnight = today
        .and_hms_opt(0, 0, 0)?
        .and_local_timezone(Kolkata)
        .single()?;
    Some(vec![
        Value::String(midnight.to_rfc3339()),
        serde_json::json!(q.open),
        serde_json::json!(q.high),
        serde_json::json!(q.low),
        serde_json::json!(q.ltp),
        serde_json::json!(q.volume),
        serde_json::json!(q.oi),
    ])
}

async fn candles_at(b: &UpstoxBroker, auth: &AuthToken, path: &str) -> Result<Vec<Vec<Value>>> {
    let data = b
        .call(Method::GET, &b.api(path), auth, None, Category::Standard)
        .await?;
    Ok(data
        .get("candles")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
        .unwrap_or_default())
}

#[allow(clippy::too_many_arguments)]
async fn fetch_chunk(
    b: &UpstoxBroker,
    auth: &AuthToken,
    key: &QuoteKey,
    ik: &str,
    unit: &str,
    n: u32,
    start: NaiveDate,
    end: NaiveDate,
    interval: &str,
    today: NaiveDate,
) -> Result<Vec<Candle>> {
    let enc = urlencoding::encode(ik).into_owned();
    let mut all: Vec<Vec<Value>> = Vec::new();
    if matches!(unit, "minutes" | "hours") && end == today {
        let path = format!("/v3/historical-candle/intraday/{}/{}/{}", enc, unit, n);
        match candles_at(b, auth, &path).await {
            Ok(c) => all.extend(filter_by_date(c, start, end)),
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => tracing::debug!("Upstox intraday candles failed: {}", e.code()),
        }
    }
    if all.is_empty() || start < today {
        // Path order is {to}/{from}; the other order returns no data.
        let path = format!(
            "/v3/historical-candle/{}/{}/{}/{}/{}",
            enc,
            unit,
            n,
            end.format("%Y-%m-%d"),
            start.format("%Y-%m-%d")
        );
        match candles_at(b, auth, &path).await {
            Ok(c) => all.extend(c),
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => tracing::warn!("Upstox history chunk failed: {}", e.code()),
        }
    }
    if unit == "days" && interval == "D" && start <= today && today <= end {
        let found = all.iter().any(|c| is_on(c, today));
        if !found {
            match get_quote(b, auth, key).await {
                Ok(q) => {
                    if let Some(c) = today_candle(&q, today, &all) {
                        all.push(c);
                    }
                }
                Err(e) => tracing::info!("No quote for today's daily candle: {}", e.code()),
            }
        }
    }
    Ok(parse_candles(&all, interval == "D"))
}

pub async fn get_history(
    b: &UpstoxBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let (unit, n) = upstox_interval(&req.interval)?;
    let key = normalise_key(&req.key);
    let ik = instrument_key(b, &key.symbol, &key.exchange).ok_or_else(|| not_found(&key))?;
    let today = today_ist();
    let mut out = Vec::new();
    for (from, to) in chunks(req.start, req.end, chunk_days(unit, n)) {
        out.extend(fetch_chunk(b, auth, &key, &ik, unit, n, from, to, &req.interval, today).await?);
    }
    Ok(sort_dedupe(out))
}
