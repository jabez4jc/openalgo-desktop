//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).

use super::mapping::product_code;
use super::orders::raw_positions;
use super::{envelope_data, Category, UpstoxBroker};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

/// Upstox code for "funds service is outside 5:30 AM to 12:00 AM IST".
pub const SERVICE_HOURS_CODE: &str = "UDAPI100072";
/// Most instruments one margin request may carry.
pub const MARGIN_MAX_INSTRUMENTS: usize = 20;

fn num(v: &Value, path: &[&str]) -> f64 {
    let mut cur = v;
    for k in path {
        match cur.get(*k) {
            Some(n) => cur = n,
            None => return 0.0,
        }
    }
    match cur {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Funds from the v3 `get-funds-and-margin` data (web `get_margin_data`):
/// cash available, pledge margin net of what it already backs, and the
/// margin used by both. `pledge_available_to_trade` has no top-level total.
pub fn funds_from_v3(data: &Value) -> Funds {
    let a = ["available_to_trade"];
    let cash_total = num(data, &[a[0], "cash_available_to_trade", "total"]);
    let cash_used = num(
        data,
        &[a[0], "cash_available_to_trade", "margin_used", "total"],
    );
    let pledge = num(
        data,
        &[
            a[0],
            "pledge_available_to_trade",
            "margin_from_pledge",
            "total",
        ],
    );
    let pledge_used = num(
        data,
        &[a[0], "pledge_available_to_trade", "margin_used", "total"],
    );
    let used = cash_used + pledge_used;
    Funds {
        available_cash: cash_total,
        used_margin: used,
        total_margin: cash_total + used,
        collateral: pledge - pledge_used,
        utilised_debits: used,
        ..Default::default()
    }
}

/// Whether an error body is the funds service-hours refusal.
pub fn is_service_hours(status: StatusCode, body: &Value) -> bool {
    status == StatusCode::LOCKED
        && body
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|e| {
                e.iter().any(|x| {
                    x.get("errorCode")
                        .or_else(|| x.get("error_code"))
                        .and_then(Value::as_str)
                        == Some(SERVICE_HOURS_CODE)
                })
            })
}

pub async fn get_funds(b: &UpstoxBroker, auth: &AuthToken) -> Result<Funds> {
    let url = b.api("/v3/user/get-funds-and-margin");
    let (status, body) = b
        .send(
            Method::GET,
            &url,
            auth,
            None,
            Category::Standard,
            &[("Api-Version", "3.0")],
        )
        .await?;
    if is_service_hours(status, &body) {
        // web: outside service hours every figure reads 0.00.
        tracing::info!("Upstox funds service is outside its operating hours");
        return Ok(Funds::default());
    }
    let data = envelope_data(status, body, &url)?;
    let mut funds = funds_from_v3(&data);
    // Realised / unrealised from the position book, best effort as on the web.
    match raw_positions(b, auth).await {
        Ok(rows) => {
            funds.m2m_realized = rows.iter().map(|p| p.realised).sum();
            funds.m2m_unrealized = rows.iter().map(|p| p.unrealised).sum();
        }
        Err(e) => tracing::warn!("Upstox position P&L for funds failed: {}", e.code()),
    }
    Ok(funds)
}

/// Margin instruments (web `transform_margin_positions`): legs without an
/// instrument key are skipped; price only when positive.
pub fn margin_instruments(b: &UpstoxBroker, legs: &[MarginLeg]) -> Vec<Value> {
    let mut out = Vec::with_capacity(legs.len());
    for leg in legs {
        let key = b
            .resolver()
            .token(&leg.key.symbol, &leg.key.exchange)
            .filter(|k| k.contains('|'));
        let Some(key) = key else {
            tracing::warn!(
                "Margin leg skipped, no Upstox instrument key: {} ({})",
                leg.key.symbol,
                leg.key.exchange
            );
            continue;
        };
        let mut v = json!({
            "instrument_key": key,
            "quantity": leg.quantity,
            "transaction_type": leg.action.as_str(),
            "product": product_code(leg.product),
        });
        if leg.price > 0.0 {
            v["price"] = json!(leg.price);
        }
        out.push(v);
    }
    out
}

/// web `parse_margin_response`: required margin plus summed span/exposure.
pub fn parse_margin(data: &Value) -> MarginResult {
    let f = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let mut r = MarginResult {
        total_margin_required: f(data, "required_margin"),
        ..Default::default()
    };
    if let Some(ms) = data.get("margins").and_then(Value::as_array) {
        for m in ms {
            r.span_margin += f(m, "span_margin");
            r.exposure_margin += f(m, "exposure_margin");
        }
    }
    r
}

pub async fn calculate_margin(
    b: &UpstoxBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let instruments = margin_instruments(b, legs);
    if instruments.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    if instruments.len() > MARGIN_MAX_INSTRUMENTS {
        return Err(AppError::Validation(
            "Upstox supports maximum 20 instruments per margin request. Please reduce the number of positions."
                .into(),
        ));
    }
    let body = json!({ "instruments": instruments });
    let data = b
        .call(
            Method::POST,
            &b.api("/v2/charges/margin"),
            auth,
            Some(&body),
            Category::Standard,
        )
        .await?;
    Ok(parse_margin(&data))
}
