//! Quotes, multiquotes, depth and history (web `api/data.py`).
//!
//! Quotes, depth and multiquotes all come from `POST /v2/marketfeed/quote`
//! with a `{segment: [securityId, ...]}` body (ids as integers in the
//! request, string keys in the answer). History uses `/v2/charts/historical`
//! for `D` and `/v2/charts/intraday` otherwise, with the web's weekend
//! adjustment, 90-day intraday chunks (5 on the sandbox) with per-chunk
//! retries and the interior-gap check.
//!
//! Timestamps follow the platform convention (zerodha is canonical):
//! intraday candles are the true epoch Dhan sends; daily candles land on the
//! IST session date at 00:00 UTC (the web's `+ 19800` on an IST host).

use super::mapping::data_segment;
use super::{Category, DhanBroker, DhanSession, TIMEFRAME_MAP};
use crate::brokers::common::history::IST_OFFSET_SECS;
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{Datelike, Duration as Days, NaiveDate, Weekday};
use reqwest::Method;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// `/v2/marketfeed/quote` instruments per request (web `BATCH_SIZE`).
pub const QUOTE_BATCH: usize = 1000;
/// Intraday chunk length in days (web `_get_intraday_chunks`).
pub const INTRADAY_CHUNK_DAYS: i64 = 90;
/// Attempts per intraday chunk (web `CHUNK_MAX_RETRIES`).
pub const CHUNK_MAX_RETRIES: u32 = 3;

/// Lenient number from a JSON value (number, numeric string, null).
pub(crate) fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn int(v: Option<&Value>) -> i64 {
    num(v) as i64
}

pub(crate) fn lookup(b: &DhanBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

fn segment(key: &QuoteKey) -> Result<&'static str> {
    data_segment(&key.exchange).ok_or_else(|| {
        AppError::Validation(format!(
            "Dhan has no market data for the {} exchange.",
            key.exchange
        ))
    })
}

/// First error code of a `status: failed` body (`{"data": {"805": "..."}}`).
fn failed_code(v: &Value) -> Option<String> {
    if v.get("status").and_then(Value::as_str) != Some("failed") {
        return None;
    }
    v.get("data")
        .and_then(Value::as_object)
        .and_then(|d| d.keys().next().cloned())
        .or_else(|| Some("unknown".into()))
}

/// A market-data call (web `data.get_api_response`): `client-id` is
/// required, error 805 (rate limit) is retried three times with exponential
/// back-off, any other Dhan error is raised.
pub(crate) async fn data_call(
    b: &DhanBroker,
    s: &DhanSession,
    path: &str,
    body: &Value,
) -> Result<Value> {
    let cid = s.require_client_id()?.to_string();
    let category = if path.starts_with("/v2/marketfeed") {
        Category::Quote
    } else {
        Category::Data
    };
    let mut retry = 0u32;
    loop {
        let (status, v) = b
            .send(Method::POST, path, s, Some(body), Some(&cid), category)
            .await?;
        if failed_code(&v).as_deref() == Some("805") && retry < 3 {
            tokio::time::sleep(b.retry_base * (1 << retry)).await;
            retry += 1;
            continue;
        }
        if let Some(e) = super::dhan_error(&v) {
            tracing::warn!(status = status.as_u16(), "Dhan data call {} failed", path);
            return Err(e);
        }
        if !status.is_success() {
            return Err(if status.as_u16() == 401 || status.as_u16() == 403 {
                super::session_expired()
            } else {
                AppError::Broker("Dhan did not return market data. Try again shortly.".into())
            });
        }
        return Ok(v);
    }
}

fn security_id_int(row: &SymToken) -> Result<i64> {
    row.token.trim().parse::<i64>().map_err(|_| {
        AppError::Broker(format!(
            "The master contract has no Dhan security id for {}. Download the master contract again.",
            row.symbol
        ))
    })
}

/// `data[segment][securityId]` of a quote answer, when it carries anything.
pub fn quote_entry<'a>(v: &'a Value, segment: &str, security_id: &str) -> Option<&'a Value> {
    v.get("data")?
        .get(segment)?
        .get(security_id.trim())
        .filter(|q| q.as_object().is_some_and(|o| !o.is_empty()))
}

fn ltp_of(q: &Value) -> f64 {
    let a = num(q.get("last_price"));
    if a != 0.0 {
        a
    } else {
        num(q.get("lastPrice"))
    }
}

fn oi_of(q: &Value) -> i64 {
    let a = int(q.get("oi"));
    if a != 0 {
        a
    } else {
        int(q.get("open_interest"))
    }
}

