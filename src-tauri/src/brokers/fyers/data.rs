//! Quotes, multiquotes, depth and history (web `api/data.py`).

use super::mapping::num;
use super::{FyersBroker, TIMEFRAME_MAP};
use crate::brokers::common::de::{f64_lenient, i64_lenient};
use crate::brokers::common::history::sort_dedupe;
use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{Duration as Days, NaiveDate};
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

/// web `get_multiquotes` `BATCH_SIZE` (Fyers `/data/quotes` limit).
pub const QUOTE_BATCH: usize = 50;
/// web `OI_THRESHOLD`: above this many symbols no per-symbol OI is fetched.
pub const OI_THRESHOLD: usize = 100;
/// web: history chunk retries before a chunk is skipped.
const HISTORY_RETRIES: u32 = 3;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FyersDepthLevel {
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub volume: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub ord: i64,
}

/// One instrument of `/data/depth` (`d[brsymbol]`). Fyers names the ask
/// side `ask` (singular).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FyersDepth {
    pub bids: Vec<FyersDepthLevel>,
    pub ask: Vec<FyersDepthLevel>,
    #[serde(deserialize_with = "f64_lenient")]
    pub o: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub h: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub l: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub c: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub ltp: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub ltq: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub v: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub oi: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub totalbuyqty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub totalsellqty: i64,
}

/// The `v` object of one `/data/quotes` entry.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FyersQuoteValues {
    #[serde(deserialize_with = "f64_lenient")]
    pub bid: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub ask: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub open_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub high_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub low_price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub lp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub prev_close_price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub volume: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub ch: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub chp: f64,
}

fn with_change(mut q: Quote) -> Quote {
    if q.close > 0.0 && q.ltp > 0.0 {
        q.change = round2(q.ltp - q.close);
        q.change_percent = round2((q.ltp - q.close) / q.close * 100.0);
    }
    q
}

/// web `get_quotes` (from `/data/depth` so OI is included).
pub fn depth_to_quote(key: &QuoteKey, d: &FyersDepth) -> Quote {
    let bid = d.bids.first().cloned().unwrap_or_default();
    let ask = d.ask.first().cloned().unwrap_or_default();
    with_change(Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: d.ltp,
        open: d.o,
        high: d.h,
        low: d.l,
        close: d.c,
        volume: d.v,
        bid: bid.price,
        ask: ask.price,
        bid_qty: bid.volume,
        ask_qty: ask.volume,
        oi: d.oi,
        change: 0.0,
        change_percent: 0.0,
        timestamp: String::new(),
    })
}

/// web `_process_quotes_batch` data fields from one `/data/quotes` entry.
pub fn values_to_quote(key: &QuoteKey, v: &FyersQuoteValues, oi: i64) -> Quote {
    let mut q = with_change(Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: v.lp,
        open: v.open_price,
        high: v.high_price,
        low: v.low_price,
        close: v.prev_close_price,
        volume: v.volume,
        bid: v.bid,
        ask: v.ask,
        bid_qty: 0,
        ask_qty: 0,
        oi,
        change: 0.0,
        change_percent: 0.0,
        timestamp: String::new(),
    });
    if v.ch != 0.0 || v.chp != 0.0 {
        q.change = v.ch;
        q.change_percent = v.chp;
    }
    q
}

/// web `get_depth`: five levels per side padded with zero levels.
pub fn depth_to_book(key: &QuoteKey, d: &FyersDepth) -> MarketDepth {
    let pad = |side: &[FyersDepthLevel]| -> Vec<DepthLevel> {
        (0..5)
            .map(|i| {
                side.get(i)
                    .map(|l| DepthLevel {
                        price: l.price,
                        quantity: l.volume,
                        orders: l.ord,
                    })
                    .unwrap_or_default()
            })
            .collect()
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: pad(&d.bids),
        asks: pad(&d.ask),
        ltp: d.ltp,
        ltq: d.ltq,
        open: d.o,
        high: d.h,
        low: d.l,
        prev_close: d.c,
        volume: d.v,
        oi: d.oi,
        total_buy_qty: d.totalbuyqty,
        total_sell_qty: d.totalsellqty,
    }
}

/// `/data/depth?symbol=<brsymbol>&ohlcv_flag=1` -> `d[brsymbol]`.
pub(crate) async fn fetch_depth(
    b: &FyersBroker,
    auth: &AuthToken,
    brsymbol: &str,
) -> Result<Option<FyersDepth>> {
    let path = format!(
        "/data/depth?symbol={}&ohlcv_flag=1",
        urlencoding::encode(brsymbol)
    );
    let v = b.call(Method::GET, &path, auth, None).await?;
    Ok(v.get("d")
        .and_then(|d| d.get(brsymbol))
        .filter(|x| !x.is_null())
        .and_then(|x| serde_json::from_value(x.clone()).ok()))
}

