//! Quotes, multiquotes, depth and history (web `api/data.py`).

use super::mapping::{instrument_token, kite_quote_exchange};
use super::{Body, Category, ZerodhaBroker, TIMEFRAME_MAP};
use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::history::{chunks, parse_iso_epoch, sort_dedupe, IST_OFFSET_SECS};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

/// Kite `/quote` batch limit.
pub const QUOTE_BATCH: usize = 500;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteOhlc {
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
pub struct KiteDepthLevel {
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    pub orders: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteDepth {
    pub buy: Vec<KiteDepthLevel>,
    pub sell: Vec<KiteDepthLevel>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteQuote {
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
    pub ohlc: KiteOhlc,
    pub depth: Option<KiteDepth>,
}

/// Web quote fields from one Kite `/quote` entry.
pub fn to_quote(key: &QuoteKey, q: &KiteQuote) -> Quote {
    let (bid, ask) = match &q.depth {
        Some(d) => (d.buy.first().cloned(), d.sell.first().cloned()),
        None => (None, None),
    };
    let bid = bid.unwrap_or_default();
    let ask = ask.unwrap_or_default();
    let mut quote = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: q.last_price,
        open: q.ohlc.open,
        high: q.ohlc.high,
        low: q.ohlc.low,
        close: q.ohlc.close,
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
        quote.change = crate::brokers::common::streaming::round2(quote.ltp - quote.close);
        quote.change_percent = crate::brokers::common::streaming::round2(
            (quote.ltp - quote.close) / quote.close * 100.0,
        );
    }
    quote
}

/// Web `get_market_depth` from one Kite `/quote` entry: five levels per side,
/// padded with zero levels; totals are the sums of those levels.
pub fn to_depth(key: &QuoteKey, q: &KiteQuote) -> MarketDepth {
    let d = q.depth.clone().unwrap_or_default();
    let pad = |side: &[KiteDepthLevel]| -> Vec<DepthLevel> {
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
        open: q.ohlc.open,
        high: q.ohlc.high,
        low: q.ohlc.low,
        prev_close: q.ohlc.close,
        volume: q.volume,
        oi: q.oi,
        total_buy_qty: d.buy.iter().map(|l| l.quantity).sum(),
        total_sell_qty: d.sell.iter().map(|l| l.quantity).sum(),
    }
}

fn lookup(b: &ZerodhaBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

/// `EXCHANGE:brsymbol` as Kite keys a quote.
pub fn instrument_key(row: &SymToken) -> String {
    format!(
        "{}:{}",
        kite_quote_exchange(&row.exchange, row.br_exchange()),
        row.br_symbol()
    )
}

async fn fetch_quotes(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    instruments: &[String],
) -> Result<HashMap<String, KiteQuote>> {
    let query: Vec<String> = instruments
        .iter()
        .map(|i| format!("i={}", urlencoding::encode(i)))
        .collect();
    let env = b
        .call_raw::<HashMap<String, KiteQuote>>(
            Method::GET,
            &format!("/quote?{}", query.join("&")),
            auth,
            Body::None,
            Category::Quote,
        )
        .await?;
    Ok(env.data.unwrap_or_default())
}

pub async fn get_quote(b: &ZerodhaBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let row = lookup(b, key)?;
    let ik = instrument_key(&row);
    let mut data = fetch_quotes(b, auth, std::slice::from_ref(&ik)).await?;
    let q = data.remove(&ik).ok_or_else(|| {
        AppError::Broker(format!(
            "Zerodha returned no quote for {} {}.",
            key.exchange, key.symbol
        ))
    })?;
    Ok(to_quote(key, &q))
}

pub async fn get_multiquotes(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    // Resolve first; unresolved symbols become per-symbol errors.
    let resolved: Vec<(QuoteKey, Option<String>)> = keys
        .iter()
        .map(|k| {
            let ik = b
                .resolver()
                .by_symbol(&k.exchange, &k.symbol)
                .map(|r| instrument_key(&r));
            (k.clone(), ik)
        })
        .collect();
    let mut wanted: Vec<String> = resolved.iter().filter_map(|(_, ik)| ik.clone()).collect();
    wanted.sort();
    wanted.dedup();
    let mut quotes: HashMap<String, KiteQuote> = HashMap::new();
    for batch in wanted.chunks(QUOTE_BATCH) {
        // The quote pacer spaces batches at Kite's 1 request per second.
        quotes.extend(fetch_quotes(b, auth, batch).await?);
    }
    Ok(resolved
        .into_iter()
        .map(|(k, ik)| match ik {
            None => QuoteResult {
                symbol: k.symbol,
                exchange: k.exchange,
                data: None,
                error: Some("Could not resolve broker symbol".into()),
            },
            Some(ik) => match quotes.get(&ik) {
                Some(q) => QuoteResult {
                    data: Some(to_quote(&k, q)),
                    symbol: k.symbol,
                    exchange: k.exchange,
                    error: None,
                },
                None => QuoteResult {
                    symbol: k.symbol,
                    exchange: k.exchange,
                    data: None,
                    error: Some("No quote data available".into()),
                },
            },
        })
        .collect())
}

pub async fn get_market_depth(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let row = lookup(b, key)?;
    let ik = instrument_key(&row);
    let mut data = fetch_quotes(b, auth, std::slice::from_ref(&ik)).await?;
    let q = data.remove(&ik).ok_or_else(|| {
        AppError::Broker(format!(
            "Zerodha returned no market depth for {} {}.",
            key.exchange, key.symbol
        ))
    })?;
    Ok(to_depth(key, &q))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct KiteCandles {
    candles: Vec<Vec<Value>>,
}

fn num(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Kite candle rows -> candles (epoch seconds; daily shifted to IST date).
pub fn parse_candles(rows: &[Vec<Value>], daily: bool) -> Vec<Candle> {
    rows.iter()
        .filter_map(|r| {
            let ts = parse_iso_epoch(r.first()?.as_str()?)?;
            Some(Candle {
                timestamp: if daily { ts + IST_OFFSET_SECS } else { ts },
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

/// Kite interval for an OpenAlgo interval key.
pub fn kite_interval(interval: &str) -> Result<&'static str> {
    TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == interval)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let list: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Interval {} is not supported by Zerodha. Use one of: {}.",
                interval,
                list.join(", ")
            ))
        })
}

pub async fn get_history(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let resolution = kite_interval(&req.interval)?;
    let row = lookup(b, &req.key)?;
    let token = instrument_token(&row.token).ok_or_else(|| {
        AppError::Broker(format!(
            "The master contract has no Zerodha instrument token for {}. Download the master contract again.",
            req.key.symbol
        ))
    })?;
    let daily = resolution == "day";
    // Kite per-request limits: 2000 days for `day`, 60 days otherwise.
    let max_days = if daily { 2000 } else { 60 };
    let mut out = Vec::new();
    for (from, to) in chunks(req.start, req.end, max_days) {
        let path = format!(
            "/instruments/historical/{}/{}?from={}+00:00:00&to={}+23:59:59&oi=1",
            token,
            resolution,
            from.format("%Y-%m-%d"),
            to.format("%Y-%m-%d")
        );
        let env = b
            .call_raw::<KiteCandles>(Method::GET, &path, auth, Body::None, Category::History)
            .await?;
        if let Some(d) = env.data {
            out.extend(parse_candles(&d.candles, daily));
        }
    }
    Ok(sort_dedupe(out))
}
