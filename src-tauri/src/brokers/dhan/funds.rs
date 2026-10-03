//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).
//!
//! Margin follows the web's routing: one leg goes to the single-order
//! calculator (`POST /v2/margincalculator`), two or more to the multi-order
//! calculator (`POST /v2/margincalculator/multi`, positions and orders
//! included) so hedges get their spread benefit. A body that is not JSON is
//! a broker error (web: 502), and an HTTP 200 whose body says `error` is an
//! error too (web `_normalise_success_response`).

use super::data::num;
use super::mapping::{exchange_segment, product_type, DhanPosition};
use super::orders::raw_positions;
use super::{Category, DhanBroker, DhanSession};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Map, Value};

/// Funds from `/v2/fundlimit` and the position book (web `get_margin_data`).
/// `availabelBalance` (Dhan's spelling) includes pledged collateral, so the
/// collateral is subtracted to get cash.
pub fn funds_from_limit(limit: &Value, positions: &[DhanPosition]) -> Funds {
    let available = num(limit.get("availabelBalance"));
    let collateral = num(limit.get("collateralAmount"));
    let utilized = num(limit.get("utilizedAmount"));
    Funds {
        available_cash: available - collateral,
        used_margin: utilized,
        total_margin: 0.0,
        opening_balance: num(limit.get("sodLimit")),
        payin: num(limit.get("receiveableAmount")),
        payout: num(limit.get("blockedPayoutAmount")),
        span: 0.0,
        exposure: 0.0,
        collateral,
        m2m_unrealized: positions.iter().map(|p| p.unrealized_profit).sum(),
        m2m_realized: positions.iter().map(|p| p.realized_profit).sum(),
        utilised_debits: utilized,
    }
}

pub async fn get_funds(b: &DhanBroker, auth: &AuthToken) -> Result<Funds> {
    let s = DhanSession::parse(auth)?;
    let limit = b
        .call(
            Method::GET,
            "/v2/fundlimit",
            &s,
            None,
            None,
            Category::Trade,
        )
        .await?;
    // P&L is best effort, as on the web: a failed position read is zero.
    let positions = match raw_positions(b, auth).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Dhan positions for funds failed: {}", e.code());
            Vec::new()
        }
    };
    Ok(funds_from_limit(&limit, &positions))
}

/// One leg in Dhan's margin shape (web `transform_margin_position`); `None`
/// when the symbol or exchange does not resolve.
pub fn margin_leg(b: &DhanBroker, leg: &MarginLeg, client_id: &str) -> Option<Value> {
    let row = b.resolver().by_symbol(&leg.key.exchange, &leg.key.symbol)?;
    let segment = exchange_segment(&leg.key.exchange)?;
    let mut m = Map::new();
    m.insert("dhanClientId".into(), json!(client_id));
    m.insert("exchangeSegment".into(), json!(segment));
    m.insert("transactionType".into(), json!(leg.action.as_str()));
    m.insert("quantity".into(), json!(leg.quantity));
    m.insert("productType".into(), json!(product_type(leg.product)));
    m.insert("securityId".into(), json!(row.token.trim()));
    m.insert("price".into(), json!(leg.price));
    if leg.trigger_price > 0.0 {
        m.insert("triggerPrice".into(), json!(leg.trigger_price));
    }
    Some(Value::Object(m))
}