pub async fn get_quote(b: &FyersBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let row = b.lookup(key)?;
    let d = fetch_depth(b, auth, row.br_symbol())
        .await?
        .ok_or_else(|| {
            AppError::Broker(format!(
                "No quote data available for {}:{}",
                key.exchange, key.symbol
            ))
        })?;
    Ok(depth_to_quote(key, &d))
}

pub async fn get_market_depth(
    b: &FyersBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let row = b.lookup(key)?;
    let d = fetch_depth(b, auth, row.br_symbol())
        .await?
        .ok_or_else(|| {
            AppError::Broker(format!(
                "Fyers returned no market depth for {} {}.",
                key.exchange, key.symbol
            ))
        })?;
    Ok(depth_to_book(key, &d))
}

/// Parse a `/data/quotes` body into `n -> v` for entries with `s == ok`.
pub fn parse_quotes(v: &Value) -> HashMap<String, FyersQuoteValues> {
    let mut out = HashMap::new();
    if let Some(Value::Array(items)) = v.get("d") {
        for item in items {
            if !super::is_ok(item) {
                continue;
            }
            let name = item.get("n").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            if let Some(vals) = item
                .get("v")
                .and_then(|x| serde_json::from_value::<FyersQuoteValues>(x.clone()).ok())
            {
                out.insert(name.to_string(), vals);
            }
        }
    }
    out
}

fn is_fno(exchange: &str) -> bool {
    exchange
        .parse::<Exchange>()
        .map(Exchange::is_derivative)
        .unwrap_or(false)
}

/// OI for one derivative via `/data/depth`; 0 on any failure (web
/// `_fetch_oi_for_symbol`).
async fn oi_for(b: &FyersBroker, auth: &AuthToken, brsymbol: &str) -> i64 {
    match fetch_depth(b, auth, brsymbol).await {
        Ok(Some(d)) => d.oi,
        Ok(None) => 0,
        Err(e) => {
            tracing::debug!("Fyers OI lookup failed for {}: {}", brsymbol, e.code());
            0
        }
    }
}

/// web `get_multiquotes`: `/data/quotes` in batches of 50; OI per
/// derivative via `/data/depth` when the request has at most 100 symbols.
pub async fn get_multiquotes(
    b: &FyersBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let fetch_oi = keys.len() <= OI_THRESHOLD;
    let resolved: Vec<(QuoteKey, Option<SymToken>)> = keys
        .iter()
        .map(|k| (k.clone(), b.resolver().by_symbol(&k.exchange, &k.symbol)))
        .collect();
    let mut quotes: HashMap<String, FyersQuoteValues> = HashMap::new();
    let wanted: Vec<&str> = resolved
        .iter()
        .filter_map(|(_, r)| r.as_ref().map(|r| r.br_symbol()))
        .collect();
    for batch in wanted.chunks(QUOTE_BATCH) {
        let joined = batch.join(",");
        let path = format!("/data/quotes?symbols={}", urlencoding::encode(&joined));
        // The shared pacer spaces batches (web sleeps 0.1 s between them).
        match b.raw(Method::GET, &path, auth, None).await {
            Ok((_, v)) if super::is_ok(&v) => quotes.extend(parse_quotes(&v)),
            Ok((status, v)) => {
                let (code, message) = super::code_message(&v);
                let err = super::fyers_error(status.as_u16(), code, &message);
                if matches!(err, AppError::Auth(_)) {
                    return Err(err);
                }
                tracing::warn!("Fyers quotes batch refused: {}", message);
            }
            Err(e) => return Err(e),
        }
    }
    let mut out = Vec::with_capacity(resolved.len());
    for (k, row) in resolved {
        let Some(row) = row else {
            tracing::warn!(
                "Skipping {} on {}: could not resolve broker symbol",
                k.symbol,
                k.exchange
            );
            out.push(QuoteResult {
                symbol: k.symbol,
                exchange: k.exchange,
                data: None,
                error: Some("Could not resolve broker symbol".into()),
            });
            continue;
        };
        match quotes.get(row.br_symbol()) {
            Some(v) => {
                let oi = if fetch_oi && is_fno(&k.exchange) {
                    oi_for(b, auth, row.br_symbol()).await
                } else {
                    0
                };
                out.push(QuoteResult {
                    data: Some(values_to_quote(&k, v, oi)),
                    symbol: k.symbol,
                    exchange: k.exchange,
                    error: None,
                });
            }
            None => out.push(QuoteResult {
                symbol: k.symbol,
                exchange: k.exchange,
                data: None,
                error: Some("No quote data available".into()),
            }),
        }
    }
    Ok(out)
}

