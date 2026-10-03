//! Forever Orders as GTT (web `api/gtt_api.py`, `mapping/gtt_data.py`).
//!
//! * Place: `POST /v2/forever/orders`, `orderFlag` SINGLE or OCO; an OCO
//!   carries the stop-loss leg as `price`/`triggerPrice` and the target as
//!   `price1`/`triggerPrice1`/`quantity1`.
//! * Modify: one `PUT /v2/forever/orders/{id}` per leg. OCO sends
//!   STOP_LOSS_LEG then TARGET_LEG; SINGLE first reads the book for the leg
//!   name Dhan actually stored. A SINGLE LIMIT with price 0 is sent as
//!   MARKET (Dhan answers DH-905 otherwise).
//! * Book: `GET /v2/forever/orders` (the documented `/v2/forever/all` is a
//!   404), one row per leg, grouped by order id, active states only.
//!
//! Dhan's load balancer answers HTTP/2 writes to these paths with a bogus
//! 301; the shared client is HTTP/1.1 only, as the web's dedicated client.

use super::mapping::{exchange_segment, map_exchange, product_type, reverse_product};
use super::{Category, DhanBroker, DhanSession};
use crate::brokers::common::mapping::PriceType;
use crate::brokers::common::symbols::SymbolResolver;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::{Method, StatusCode};
use serde_json::{json, Map, Value};

fn single_trigger(req: &GttRequest) -> f64 {
    if req.trigger_price != 0.0 {
        req.trigger_price
    } else if req.triggerprice_sl > 0.0 {
        req.triggerprice_sl
    } else {
        req.triggerprice_tg
    }
}

fn flag(req: &GttRequest) -> &'static str {
    match req.trigger_type {
        GttTriggerType::Single => "SINGLE",
        GttTriggerType::Oco => "OCO",
    }
}

/// `POST /v2/forever/orders` body (web `transform_place_gtt`).
pub fn place_gtt_body(req: &GttRequest, security_id: &str, client_id: &str) -> Result<Value> {
    let segment = exchange_segment(&req.key.exchange).ok_or_else(|| {
        AppError::Validation(format!(
            "Dhan does not accept GTT orders on {}.",
            req.key.exchange
        ))
    })?;
    let (price, trigger) = match req.trigger_type {
        GttTriggerType::Single => (req.price, single_trigger(req)),
        GttTriggerType::Oco => (req.stoploss, req.triggerprice_sl),
    };
    let mut m = Map::new();
    m.insert("dhanClientId".into(), json!(client_id));
    m.insert("orderFlag".into(), json!(flag(req)));
    m.insert("transactionType".into(), json!(req.action.as_str()));
    m.insert("exchangeSegment".into(), json!(segment));
    m.insert("productType".into(), json!(product_type(req.product)));
    m.insert("orderType".into(), json!(req.pricetype.as_str()));
    m.insert("validity".into(), json!("DAY"));
    m.insert("securityId".into(), json!(security_id));
    m.insert("quantity".into(), json!(req.quantity));
    m.insert("price".into(), json!(price));
    m.insert("triggerPrice".into(), json!(trigger));
    if req.trigger_type == GttTriggerType::Oco {
        m.insert("price1".into(), json!(req.target));
        m.insert("triggerPrice1".into(), json!(req.triggerprice_tg));
        m.insert("quantity1".into(), json!(req.quantity));
    }
    Ok(Value::Object(m))
}

/// One leg's `PUT /v2/forever/orders/{id}` body (web `transform_modify_gtt`).
pub fn modify_gtt_body(
    req: &GttRequest,
    trigger_id: &str,
    leg_name: &str,
    client_id: &str,
) -> Value {
    let (price, trigger) = match req.trigger_type {
        GttTriggerType::Oco if leg_name == "TARGET_LEG" => (req.target, req.triggerprice_tg),
        GttTriggerType::Oco => (req.stoploss, req.triggerprice_sl),
        GttTriggerType::Single => (req.price, single_trigger(req)),
    };
    json!({
        "dhanClientId": client_id,
        "orderId": trigger_id,
        "orderFlag": flag(req),
        "orderType": req.pricetype.as_str(),
        "legName": leg_name,
        "quantity": req.quantity,
        "price": price,
        "triggerPrice": trigger,
        "validity": "DAY",
    })
}