fn broker_error_message(v: &Value) -> Option<String> {
    let o = v.as_object()?;
    let status = o
        .get("status")
        .map(|s| match s {
            Value::String(s) => s.to_ascii_lowercase(),
            other => other.to_string().to_ascii_lowercase(),
        })
        .unwrap_or_default();
    let has_type = o
        .get("errorType")
        .is_some_and(|t| !t.is_null() && t.as_str() != Some(""));
    if !has_type && !matches!(status.as_str(), "error" | "failed" | "failure") {
        return None;
    }
    let pick = |k: &str| -> Option<String> {
        match o.get(k)? {
            Value::Null => None,
            Value::String(s) if s.is_empty() => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    };
    Some(
        pick("errorMessage")
            .or_else(|| pick("message"))
            .or_else(|| pick("errors"))
            .or_else(|| pick("error"))
            .unwrap_or_else(|| "Dhan margin API returned an error".into()),
    )
}

fn margin_error(message: String) -> AppError {
    AppError::Broker(format!("Dhan could not calculate the margin: {}", message))
}

/// web `parse_margin_response` (single-order calculator).
pub fn parse_single_margin(v: &Value) -> Result<MarginResult> {
    if !v.is_object() {
        return Err(margin_error("Invalid response from broker".into()));
    }
    if let Some(m) = broker_error_message(v) {
        return Err(margin_error(m));
    }
    Ok(MarginResult {
        total_margin_required: num(v.get("totalMargin")),
        span_margin: num(v.get("spanMargin")),
        exposure_margin: num(v.get("exposureMargin")),
    })
}

fn pick_float(v: &Value, keys: &[&str]) -> f64 {
    for k in keys {
        match v.get(*k) {
            None | Some(Value::Null) => continue,
            Some(Value::String(s)) if s.is_empty() => continue,
            Some(Value::String(s)) => match s.trim().parse::<f64>() {
                Ok(f) => return f,
                Err(_) => continue,
            },
            Some(Value::Number(n)) => return n.as_f64().unwrap_or(0.0),
            Some(_) => continue,
        }
    }
    0.0
}

/// web `parse_basket_margin_response` (multi-order calculator): snake_case
/// per the docs, camelCase live; both accepted.
pub fn parse_basket_margin(v: &Value) -> Result<MarginResult> {
    if v.as_object().is_none_or(|o| o.is_empty()) {
        return Err(margin_error("Invalid response from broker".into()));
    }
    if let Some(m) = broker_error_message(v) {
        return Err(margin_error(m));
    }
    Ok(MarginResult {
        total_margin_required: pick_float(v, &["total_margin", "totalMargin"]),
        span_margin: pick_float(v, &["span_margin", "spanMargin"]),
        exposure_margin: pick_float(v, &["exposure_margin", "exposureMargin", "exposure"]),
    })
}

pub async fn calculate_margin(
    b: &DhanBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let s = DhanSession::parse(auth)?;
    let cid = s
        .require_client_id()
        .map_err(|_| {
            AppError::Validation(
                "Could not determine Dhan client ID. Please ensure BROKER_API_KEY is configured correctly."
                    .into(),
            )
        })?
        .to_string();
    let mut payload = Vec::with_capacity(legs.len());
    for leg in legs {
        match margin_leg(b, leg, &cid) {
            Some(v) => payload.push(v),
            None => tracing::warn!(
                "Margin leg skipped, symbol not found: {} ({})",
                leg.key.symbol,
                leg.key.exchange
            ),
        }
    }
    if payload.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let single = payload.len() == 1;
    let (path, body) = if single {
        ("/v2/margincalculator", payload.remove(0))
    } else {
        (
            "/v2/margincalculator/multi",
            json!({
                "dhanClientId": cid,
                "includePosition": true,
                "includeOrder": true,
                "scripList": payload,
            }),
        )
    };
    let resp = b
        .http
        .post(format!("{}{}", b.base_url, path))
        .header("access-token", &s.access_token)
        .header("client-id", &cid)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    // JSON-decode guard: a non-JSON answer is a broker error (web 502).
    let v: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!(status = status.as_u16(), "Dhan margin answer is not JSON");
            return Err(AppError::Broker(
                "Invalid response from broker API. Try again shortly.".into(),
            ));
        }
    };
    if status.as_u16() == 401 || status.as_u16() == 403 {
        if let Some(e) = super::dhan_error(&v) {
            return Err(e);
        }
    }
    // HTTP 200 with an error body is still an error (normaliser).
    let parsed = if single {
        parse_single_margin(&v)
    } else {
        parse_basket_margin(&v)
    };
    if parsed.is_ok() && !status.is_success() {
        tracing::warn!(status = status.as_u16(), "Dhan margin call failed");
        return Err(AppError::Broker(
            "Dhan could not calculate the margin right now. Try again shortly.".into(),
        ));
    }
    parsed
}