fn levels(q: &Value, side: &str) -> Vec<Value> {
    q.get("depth")
        .and_then(|d| d.get(side))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Web quote fields from one `/marketfeed/quote` entry.
pub fn to_quote(key: &QuoteKey, q: &Value) -> Quote {
    let ohlc = q.get("ohlc").cloned().unwrap_or(Value::Null);
    let buy = levels(q, "buy");
    let sell = levels(q, "sell");
    let mut quote = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: ltp_of(q),
        open: num(ohlc.get("open")),
        high: num(ohlc.get("high")),
        low: num(ohlc.get("low")),
        close: num(ohlc.get("close")),
        volume: int(q.get("volume")),
        bid: buy.first().map(|l| num(l.get("price"))).unwrap_or(0.0),
        ask: sell.first().map(|l| num(l.get("price"))).unwrap_or(0.0),
        bid_qty: buy.first().map(|l| int(l.get("quantity"))).unwrap_or(0),
        ask_qty: sell.first().map(|l| int(l.get("quantity"))).unwrap_or(0),
        oi: oi_of(q),
        ..Default::default()
    };
    if quote.close > 0.0 {
        quote.change = round2(quote.ltp - quote.close);
        quote.change_percent = round2((quote.ltp - quote.close) / quote.close * 100.0);
    }
    quote
}

/// Web `get_depth` from one entry: exactly five levels per side, padded with
/// zeros; totals are the sums of those five levels (not Dhan's totals).
pub fn to_depth(key: &QuoteKey, q: &Value) -> MarketDepth {
    let pad = |side: &[Value]| -> Vec<DepthLevel> {
        (0..5)
            .map(|i| {
                side.get(i)
                    .map(|l| DepthLevel {
                        price: num(l.get("price")),
                        quantity: int(l.get("quantity")),
                        orders: int(l.get("orders")),
                    })
                    .unwrap_or_default()
            })
            .collect()
    };
    let bids = pad(&levels(q, "buy"));
    let asks = pad(&levels(q, "sell"));
    let ohlc = q.get("ohlc").cloned().unwrap_or(Value::Null);
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        total_buy_qty: bids.iter().map(|l| l.quantity).sum(),
        total_sell_qty: asks.iter().map(|l| l.quantity).sum(),
        bids,
        asks,
        ltp: num(q.get("last_price")),
        ltq: int(q.get("last_quantity")),
        open: num(ohlc.get("open")),
        high: num(ohlc.get("high")),
        low: num(ohlc.get("low")),
        prev_close: num(ohlc.get("close")),
        volume: int(q.get("volume")),
        oi: int(q.get("oi")),
    }
}

fn empty_depth(key: &QuoteKey) -> MarketDepth {
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: vec![DepthLevel::default(); 5],
        asks: vec![DepthLevel::default(); 5],
        ..Default::default()
    }
}

