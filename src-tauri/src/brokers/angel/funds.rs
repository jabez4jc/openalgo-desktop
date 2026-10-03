//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).

use super::mapping::{map_order_type, map_product_type};
use super::orders::raw_positions;
use super::{AngelBroker, Category};
use crate::brokers::common::de::f64_lenient;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

pub const RMS_PATH: &str = "/rest/secure/angelbroking/user/v1/getRMS";
pub const MARGIN_PATH: &str = "/rest/secure/angelbroking/margin/v1/batch";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelRms {
    #[serde(deserialize_with = "f64_lenient")]
    pub net: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub availablecash: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub availableintradaypayin: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub utiliseddebits: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub utilisedspan: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub utilisedexposure: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pub utilisedpayout: f64,
}

/// web `get_margin_data`: Angel's raw `availablecash` is a net-margin figure;
/// collateral is `availablecash - utilisedpayout` and free cash is backed
/// out as `availablecash + utiliseddebits - collateral`.
pub fn funds_from_rms(d: &AngelRms) -> Funds {
    let collateral = d.availablecash - d.utilisedpayout;
    Funds {
        available_cash: d.availablecash + d.utiliseddebits - collateral,
        used_margin: d.utiliseddebits,
        total_margin: d.net,
        opening_balance: d.availableintradaypayin,
        payin: d.availableintradaypayin,
        payout: d.utilisedpayout,
        span: d.utilisedspan,
        exposure: d.utilisedexposure,
        collateral,
        m2m_unrealized: 0.0,
        m2m_realized: 0.0,
        utilised_debits: d.utiliseddebits,
    }
}

/// web `_get_realised_unrealised_pnl`: flat rows count as realised, open
/// rows as unrealised, both from the position book's `pnl`.
pub fn m2m(positions: &[Position]) -> (f64, f64) {
    positions.iter().fold((0.0, 0.0), |(r, u), p| {
        if p.quantity == 0 {
            (r + p.pnl, u)
        } else {
            (r, u + p.pnl)
        }
    })
}

pub async fn get_funds(b: &AngelBroker, auth: &AuthToken) -> Result<Funds> {
    let d: AngelRms = b
        .call(Method::GET, RMS_PATH, auth, None, Category::Other)
        .await?
        .ok_or_else(|| {
            AppError::Broker("Angel One returned no fund details. Try again shortly.".into())
        })?;
    let mut funds = funds_from_rms(&d);
    // Best effort, as on the web: a failure leaves P&L at zero.
    match raw_positions(b, auth).await {
        Ok(rows) => {
            let ps = super::mapping::map_positions(rows, b.resolver());
            let (r, u) = m2m(&ps);
            funds.m2m_realized = r;
            funds.m2m_unrealized = u;
        }
        Err(e) => tracing::warn!("Angel One position P&L for funds failed: {}", e.code()),
    }
    Ok(funds)
}

/// Angel `margin/v1/batch` positions (web `transform_margin_positions`):
/// legs whose token cannot be resolved (or is not numeric) are skipped.
pub fn margin_positions(b: &AngelBroker, legs: &[MarginLeg]) -> Vec<Value> {
    legs.iter()
        .filter_map(|leg| {
            let token = b
                .resolver()
                .token(&leg.key.symbol, &leg.key.exchange)
                .map(|t| t.trim().to_string())
                .filter(|t| {
                    !t.is_empty()
                        && t.chars()
                            .filter(|c| *c != '.' && *c != '-')
                            .all(|c| c.is_ascii_digit())
                });
            let Some(token) = token else {
                tracing::warn!(
                    "Margin leg skipped, token not found: {} ({})",
                    leg.key.symbol,
                    leg.key.exchange
                );
                return None;
            };
            Some(json!({
                "exchange": leg.key.exchange,
                "qty": leg.quantity,
                "price": leg.price,
                "productType": map_product_type(leg.product.as_str()),
                "token": token,
                "tradeType": leg.action.as_str(),
                "orderType": map_order_type(leg.pricetype.as_str()),
            }))
        })
        .collect()
}

/// web `parse_margin_response`: `data.totalMarginRequired` and
/// `data.marginComponents.spanMargin`; Angel reports no exposure figure.
pub fn parse_margin(data: &Value) -> MarginResult {
    let f = |v: Option<&Value>| match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    };
    MarginResult {
        total_margin_required: f(data.get("totalMarginRequired")),
        span_margin: f(data
            .get("marginComponents")
            .and_then(|m| m.get("spanMargin"))),
        exposure_margin: 0.0,
    }
}

pub async fn calculate_margin(
    b: &AngelBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let positions = margin_positions(b, legs);
    if positions.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let body = json!({ "positions": positions });
    let data: Value = b
        .call(
            Method::POST,
            MARGIN_PATH,
            auth,
            Some(&body),
            Category::Other,
        )
        .await?
        .unwrap_or_default();
    Ok(parse_margin(&data))
}
