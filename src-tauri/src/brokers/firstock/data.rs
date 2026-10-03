//! Quotes, multiquotes, depth and history (web `api/data.py`).

use super::{session, FirstockBroker, Session, TIMEFRAME_MAP};
use crate::brokers::common::history::chunks;
use crate::brokers::common::streaming::round2;
use crate::brokers::common::symbols::SymToken;
use crate::brokers::families::noren::mapping::{f, i, noren_exchange, text};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};

/// `getMultiQuotes` batch size.
pub const MULTIQUOTE_BATCH: usize = 50;

fn lookup(b: &FirstockBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

/// `/getQuote` data object (by trading symbol, not token).
pub(crate) async fn quote_data(
    b: &FirstockBroker,
    s: &Session,
    exchange: &str,
    brsymbol: &str,
) -> Result<Value> {
    b.quote_pacer.acquire().await;
    let v = b
        .call_ok(
            "/getQuote",
            json!({"exchange": noren_exchange(exchange), "tradingSymbol": brsymbol}),
            s,
        )
        .await?;
    Ok(v.get("data").cloned().unwrap_or_default())
}

pub fn to_quote(key: &QuoteKey, d: &Value) -> Quote {
    let mut q = Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: f(d, "lastTradedPrice"),
        open: f(d, "dayOpenPrice"),
        high: f(d, "dayHighPrice"),
        low: f(d, "dayLowPrice"),
        close: f(d, "dayClosePrice"),
        volume: i(d, "volume"),
        bid: f(d, "bestBuyPrice1"),
        ask: f(d, "bestSellPrice1"),
        bid_qty: i(d, "bestBuyQuantity1"),
        ask_qty: i(d, "bestSellQuantity1"),
        oi: i(d, "openInterest"),
        ..Default::default()
    };
    if q.close > 0.0 {
        q.change = round2(q.ltp - q.close);
        q.change_percent = round2((q.ltp - q.close) / q.close * 100.0);
    }
    q
}

pub fn to_depth(key: &QuoteKey, d: &Value) -> MarketDepth {
    let lvl = |side: &str, n: usize| DepthLevel {
        price: f(d, &format!("best{}Price{}", side, n)),
        quantity: i(d, &format!("best{}Quantity{}", side, n)),
        orders: 0,
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: (1..=5).map(|n| lvl("Buy", n)).collect(),
        asks: (1..=5).map(|n| lvl("Sell", n)).collect(),
        ltp: f(d, "lastTradedPrice"),
        ltq: i(d, "lastTradedQuantity"),
        open: f(d, "dayOpenPrice"),
        high: f(d, "dayHighPrice"),
        low: f(d, "dayLowPrice"),
        prev_close: f(d, "dayClosePrice"),
        volume: i(d, "volume"),
        oi: i(d, "openInterest"),
        total_buy_qty: i(d, "totalBuyQuantity"),
        total_sell_qty: i(d, "totalSellQuantity"),
    }
}

pub async fn get_quote(b: &FirstockBroker, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
    let s = session(auth)?;
    let row = lookup(b, key)?;
    let d = quote_data(b, &s, &key.exchange, row.br_symbol()).await?;
    Ok(to_quote(key, &d))
}

pub async fn get_market_depth(
    b: &FirstockBroker,
    auth: &AuthToken,
    key: &QuoteKey,
) -> Result<MarketDepth> {
    let s = session(auth)?;
    let row = lookup(b, key)?;
    let d = quote_data(b, &s, &key.exchange, row.br_symbol()).await?;
    Ok(to_depth(key, &d))
}