fn sv(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn fv(v: &Value, k: &str) -> f64 {
    super::data::num(v.get(k))
}

fn status_map(raw: &str) -> String {
    match raw {
        "TRANSIT" | "PENDING" | "CONFIRM" => "active".into(),
        "TRADED" => "triggered".into(),
        "EXPIRED" => "expired".into(),
        "CANCELLED" => "cancelled".into(),
        "REJECTED" => "rejected".into(),
        other => other.to_ascii_lowercase(),
    }
}

/// Group Dhan's per-leg rows into one GTT per order id (web `map_gtt_book`).
/// Only TRANSIT/PENDING/CONFIRM rows are kept, as the web does.
pub fn map_gtt_book(rows: &[Value], symbols: &SymbolResolver) -> Vec<GttOrder> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<&Value>> =
        std::collections::HashMap::new();
    for r in rows {
        let id = sv(r, "orderId");
        if id.is_empty() {
            continue;
        }
        let st = sv(r, "orderStatus").to_ascii_uppercase();
        if !matches!(st.as_str(), "TRANSIT" | "PENDING" | "CONFIRM") {
            continue;
        }
        if !groups.contains_key(&id) {
            order.push(id.clone());
        }
        groups.entry(id).or_default().push(r);
    }
    order
        .into_iter()
        .filter_map(|id| {
            let mut legs = groups.remove(&id)?;
            let first = *legs.first()?;
            let ex = map_exchange(&sv(first, "exchangeSegment"));
            let br = sv(first, "tradingSymbol");
            let security_id = sv(first, "securityId");
            let symbol = symbols
                .by_token(&ex, security_id.trim())
                .map(|r| r.symbol)
                .or_else(|| symbols.oa_symbol(&br, &ex))
                .unwrap_or_else(|| br.clone());
            legs.sort_by(|a, b| fv(a, "triggerPrice").total_cmp(&fv(b, "triggerPrice")));
            let trigger_type = if sv(first, "orderType").eq_ignore_ascii_case("OCO") {
                "two-leg"
            } else {
                "single"
            };
            Some(GttOrder {
                trigger_id: id,
                trigger_type: trigger_type.into(),
                status: status_map(&sv(first, "orderStatus").to_ascii_uppercase()),
                symbol,
                exchange: ex,
                trigger_prices: legs.iter().map(|l| fv(l, "triggerPrice")).collect(),
                last_price: 0.0,
                legs: legs
                    .iter()
                    .map(|l| {
                        let price = fv(l, "price");
                        GttLeg {
                            action: sv(l, "transactionType").to_ascii_uppercase(),
                            quantity: fv(l, "quantity") as i64,
                            price,
                            pricetype: if price == 0.0 { "MARKET" } else { "LIMIT" }.into(),
                            product: reverse_product(&sv(l, "productType"))
                                .unwrap_or("CNC")
                                .into(),
                        }
                    })
                    .collect(),
                created_at: sv(first, "createTime"),
                updated_at: sv(first, "updateTime"),
                expires_at: String::new(),
            })
        })
        .collect()
}

fn order_id(v: &Value) -> Option<String> {
    Some(sv(v, "orderId")).filter(|s| !s.is_empty())
}

fn gtt_error(v: &Value, fallback: &str) -> AppError {
    if let Some(e) = super::dhan_error(v) {
        return e;
    }
    let m = sv(v, "errorMessage");
    let m = if m.is_empty() { sv(v, "message") } else { m };
    if m.is_empty() {
        AppError::Broker(fallback.into())
    } else {
        AppError::Broker(format!("Dhan: {}", m))
    }
}