/// Fyers resolution for an OpenAlgo interval (web `timeframe_map`).
pub fn fyers_resolution(interval: &str) -> Result<&'static str> {
    TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            AppError::Validation(format!(
                "Timeframe {} is not supported by Fyers. Supported timeframes are: seconds 5s, 10s, 15s, 30s, 45s; minutes 1m, 2m, 3m, 5m, 10m, 15m, 20m, 30m; hours 1h, 2h, 4h; daily D.",
                interval
            ))
        })
}

/// web chunk size: 300 days daily, 25 days for seconds, 60 days otherwise.
pub fn chunk_days(resolution: &str) -> i64 {
    if resolution == "1D" {
        300
    } else if resolution.ends_with('S') {
        25
    } else {
        60
    }
}

/// OI comes with the candles only for derivatives (web `enable_oi`).
pub fn wants_oi(exchange: &str) -> bool {
    matches!(exchange, "NFO" | "BFO" | "MCX" | "CDS")
}

/// web: clamp the end to today, seconds data to the last 30 days.
pub fn effective_range(
    resolution: &str,
    start: NaiveDate,
    end: NaiveDate,
    today: NaiveDate,
) -> Result<(NaiveDate, NaiveDate)> {
    let end = end.min(today);
    let mut start = start;
    if start > end {
        return Err(AppError::Validation(format!(
            "Start date {} cannot be after end date {}.",
            start, end
        )));
    }
    if resolution.ends_with('S') {
        let floor = today - Days::days(30);
        if start < floor {
            tracing::warn!(
                "Fyers seconds data covers the last 30 days; start moved from {} to {}",
                start,
                floor
            );
            start = floor;
        }
    }
    Ok((start, end))
}

/// Candle rows `[ts, o, h, l, c, v(, oi)]`; OI read only when requested and
/// the row has seven columns (web).
pub fn parse_candles(rows: &[Vec<Value>], with_oi: bool) -> Vec<Candle> {
    rows.iter()
        .filter_map(|r| {
            let ts = r.first().map(num)? as i64;
            Some(Candle {
                timestamp: ts,
                open: r.get(1).map(num).unwrap_or(0.0),
                high: r.get(2).map(num).unwrap_or(0.0),
                low: r.get(3).map(num).unwrap_or(0.0),
                close: r.get(4).map(num).unwrap_or(0.0),
                volume: r.get(5).map(num).unwrap_or(0.0) as i64,
                oi: if with_oi && r.len() == 7 {
                    r.get(6).map(num).unwrap_or(0.0) as i64
                } else {
                    0
                },
            })
        })
        .collect()
}

/// Today in IST (Fyers dates are exchange dates).
fn today_ist() -> NaiveDate {
    (chrono::Utc::now() + chrono::Duration::minutes(330)).date_naive()
}

pub async fn get_history(
    b: &FyersBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let resolution = fyers_resolution(&req.interval)?;
    let row = b.lookup(&req.key)?;
    let (start, end) = effective_range(resolution, req.start, req.end, today_ist())?;
    let with_oi = wants_oi(&req.key.exchange);
    let encoded = urlencoding::encode(row.br_symbol()).into_owned();
    let mut out = Vec::new();
    for (from, to) in crate::brokers::common::history::chunks(start, end, chunk_days(resolution)) {
        let mut path = format!(
            "/data/history?symbol={}&resolution={}&date_format=1&range_from={}&range_to={}&cont_flag=1",
            encoded,
            resolution,
            from.format("%Y-%m-%d"),
            to.format("%Y-%m-%d")
        );
        if with_oi {
            path.push_str("&oi_flag=1");
        }
        let mut attempt = 0;
        loop {
            match b.call(Method::GET, &path, auth, None).await {
                Ok(v) => {
                    let rows: Vec<Vec<Value>> = v
                        .get("candles")
                        .and_then(|c| serde_json::from_value(c.clone()).ok())
                        .unwrap_or_default();
                    out.extend(parse_candles(&rows, with_oi));
                    break;
                }
                // An expired session will not recover by retrying.
                Err(e @ AppError::Auth(_)) => return Err(e),
                Err(e) if attempt < HISTORY_RETRIES => {
                    attempt += 1;
                    tracing::debug!(
                        "Fyers history chunk {}..{} failed ({}); retry {}",
                        from,
                        to,
                        e.code(),
                        attempt
                    );
                    tokio::time::sleep(b.retry_base.saturating_mul(2 * attempt)).await;
                }
                Err(e) => {
                    // web: after the retries the chunk is skipped.
                    tracing::error!("Fyers history chunk {}..{} skipped: {}", from, to, e.code());
                    break;
                }
            }
        }
    }
    Ok(sort_dedupe(out))
}