/// `/getMultiQuotes`, 50 per call, answers matched on `exchange:tradingSymbol`.
pub async fn get_multiquotes(
    b: &FirstockBroker,
    auth: &AuthToken,
    keys: &[QuoteKey],
) -> Result<Vec<QuoteResult>> {
    let s = session(auth)?;
    let mut out = Vec::with_capacity(keys.len());
    for batch in keys.chunks(MULTIQUOTE_BATCH) {
        let mut wanted = Vec::new();
        let mut index = Vec::new();
        for k in batch {
            match b.resolver().by_symbol(&k.exchange, &k.symbol) {
                Some(r) => {
                    let ex = noren_exchange(&k.exchange).to_string();
                    wanted.push(json!({"exchange": ex, "tradingSymbol": r.br_symbol()}));
                    index.push((k.clone(), Some(format!("{}:{}", ex, r.br_symbol()))));
                }
                None => index.push((k.clone(), None)),
            }
        }
        let mut got = std::collections::HashMap::new();
        if !wanted.is_empty() {
            b.quote_pacer.acquire().await;
            let v = b
                .call_ok("/getMultiQuotes", json!({"data": wanted}), &s)
                .await?;
            for q in v
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                got.insert(
                    format!("{}:{}", text(&q, "exchange"), text(&q, "tradingSymbol")),
                    q,
                );
            }
        }
        for (k, key) in index {
            let r = match key {
                None => QuoteResult {
                    symbol: k.symbol.clone(),
                    exchange: k.exchange.clone(),
                    data: None,
                    error: Some("Could not resolve broker symbol".into()),
                },
                Some(key) => match got.get(&key) {
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
            out.push(r);
        }
    }
    Ok(out)
}

/// Calendar days per history request (web `interval_limits`; 1m per day).
pub fn chunk_days(interval: &str) -> i64 {
    match interval {
        "1m" => 1,
        "3m" => 2,
        "5m" => 3,
        "10m" => 5,
        "15m" => 7,
        "30m" => 10,
        "1h" | "2h" | "4h" => 15,
        _ => 30,
    }
}

/// One candle: `epochTime`, else ISO `time` (IST wall clock). Daily bars
/// are re-stamped to 09:15 of their UTC date, as the web does.
pub fn parse_candle(c: &Value, daily: bool) -> Option<Candle> {
    let ts = if c.get("epochTime").is_some() {
        i(c, "epochTime")
    } else {
        let t = text(c, "time").replace('T', " ");
        let dt = NaiveDateTime::parse_from_str(&t, "%Y-%m-%d %H:%M:%S")
            .ok()
            .or_else(|| {
                let date = t.split(' ').nth(1)?;
                NaiveDate::parse_from_str(date, "%d-%m-%Y")
                    .ok()?
                    .and_hms_opt(0, 0, 0)
            })?;
        Kolkata.from_local_datetime(&dt).single()?.timestamp()
    };
    let ts = if daily {
        ts.div_euclid(86400) * 86400 + 9 * 3600 + 15 * 60
    } else {
        ts
    };
    Some(Candle {
        timestamp: ts,
        open: f(c, "open"),
        high: f(c, "high"),
        low: f(c, "low"),
        close: f(c, "close"),
        volume: i(c, "volume"),
        oi: 0,
    })
}

pub fn interval(code: &str) -> Result<&'static str> {
    TIMEFRAME_MAP
        .iter()
        .find(|(k, _)| *k == code)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let list: Vec<&str> = TIMEFRAME_MAP.iter().map(|(k, _)| *k).collect();
            AppError::Validation(format!(
                "Interval {} is not supported by Firstock. Use one of: {}.",
                code,
                list.join(", ")
            ))
        })
}

pub async fn get_history(
    b: &FirstockBroker,
    auth: &AuthToken,
    req: &HistoryRequest,
) -> Result<Vec<Candle>> {
    let api_interval = interval(&req.interval)?;
    let s = session(auth)?;
    let row = lookup(b, &req.key)?;
    let daily = req.interval == "D";
    let mut out = Vec::new();
    let (mut tried, mut failed, mut last) = (0, 0, None);
    for (from, to) in chunks(req.start, req.end, chunk_days(&req.interval)) {
        tried += 1;
        let end_time = if daily { "00:00:00" } else { "23:59:59" };
        let body = json!({
            "exchange": noren_exchange(&req.key.exchange),
            "tradingSymbol": row.br_symbol(),
            "startTime": format!("00:00:00 {}", from.format("%d-%m-%Y")),
            "endTime": format!("{} {}", end_time, to.format("%d-%m-%Y")),
            "interval": api_interval,
        });
        match b.call_ok("/timePriceSeries", body, &s).await {
            Ok(v) => out.extend(
                v.get("data")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| parse_candle(c, daily))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            ),
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => {
                failed += 1;
                last = Some(e);
            }
        }
    }
    if tried > 0 && failed == tried {
        if let Some(e) = last {
            return Err(e);
        }
    }
    Ok(crate::brokers::common::history::sort_dedupe(out))
}
