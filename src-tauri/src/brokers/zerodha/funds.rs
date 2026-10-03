//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).

use super::mapping::{self, KitePosition};
use super::orders::raw_positions;
use super::{Body, Category, ZerodhaBroker};
use crate::brokers::common::de::f64_lenient;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteAvailable {
    #[serde(deserialize_with = "f64_lenient")]
    pub cash: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub collateral: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub opening_balance: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub intraday_payin: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteUtilised {
    #[serde(deserialize_with = "f64_lenient")]
    pub debits: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub exposure: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub span: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub payout: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteSegment {
    #[serde(deserialize_with = "f64_lenient")]
    pub net: f64,
    pub available: KiteAvailable,
    pub utilised: KiteUtilised,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteMargins {
    pub equity: KiteSegment,
    pub commodity: KiteSegment,
}

/// Funds from `/user/margins` (web `get_margin_data`): debits and
/// collateral summed over both segments; available cash derived from the
/// always-reliable `net + debits - collateral` (Kite's `available.cash` has
/// been seen reporting 0 for funded accounts, web issue #1582).
pub fn funds_from_margins(m: &KiteMargins) -> Funds {
    let debits = m.commodity.utilised.debits + m.equity.utilised.debits;
    let collateral = m.commodity.available.collateral + m.equity.available.collateral;
    let net = m.commodity.net + m.equity.net;
    let available = net + debits - collateral;
    Funds {
        available_cash: available,
        used_margin: debits,
        total_margin: net,
        opening_balance: m.equity.available.opening_balance + m.commodity.available.opening_balance,
        payin: m.equity.available.intraday_payin + m.commodity.available.intraday_payin,
        payout: m.equity.utilised.payout + m.commodity.utilised.payout,
        span: m.equity.utilised.span + m.commodity.utilised.span,
        exposure: m.equity.utilised.exposure + m.commodity.utilised.exposure,
        collateral,
        m2m_unrealized: 0.0,
        m2m_realized: 0.0,
        utilised_debits: debits,
    }
}

/// Realised P&L of closed rows and unrealised P&L of open rows priced at the
/// live LTP (falling back to the row's `last_price`), using Kite's own
/// position `multiplier` on the raw (contract) quantity.
pub fn m2m(positions: &[KitePosition], ltp: &HashMap<String, f64>) -> (f64, f64) {
    let mut realised = 0.0;
    let mut unrealised = 0.0;
    for p in positions {
        if p.quantity == 0 {
            realised += p.sell_value - p.buy_value;
        } else {
            let key = format!("{}:{}", p.exchange, p.tradingsymbol);
            let live = ltp.get(&key).copied().unwrap_or(p.last_price);
            let mult = if p.multiplier == 0.0 {
                1.0
            } else {
                p.multiplier
            };
            unrealised += (live - p.average_price) * p.quantity as f64 * mult;
        }
    }
    (realised, unrealised)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LtpEntry {
    #[serde(deserialize_with = "f64_lenient")]
    last_price: f64,
}

pub async fn get_funds(b: &ZerodhaBroker, auth: &AuthToken) -> Result<Funds> {
    let margins: KiteMargins = b
        .call(
            Method::GET,
            "/user/margins",
            auth,
            Body::None,
            Category::Other,
        )
        .await?;
    let mut funds = funds_from_margins(&margins);
    // P&L is best effort, as on the web: a failure here leaves it at zero.
    match pnl(b, auth).await {
        Ok((r, u)) => {
            funds.m2m_realized = r;
            funds.m2m_unrealized = u;
        }
        Err(e) => tracing::warn!("Zerodha position P&L for funds failed: {}", e.code()),
    }
    Ok(funds)
}

async fn pnl(b: &ZerodhaBroker, auth: &AuthToken) -> Result<(f64, f64)> {
    let positions = raw_positions(b, auth).await?;
    let open: Vec<String> = positions
        .iter()
        .filter(|p| p.quantity != 0)
        .map(|p| format!("{}:{}", p.exchange, p.tradingsymbol))
        .collect();
    let mut ltp = HashMap::new();
    for batch in open.chunks(super::data::QUOTE_BATCH) {
        let q: Vec<String> = batch
            .iter()
            .map(|i| format!("i={}", urlencoding::encode(i)))
            .collect();
        let env = b
            .call_raw::<HashMap<String, LtpEntry>>(
                Method::GET,
                &format!("/quote/ltp?{}", q.join("&")),
                auth,
                Body::None,
                Category::Quote,
            )
            .await?;
        for (k, v) in env.data.unwrap_or_default() {
            ltp.insert(k, v.last_price);
        }
    }
    Ok(m2m(&positions, &ltp))
}

/// Kite margin order entries (web `transform_margin_positions`). Legs that
/// do not resolve are skipped; a quantity that is not whole MCX contracts
/// refuses the whole basket.
pub fn margin_payload(b: &ZerodhaBroker, legs: &[MarginLeg]) -> Result<Vec<Value>> {
    let mut out = Vec::with_capacity(legs.len());
    for leg in legs {
        let Some(row) = b.resolver().by_symbol(&leg.key.exchange, &leg.key.symbol) else {
            tracing::warn!(
                "Margin leg skipped, symbol not found: {} ({})",
                leg.key.symbol,
                leg.key.exchange
            );
            continue;
        };
        let lot = (row.lot_size > 0).then_some(i64::from(row.lot_size));
        let qty = mapping::to_kite_quantity(
            leg.quantity,
            &leg.key.symbol,
            &leg.key.exchange,
            lot,
            "Quantity",
        )?;
        out.push(json!({
            "exchange": leg.key.exchange,
            "tradingsymbol": row.br_symbol(),
            "transaction_type": leg.action.as_str(),
            "variety": "regular",
            "product": leg.product.as_str(),
            "order_type": leg.pricetype.as_str(),
            "quantity": qty,
            "price": leg.price,
            "trigger_price": leg.trigger_price,
        }));
    }
    Ok(out)
}

/// Basket response (`final` = fully optimised) or orders response (summed).
pub fn parse_margin(data: &Value) -> MarginResult {
    let f = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    if let Some(fin) = data.get("final") {
        MarginResult {
            total_margin_required: f(fin, "total"),
            span_margin: f(fin, "span"),
            exposure_margin: f(fin, "exposure"),
        }
    } else if let Some(arr) = data.as_array() {
        let mut r = MarginResult::default();
        for o in arr {
            r.total_margin_required += f(o, "total");
            r.span_margin += f(o, "span");
            r.exposure_margin += f(o, "exposure");
        }
        r
    } else {
        MarginResult::default()
    }
}

pub async fn calculate_margin(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let payload = margin_payload(b, legs)?;
    if payload.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    // Two or more legs: basket endpoint with existing positions considered,
    // so hedges get their spread benefit. One leg: the orders endpoint.
    let path = if payload.len() > 1 {
        "/margins/basket?consider_positions=true"
    } else {
        "/margins/orders"
    };
    let body = Value::Array(payload);
    let data: Value = b
        .call(Method::POST, path, auth, Body::Json(&body), Category::Other)
        .await?;
    Ok(parse_margin(&data))
}