async fn fetch_one(b: &DhanBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Option<Value>> {
    let s = DhanSession::parse(auth)?;
    let row = lookup(b, key)?;
    let seg = segment(key)?;
    let body = json!({ seg: [security_id_int(&row)?] });
    let v = data_call(b, &s, "/v2/marketfeed/quote", &body).await?;
    Ok(quote_entry(&v, seg, &row.token).cloned())
}

pub async fn get_quote(b: &DhanBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    if b.is_sandbox() {
        return crate::brokers::dhan_sandbox::chart_quote(b, auth, key).await;
    }
    // web: an empty answer is an all-zero quote, not an error.
    Ok(match fetch_one(b, auth, key).await? {
        Some(q) => to_quote(key, &q),
        None => {
            tracing::warn!("Dhan returned no quote for {}:{}", key.exchange, key.symbol);
            Quote {
                symbol: key.symbol.clone(),
                exchange: key.exchange.clone(),
                ..Default::default()
            }
        }
    })
}

pub async fn get_market_depth(
    b: &DhanBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    if b.is_sandbox() {
        // web: the sandbox has no book; OHLC from the chart, zero levels.
        let q = crate::brokers::dhan_sandbox::chart_quote(b, auth, key).await?;
        let mut d = empty_depth(key);
        d.ltp = q.ltp;
        d.open = q.open;
        d.high = q.high;
        d.low = q.low;
        d.volume = q.volume;
        d.oi = q.oi;
        return Ok(d);
    }
    Ok(match fetch_one(b, auth, key).await? {
        Some(q) => to_depth(key, &q),
        None => empty_depth(key),
    })
}

/// `{segment: [ids]}` request bodies of at most `QUOTE_BATCH` instruments.
pub fn multiquote_bodies(wanted: &[(&'static str, i64)]) -> Vec<Value> {
    wanted
        .chunks(QUOTE_BATCH)
        .map(|batch| {
            let mut m: Map<String, Value> = Map::new();
            for (seg, id) in batch {
                let e = m.entry(seg.to_string()).or_insert_with(|| json!([]));
                if let Some(a) = e.as_array_mut() {
                    a.push(json!(id));
                }
            }
            Value::Object(m)
        })
        .collect()
}

/// A resolved multiquote key: segment, security id text and number.
type Resolved = (&'static str, String, i64);

pub async fn get_multiquotes(
    b: &DhanBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    if b.is_sandbox() {
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            out.push(match get_quote(b, auth, k).await {
                Ok(q) => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: Some(q),
                    error: None,
                },
                Err(e) => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some(e.client_message()),
                },
            });
        }
        return Ok(out);
    }
    let s = DhanSession::parse(auth)?;
    // Resolve first; unresolved symbols become per-symbol errors.
    let resolved: Vec<(QuoteKey, Option<Resolved>)> = keys
        .iter()
        .map(|k| {
            let r = b
                .resolver()
                .by_symbol(&k.exchange, &k.symbol)
                .and_then(|row| {
                    let seg = data_segment(&k.exchange)?;
                    let id = row.token.trim().parse::<i64>().ok()?;
                    Some((seg, row.token.trim().to_string(), id))
                });
            (k.clone(), r)
        })
        .collect();
    let mut wanted: Vec<(&'static str, i64)> = resolved
        .iter()
        .filter_map(|(_, r)| r.as_ref().map(|(seg, _, id)| (*seg, *id)))
        .collect();
    wanted.sort();
    wanted.dedup();
    let mut answers: HashMap<(String, String), Value> = HashMap::new();
    for body in multiquote_bodies(&wanted) {
        // The quote pacer spaces batches at Dhan's 1 request per second.
        let v = data_call(b, &s, "/v2/marketfeed/quote", &body).await?;
        if let Some(Value::Object(segs)) = v.get("data") {
            for (seg, entries) in segs {
                if let Some(entries) = entries.as_object() {
                    for (id, q) in entries {
                        answers.insert((seg.clone(), id.clone()), q.clone());
                    }
                }
            }
        }
    }
    Ok(resolved
        .into_iter()
        .map(|(k, r)| {
            let err = |m: &str| QuoteResult {
                symbol: k.symbol.clone(),
                exchange: k.exchange.clone(),
                data: None,
                error: Some(m.to_string()),
            };
            match r {
                None => err("Could not resolve broker symbol"),
                Some((seg, id, _)) => match answers
                    .get(&(seg.to_string(), id))
                    .filter(|q| q.as_object().is_some_and(|o| !o.is_empty()))
                {
                    Some(q) => QuoteResult {
                        data: Some(to_quote(&k, q)),
                        symbol: k.symbol,
                        exchange: k.exchange,
                        error: None,
                    },
                    None => err("No quote data available"),
                },
            }
        })
        .collect())
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Dhan resolution for an OpenAlgo interval.
pub fn dhan_interval(interval: &str) -> Result<&'static str> {
    TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let list: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Unsupported interval '{}'. Supported intervals are: {}",
                interval,
                list.join(", ")
            ))
        })
}

const INDEX_NAMES: &[&str] = &[
    "NIFTY",
    "NIFTYNXT50",
    "FINNIFTY",
    "BANKNIFTY",
    "MIDCPNIFTY",
    "INDIAVIX",
    "SENSEX",
    "BANKEX",
    "SENSEX50",
];
const MCX_INDEX_UNDERLYINGS: &[&str] = &["MCXBULLDEX", "MCXMETLDEX", "MCXENRGDEX"];