pub async fn place_gtt(b: &DhanBroker, auth: &AuthToken, req: &GttRequest) -> Result<GttResponse> {
    let s = DhanSession::parse(auth)?;
    let cid = s.require_client_id()?.to_string();
    let row = super::data::lookup(b, &req.key)?;
    let body = place_gtt_body(req, row.token.trim(), &cid)?;
    let (status, v) = b
        .send(
            Method::POST,
            "/v2/forever/orders",
            &s,
            Some(&body),
            Some(&cid),
            Category::Trade,
        )
        .await?;
    match order_id(&v) {
        Some(id) if matches!(status, StatusCode::OK | StatusCode::CREATED) => {
            Ok(GttResponse { trigger_id: id })
        }
        _ => {
            tracing::warn!(status = status.as_u16(), "Dhan refused a Forever Order");
            Err(gtt_error(&v, "Dhan did not accept the GTT order."))
        }
    }
}

async fn raw_book(b: &DhanBroker, s: &DhanSession) -> Result<(StatusCode, Value)> {
    b.send(
        Method::GET,
        "/v2/forever/orders",
        s,
        None,
        None,
        Category::Trade,
    )
    .await
}

fn book_rows(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("data") {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

pub async fn modify_gtt(
    b: &DhanBroker,
    auth: &AuthToken,
    trigger_id: &str,
    req: &GttRequest,
) -> Result<GttResponse> {
    if trigger_id.trim().is_empty() {
        return Err(AppError::Validation("trigger_id is required".into()));
    }
    let s = DhanSession::parse(auth)?;
    let cid = s.require_client_id()?.to_string();
    let legs: Vec<String> = match req.trigger_type {
        GttTriggerType::Oco => vec!["STOP_LOSS_LEG".into(), "TARGET_LEG".into()],
        GttTriggerType::Single => {
            // The leg name Dhan stored, which may not be ENTRY_LEG.
            let stored = match raw_book(b, &s).await {
                Ok((StatusCode::OK, v)) => book_rows(v)
                    .into_iter()
                    .find(|r| sv(r, "orderId") == trigger_id)
                    .map(|r| sv(&r, "legName").to_ascii_uppercase())
                    .filter(|l| !l.is_empty()),
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!("Dhan leg-name lookup failed: {}", e.code());
                    None
                }
            };
            vec![stored.unwrap_or_else(|| "ENTRY_LEG".into())]
        }
    };
    let mut req = req.clone();
    if req.trigger_type == GttTriggerType::Single
        && req.pricetype == PriceType::Limit
        && req.price == 0.0
    {
        req.pricetype = PriceType::Market;
    }
    let path = format!("/v2/forever/orders/{}", urlencoding::encode(trigger_id));
    let mut last = trigger_id.to_string();
    for leg in legs {
        let body = modify_gtt_body(&req, trigger_id, &leg, &cid);
        let (status, v) = b
            .send(
                Method::PUT,
                &path,
                &s,
                Some(&body),
                Some(&cid),
                Category::Trade,
            )
            .await?;
        match order_id(&v) {
            Some(id) if status == StatusCode::OK => last = id,
            _ => {
                tracing::warn!(
                    status = status.as_u16(),
                    "Dhan refused a GTT modify ({})",
                    leg
                );
                return Err(gtt_error(&v, "Dhan did not modify the GTT order."));
            }
        }
    }
    Ok(GttResponse { trigger_id: last })
}

pub async fn cancel_gtt(b: &DhanBroker, auth: &AuthToken, trigger_id: &str) -> Result<GttResponse> {
    if trigger_id.trim().is_empty() {
        return Err(AppError::Validation("trigger_id is required".into()));
    }
    let s = DhanSession::parse(auth)?;
    let (status, v) = b
        .send(
            Method::DELETE,
            &format!("/v2/forever/orders/{}", urlencoding::encode(trigger_id)),
            &s,
            None,
            None,
            Category::Trade,
        )
        .await?;
    match order_id(&v) {
        Some(id) if status == StatusCode::OK => Ok(GttResponse { trigger_id: id }),
        _ => Err(gtt_error(&v, "Failed to cancel GTT")),
    }
}

pub async fn get_gtt_book(
    b: &DhanBroker,
    auth: &AuthToken,
    _include_history: bool,
) -> Result<Vec<GttOrder>> {
    let s = DhanSession::parse(auth)?;
    let (status, v) = raw_book(b, &s).await?;
    if status != StatusCode::OK {
        return Err(gtt_error(&v, "Failed to fetch Forever orders"));
    }
    Ok(map_gtt_book(&book_rows(v), b.resolver()))
}
