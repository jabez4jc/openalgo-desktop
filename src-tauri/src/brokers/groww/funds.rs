//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).

use super::mapping;
use super::{groww_error, Category, GrowwCore};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Map, Value};

fn num(v: &Value, pointer: &str) -> f64 {
    match v.pointer(pointer) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `/v1/margins/detail/user` payload -> funds (web `get_margin_data`):
/// cash = `clear_cash`, collateral = `collateral_available`, used =
/// `net_margin_used`; Groww reports no M2M here, so both are 0.
pub fn funds_from_payload(p: &Value) -> Funds {
    let used = num(p, "/net_margin_used");
    let cash = num(p, "/clear_cash");
    let collateral = num(p, "/collateral_available");
    Funds {
        available_cash: cash,
        used_margin: used,
        total_margin: cash + used,
        collateral,
        utilised_debits: used,
        m2m_realized: 0.0,
        m2m_unrealized: 0.0,
        ..Default::default()
    }
}

pub async fn get_funds(core: &GrowwCore, auth: &AuthToken) -> Result<Funds> {
    let payload = core
        .call(
            Method::GET,
            "/v1/margins/detail/user",
            auth,
            None,
            Category::Other,
        )
        .await?;
    Ok(funds_from_payload(&payload))
}

/// Groww margin `exchange` (web `map_margin_exchange`).
fn margin_exchange(exchange: &str) -> &'static str {
    match exchange {
        "BSE" | "BFO" => "BSE",
        _ => "NSE",
    }
}

/// Segment and body of a margin request (web `transform_margin_positions`):
/// every leg must share the first leg's segment (others are dropped); a
/// CASH basket is cut to its first leg; unknown symbols go as given.
pub fn margin_payload(core: &GrowwCore, legs: &[MarginLeg]) -> (String, Vec<Value>) {
    let mut segment: Option<&'static str> = None;
    let mut out = Vec::new();
    for leg in legs {
        let seg = mapping::groww_segment(&leg.key.exchange);
        match segment {
            None => segment = Some(seg),
            Some(s) if s != seg => {
                tracing::warn!(
                    "Groww margin takes one segment per request; leg on {} dropped",
                    leg.key.exchange
                );
                continue;
            }
            _ => {}
        }
        let br = core
            .symbols
            .br_symbol(&leg.key.symbol, &leg.key.exchange)
            .unwrap_or_else(|| leg.key.symbol.clone());
        let mut m = Map::new();
        m.insert("trading_symbol".into(), json!(br));
        m.insert("transaction_type".into(), json!(leg.action.as_str()));
        m.insert("quantity".into(), json!(leg.quantity));
        m.insert(
            "order_type".into(),
            json!(mapping::order_type(leg.pricetype)),
        );
        m.insert("product".into(), json!(mapping::product(leg.product)));
        m.insert("exchange".into(), json!(margin_exchange(&leg.key.exchange)));
        if leg.price > 0.0 {
            m.insert("price".into(), json!(leg.price));
        }
        out.push(Value::Object(m));
    }
    let segment = segment.unwrap_or(mapping::SEGMENT_CASH);
    if segment == mapping::SEGMENT_CASH && out.len() > 1 {
        tracing::info!("Groww calculates CASH margin for one position; using the first");
        out.truncate(1);
    }
    (segment.to_string(), out)
}

/// Response payload -> web `/margin` data.
pub fn parse_margin(p: &Value) -> MarginResult {
    MarginResult {
        total_margin_required: num(p, "/total_requirement"),
        span_margin: num(p, "/span_required"),
        exposure_margin: num(p, "/exposure_required"),
    }
}

pub async fn calculate_margin(
    core: &GrowwCore,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let (segment, body) = margin_payload(core, legs);
    if body.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let body = Value::Array(body);
    let r = core
        .send(
            Method::POST,
            &format!("/v1/margins/detail/orders?segment={}", segment),
            auth,
            Some(&body),
            Category::Other,
            true,
        )
        .await?;
    if !r.is_success() {
        return Err(groww_error(&r));
    }
    Ok(parse_margin(r.payload()))
}