/// web `_get_instrument_type`.
pub fn instrument_type(exchange: &str, symbol: &str) -> Result<&'static str> {
    let option = symbol.ends_with("CE") || symbol.ends_with("PE");
    let index = INDEX_NAMES.iter().any(|i| symbol.contains(i));
    Ok(match exchange {
        "NSE" | "BSE" => "EQUITY",
        "NSE_INDEX" | "BSE_INDEX" => "INDEX",
        "NFO" | "BFO" => match (option, index) {
            (true, true) => "OPTIDX",
            (true, false) => "OPTSTK",
            (false, true) => "FUTIDX",
            (false, false) => "FUTSTK",
        },
        "NCO" => {
            if option {
                "OPTFUT"
            } else {
                "FUTCOM"
            }
        }
        "MCX" => {
            let idx = MCX_INDEX_UNDERLYINGS.iter().any(|u| symbol.starts_with(u));
            match (option, idx) {
                (true, true) => "OPTIDX",
                (true, false) => "OPTFUT",
                (false, true) => "FUTIDX",
                (false, false) => "FUTCOM",
            }
        }
        "CDS" | "BCD" => {
            if option {
                "OPTCUR"
            } else {
                "FUTCUR"
            }
        }
        other => {
            return Err(AppError::Validation(format!(
                "Dhan has no historical data for the {} exchange.",
                other
            )))
        }
    })
}

fn is_weekday(d: NaiveDate) -> bool {
    !matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
}

/// web `_adjust_dates`: a weekend start moves to Monday, a weekend end back
/// to Friday.
pub fn adjust_dates(mut start: NaiveDate, mut end: NaiveDate) -> (NaiveDate, NaiveDate) {
    while !is_weekday(start) {
        start += Days::days(1);
    }
    while !is_weekday(end) {
        end -= Days::days(1);
    }
    (start, end)
}

/// web `_get_intraday_chunks`: `[start, min(start + days, end)]`, the next
/// chunk starting on the previous chunk's end day (duplicates are dropped
/// after the merge).
pub fn intraday_chunks(start: NaiveDate, end: NaiveDate, days: i64) -> Vec<(NaiveDate, NaiveDate)> {
    let mut out = Vec::new();
    let mut cur = start;
    while cur < end {
        let ce = (cur + Days::days(days)).min(end);
        out.push((cur, ce));
        cur = ce;
    }
    out
}

/// A daily candle on its IST session date, 00:00 UTC (web
/// `_convert_timestamp_to_ist(is_daily=True)` on an IST host).
pub fn daily_timestamp(ts: i64) -> i64 {
    (ts + IST_OFFSET_SECS).div_euclid(86_400) * 86_400
}

