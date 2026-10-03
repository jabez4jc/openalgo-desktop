//! GTT triggers (web `api/gtt_api.py`, `mapping/gtt_data.py`).
//!
//! Kite GTT orders are LIMIT only, so a MARKET request is converted to a
//! Market-Price-Protected LIMIT, as on the web.

use super::mapping;
use super::{Body, Category, ZerodhaBroker};
use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::mapping::PriceType;
use crate::brokers::common::mpp::{instrument_type_from_symbol, protected_price};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

fn lookup(b: &ZerodhaBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

/// Apply MPP to a MARKET GTT: SINGLE protects the limit around LTP, OCO
/// protects each leg around its own trigger. Returns the adjusted request.
pub fn apply_mpp(req: &GttRequest, row: &SymToken, last_price: f64) -> GttRequest {
    let mut r = req.clone();
    if r.pricetype != PriceType::Market {
        return r;
    }
    let it = if row.instrument_type.is_empty() {
        instrument_type_from_symbol(&row.symbol).to_string()
    } else {
        row.instrument_type.clone()
    };
    let tick = (row.tick_size > 0.0).then_some(row.tick_size);
    match r.trigger_type {
        GttTriggerType::Oco => {
            if r.triggerprice_sl > 0.0 {
                r.stoploss = protected_price(r.triggerprice_sl, r.action, &it, tick);
            }
            if r.triggerprice_tg > 0.0 {
                r.target = protected_price(r.triggerprice_tg, r.action, &it, tick);
            }
        }
        GttTriggerType::Single => {
            if last_price > 0.0 {
                r.price = protected_price(last_price, r.action, &it, tick);
            }
        }
    }
    r.pricetype = PriceType::Limit;
    r
}

/// Kite `{type, condition, orders}` for a GTT (web `transform_place_gtt`).
pub fn gtt_body(
    req: &GttRequest,
    row: &SymToken,
    last_price: f64,
) -> Result<(String, Value, Value)> {
    let br = row.br_symbol();
    let ex = req.key.exchange.as_str();
    let lot = (row.lot_size > 0).then_some(i64::from(row.lot_size));
    let qty = mapping::to_kite_quantity(req.quantity, br, ex, lot, "Quantity")?;
    let leg = |price: f64| {
        json!({
            "exchange": ex,
            "tradingsymbol": br,
            "transaction_type": req.action.as_str(),
            "quantity": qty,
            "order_type": req.pricetype.as_str(),
            "product": req.product.as_str(),
            "price": price,
        })
    };
    let (kind, triggers, orders) = match req.trigger_type {
        GttTriggerType::Oco => (
            "two-leg",
            vec![req.triggerprice_sl, req.triggerprice_tg],
            vec![leg(req.stoploss), leg(req.target)],
        ),
        GttTriggerType::Single => {
            let trigger = if req.trigger_price != 0.0 {
                req.trigger_price
            } else if req.triggerprice_sl > 0.0 {
                req.triggerprice_sl
            } else {
                req.triggerprice_tg
            };
            ("single", vec![trigger], vec![leg(req.price)])
        }
    };
    let condition = json!({
        "exchange": ex,
        "tradingsymbol": br,
        "trigger_values": triggers,
        "last_price": last_price,
    });
    Ok((kind.to_string(), condition, Value::Array(orders)))
}

async fn last_price(b: &ZerodhaBroker, auth: &AuthToken, req: &GttRequest) -> Result<f64> {
    if let Some(lp) = req.last_price.filter(|p| *p > 0.0) {
        return Ok(lp);
    }
    let q = super::data::get_quote(b, auth, &req.key).await?;
    if q.ltp > 0.0 {
        Ok(q.ltp)
    } else {
        Err(AppError::Broker(
            "Could not fetch the last price from Zerodha to place the GTT. Try again.".into(),
        ))
    }
}

async fn send(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    method: Method,
    path: &str,
    req: &GttRequest,
) -> Result<GttResponse> {
    let row = lookup(b, &req.key)?;
    let lp = last_price(b, auth, req).await?;
    let adjusted = apply_mpp(req, &row, lp);
    let (kind, condition, orders) = gtt_body(&adjusted, &row, lp)?;
    let form = [
        ("type", kind),
        ("condition", condition.to_string()),
        ("orders", orders.to_string()),
    ];
    let data: Value = b
        .call(method, path, auth, Body::Form(&form), Category::Order)
        .await?;
    Ok(GttResponse {
        trigger_id: trigger_id(&data),
    })
}

fn trigger_id(data: &Value) -> String {
    match data.get("trigger_id") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

pub async fn place_gtt(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    req: &GttRequest,
) -> Result<GttResponse> {
    send(b, auth, Method::POST, "/gtt/triggers", req).await
}

pub async fn modify_gtt(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    trigger_id: &str,
    req: &GttRequest,
) -> Result<GttResponse> {
    if trigger_id.is_empty() {
        return Err(AppError::Validation("trigger_id is required".into()));
    }
    let path = format!("/gtt/triggers/{}", urlencoding::encode(trigger_id));
    let mut r = send(b, auth, Method::PUT, &path, req).await?;
    if r.trigger_id.is_empty() {
        r.trigger_id = trigger_id.to_string();
    }
    Ok(r)
}

pub async fn cancel_gtt(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    trigger_id: &str,
) -> Result<GttResponse> {
    if trigger_id.is_empty() {
        return Err(AppError::Validation("trigger_id is required".into()));
    }
    let data: Value = b
        .call(
            Method::DELETE,
            &format!("/gtt/triggers/{}", urlencoding::encode(trigger_id)),
            auth,
            Body::None,
            Category::Order,
        )
        .await?;
    let id = super::gtt::trigger_id(&data);
    Ok(GttResponse {
        trigger_id: if id.is_empty() {
            trigger_id.to_string()
        } else {
            id
        },
    })
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteGttOrder {
    #[serde(deserialize_with = "string_lenient")]
    pub transaction_type: String,
    #[serde(deserialize_with = "i64_lenient")]
    pub quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub order_type: String,
    #[serde(deserialize_with = "string_lenient")]
    pub product: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteGttCondition {
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    pub trigger_values: Vec<f64>,
    #[serde(deserialize_with = "f64_lenient")]
    pub last_price: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct KiteGtt {
    #[serde(deserialize_with = "string_lenient")]
    pub id: String,
    #[serde(rename = "type", deserialize_with = "string_lenient")]
    pub kind: String,
    #[serde(deserialize_with = "string_lenient")]
    pub status: String,
    pub condition: KiteGttCondition,
    pub orders: Vec<KiteGttOrder>,
    #[serde(deserialize_with = "string_lenient")]
    pub created_at: String,
    #[serde(deserialize_with = "string_lenient")]
    pub updated_at: String,
    #[serde(deserialize_with = "string_lenient")]
    pub expires_at: String,
}

/// web `map_gtt_book`: active triggers only unless history is asked for.
pub fn map_gtt_book(rows: Vec<KiteGtt>, b: &ZerodhaBroker, include_history: bool) -> Vec<GttOrder> {
    let symbols = b.resolver();
    rows.into_iter()
        .filter(|g| include_history || g.status.eq_ignore_ascii_case("active"))
        .map(|g| {
            let ex = g.condition.exchange.clone();
            let br = g.condition.tradingsymbol.clone();
            let lot = symbols
                .by_brsymbol(&ex, &br)
                .map(|r| i64::from(r.lot_size))
                .filter(|_| ex == "MCX");
            GttOrder {
                trigger_id: g.id,
                trigger_type: g.kind,
                status: g.status,
                symbol: symbols.oa_symbol_or_raw(&br, &ex),
                exchange: ex.clone(),
                trigger_prices: g.condition.trigger_values,
                last_price: g.condition.last_price,
                legs: g
                    .orders
                    .into_iter()
                    .map(|o| GttLeg {
                        action: o.transaction_type,
                        quantity: mapping::from_kite_quantity(o.quantity, &br, &ex, lot),
                        price: o.price,
                        pricetype: if o.order_type.is_empty() {
                            "LIMIT".into()
                        } else {
                            o.order_type
                        },
                        product: o.product,
                    })
                    .collect(),
                created_at: g.created_at,
                updated_at: g.updated_at,
                expires_at: g.expires_at,
            }
        })
        .collect()
}

pub async fn get_gtt_book(
    b: &ZerodhaBroker,
    auth: &AuthToken,
    include_history: bool,
) -> Result<Vec<GttOrder>> {
    let env = b
        .call_raw::<Vec<KiteGtt>>(
            Method::GET,
            "/gtt/triggers",
            auth,
            Body::None,
            Category::Other,
        )
        .await?;
    Ok(map_gtt_book(
        env.data.unwrap_or_default(),
        b,
        include_history,
    ))
}