/// Parallel arrays of a chart answer -> candles.
pub fn parse_chart(v: &Value, daily: bool) -> Vec<Candle> {
    let arr = |k: &str| {
        v.get(k)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let (ts, o, h, l, c, vol, oi) = (
        arr("timestamp"),
        arr("open"),
        arr("high"),
        arr("low"),
        arr("close"),
        arr("volume"),
        arr("open_interest"),
    );
    ts.iter()
        .enumerate()
        .map(|(i, t)| {
            let t = num(Some(t)) as i64;
            Candle {
                timestamp: if daily { daily_timestamp(t) } else { t },
                open: num(o.get(i)),
                high: num(h.get(i)),
                low: num(l.get(i)),
                close: num(c.get(i)),
                volume: num(vol.get(i)) as i64,
                oi: num(oi.get(i)) as i64,
            }
        })
        .collect()
}

fn ymd(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn chart_body(
    row: &SymToken,
    seg: &str,
    instrument: &str,
    interval: Option<&str>,
    from: NaiveDate,
    to: NaiveDate,
) -> Value {
    let mut m = Map::new();
    m.insert("securityId".into(), json!(row.token.trim()));
    m.insert("exchangeSegment".into(), json!(seg));
    m.insert("instrument".into(), json!(instrument));
    if let Some(i) = interval {
        m.insert("interval".into(), json!(i));
    }
    m.insert("fromDate".into(), json!(ymd(from)));
    m.insert("toDate".into(), json!(ymd(to)));
    m.insert("oi".into(), json!(true));
    m.insert("expiryCode".into(), json!(0));
    Value::Object(m)
}

/// The IST calendar date now.
pub(crate) fn ist_today() -> NaiveDate {
    (chrono::Utc::now() + chrono::Duration::seconds(IST_OFFSET_SECS)).date_naive()
}

pub async fn get_history(
    b: &DhanBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let resolution = dhan_interval(&req.interval)?;
    let s = DhanSession::parse(auth)?;
    let (start, mut end) = adjust_dates(req.start, req.end);
    if start > end {
        return Ok(Vec::new());
    }
    if start == end {
        end = start + Days::days(1);
    }
    let row = lookup(b, &req.key)?;
    let seg = segment(&req.key)?;
    let instrument = instrument_type(&req.key.exchange, &req.key.symbol)?;
    let mut candles = Vec::new();

    if resolution == "D" {
        // Daily: one request, the end day inclusive (toDate = end + 1).
        let body = chart_body(&row, seg, instrument, None, start, end + Days::days(1));
        let v = data_call(b, &s, "/v2/charts/historical", &body).await?;
        candles.extend(parse_chart(&v, true));
        let today = ist_today();
        if start <= today && today <= end {
            // web: today's bar from the quote when it has a price.
            match get_quote(b, auth, &req.key).await {
                Ok(q) if q.ltp > 0.0 => candles.push(Candle {
                    timestamp: daily_timestamp(
                        today
                            .and_hms_opt(0, 0, 0)
                            .map(|d| d.and_utc().timestamp())
                            .unwrap_or(0),
                    ),
                    open: q.open,
                    high: q.high,
                    low: q.low,
                    close: q.ltp,
                    volume: q.volume,
                    oi: q.oi,
                }),
                Ok(_) => {}
                Err(e) => tracing::warn!("Dhan today's candle from quotes failed: {}", e.code()),
            }
        }
    } else if start == end - Days::days(1) {
        // One session: a single request; a failure yields no candles (web).
        let body = chart_body(&row, seg, instrument, Some(resolution), start, end);
        match data_call(b, &s, "/v2/charts/intraday", &body).await {
            Ok(v) => candles.extend(parse_chart(&v, false)),
            Err(e) => {
                tracing::warn!("Dhan intraday history failed: {}", e.code());
            }
        }
    } else {
        let days = if b.is_sandbox() {
            crate::brokers::dhan_sandbox::INTRADAY_CHUNK_DAYS
        } else {
            INTRADAY_CHUNK_DAYS
        };
        let mut counts: Vec<(NaiveDate, NaiveDate, usize)> = Vec::new();
        for (cs, ce) in intraday_chunks(start, end, days) {
            // Skip only a chunk with no weekday in it at all.
            let mut d = cs;
            let mut any = false;
            while d <= ce {
                if is_weekday(d) {
                    any = true;
                    break;
                }
                d += Days::days(1);
            }
            if !any {
                continue;
            }
            let body = chart_body(&row, seg, instrument, Some(resolution), cs, ce);
            let mut last_err: Option<AppError> = None;
            let mut got = 0usize;
            for attempt in 0..CHUNK_MAX_RETRIES {
                match data_call(b, &s, "/v2/charts/intraday", &body).await {
                    Ok(v) => {
                        let rows = parse_chart(&v, false);
                        // An empty 200 over a trading window is suspect:
                        // retry it before accepting.
                        if rows.is_empty() && attempt < CHUNK_MAX_RETRIES - 1 {
                            tokio::time::sleep(b.retry_base * (1 << attempt)).await;
                            continue;
                        }
                        got = rows.len();
                        candles.extend(rows);
                        last_err = None;
                        break;
                    }
                    Err(e) => {
                        if matches!(e, AppError::Auth(_) | AppError::Validation(_)) {
                            return Err(e);
                        }
                        if attempt < CHUNK_MAX_RETRIES - 1 {
                            tokio::time::sleep(b.retry_base * (1 << attempt)).await;
                        }
                        last_err = Some(e);
                    }
                }
            }
            if let Some(e) = last_err {
                tracing::warn!(
                    "Dhan history chunk {} to {} failed after {} attempts: {}",
                    cs,
                    ce,
                    CHUNK_MAX_RETRIES,
                    e.code()
                );
                return Err(AppError::Broker(format!(
                    "Dhan did not return history for {} to {}. Try again shortly.",
                    cs, ce
                )));
            }
            counts.push((cs, ce, got));
        }
        // An empty chunk between chunks with data is a bad answer, not a
        // holiday: refuse to return a history with a hole in it.
        let nonempty: Vec<usize> = counts
            .iter()
            .enumerate()
            .filter(|(_, c)| c.2 > 0)
            .map(|(i, _)| i)
            .collect();
        if let (Some(first), Some(last)) = (nonempty.first(), nonempty.last()) {
            let gaps: Vec<String> = counts
                .iter()
                .enumerate()
                .filter(|(i, c)| c.2 == 0 && first < i && i < last)
                .map(|(_, c)| format!("{} to {}", c.0, c.1))
                .collect();
            if !gaps.is_empty() {
                tracing::warn!("Dhan returned empty interior chunks: {}", gaps.join(", "));
                return Err(AppError::Broker(format!(
                    "Dhan returned no data for {} while the surrounding dates had data. Retry the download.",
                    gaps.join(", ")
                )));
            }
        }
    }
    Ok(crate::brokers::common::history::sort_dedupe(candles))
}
