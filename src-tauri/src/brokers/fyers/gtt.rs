//! GTT orders (web `api/gtt_api.py`, `mapping/gtt_data.py`).
//!
//! `POST/PATCH/DELETE /api/v3/gtt/orders/sync`, `GET /api/v3/gtt/orders`.
//! A Fyers GTT leg carries only price, trigger and quantity (the child order
//! is always LIMIT), so MARKET is converted to a Market-Price-Protected
//! LIMIT first, as on the web.

use super::mapping::{get_exchange, num, oa_product, product_code, side_code};
use super::{code_message, fyers_error, FyersBroker};
use crate::brokers::common::mapping::PriceType;
use crate::brokers::common::mpp::{instrument_type_from_symbol, protected_price};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

const GTT_SYNC: &str = "/api/v3/gtt/orders/sync";
const GTT_BOOK: &str = "/api/v3/gtt/orders";
/// Fyers GTT success codes: 1101 placed, 1102 modified, 1103 cancelled, 201
/// in transit, 200 book.
const GTT_OK_CODES: &[i64] = &[200, 201, 1101, 1102, 1103];
/// Book statuses that can still fire (4 transit, 6 pending).
const ACTIVE: &[i64] = &[4, 6];

/// web `_is_ok`: `s` decides; the numeric code only when `s` is absent.
pub fn gtt_ok(v: &Value) -> bool {
    match v
        .get("s")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
    {
        Some(s) if s == "ok" => true,
        Some(s) if s == "error" => false,
        _ => GTT_OK_CODES.contains(&code_message(v).0),
    }
}

/// web `_apply_mpp_if_market`: SINGLE protects the limit around LTP (kept
/// as sent when no LTP is known), OCO protects each leg around its own
/// trigger; pricetype becomes LIMIT.
pub fn apply_mpp(req: &GttRequest, row: &SymToken, last_price: Option<f64>) -> GttRequest {
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
        GttTriggerType::Single => match last_price.filter(|p| *p > 0.0) {
            Some(lp) => {
                r.last_price = Some(lp);
                r.price = protected_price(lp, r.action, &it, tick);
            }
            None => tracing::warn!(
                "GTT price protection: no last price for {} on {}; sending the price as LIMIT",
                r.key.symbol,
                r.key.exchange
            ),
        },
    }
    r.pricetype = PriceType::Limit;
    r
}

fn leg(price: f64, trigger: f64, qty: i64) -> Value {
    json!({ "price": price, "triggerPrice": trigger, "qty": qty })
}

/// web `build_order_info`: SINGLE is `leg1`; OCO puts the target (above
/// LTP) in `leg1` and the stop-loss (below LTP) in `leg2`, swapping when the
/// caller sent them the wrong way round.
pub fn order_info(req: &GttRequest) -> Value {
    match req.trigger_type {
        GttTriggerType::Single => {
            let trigger = if req.trigger_price != 0.0 {
                req.trigger_price
            } else if req.triggerprice_sl > 0.0 {
                req.triggerprice_sl
            } else {
                req.triggerprice_tg
            };
            json!({ "leg1": leg(req.price, trigger, req.quantity) })
        }
        GttTriggerType::Oco => {
            let (mut tg_t, mut sl_t) = (req.triggerprice_tg, req.triggerprice_sl);
            let (mut tg_p, mut sl_p) = (req.target, req.stoploss);
            if sl_t >= tg_t {
                tracing::warn!(
                    "Fyers GTT OCO: stop-loss trigger {} is not below target trigger {}; swapping legs",
                    sl_t,
                    tg_t
                );
                std::mem::swap(&mut tg_t, &mut sl_t);
                std::mem::swap(&mut tg_p, &mut sl_p);
            }
            json!({
                "leg1": leg(tg_p, tg_t, req.quantity),
                "leg2": leg(sl_p, sl_t, req.quantity),
            })
        }
    }
}

/// web `transform_place_gtt` with the default `openalgo` tag.
pub fn place_body(req: &GttRequest, row: &SymToken) -> Value {
    json!({
        "side": side_code(req.action),
        "symbol": row.br_symbol(),
        "productType": product_code(req.product),
        "orderInfo": order_info(req),
        "orderTag": "openalgo",
    })
}

/// web `transform_modify_gtt`: id and orderInfo only (side, symbol and
/// product cannot change on a Fyers GTT).
pub fn modify_body(trigger_id: &str, req: &GttRequest) -> Value {
    json!({ "id": trigger_id, "orderInfo": order_info(req) })
}

async fn adjusted(
    b: &FyersBroker,
    auth: &AuthToken,
    req: &GttRequest,
) -> Result<(GttRequest, SymToken)> {
    let row = b.lookup(&req.key)?;
    let needs_ltp = req.pricetype == PriceType::Market
        && req.trigger_type == GttTriggerType::Single
        && req.last_price.filter(|p| *p > 0.0).is_none();
    let lp = if needs_ltp {
        match super::data::get_quote(b, auth, &req.key).await {
            Ok(q) if q.ltp > 0.0 => Some(q.ltp),
            Ok(_) => None,
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => {
                tracing::warn!("Fyers GTT last price lookup failed: {}", e.code());
                None
            }
        }
    } else {
        req.last_price
    };
    Ok((apply_mpp(req, &row, lp), row))
}

fn id_of(v: &Value) -> String {
    match v.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

async fn send(b: &FyersBroker, auth: &AuthToken, method: Method, body: &Value) -> Result<Value> {
    let (status, v) = b.raw(method, GTT_SYNC, auth, Some(body)).await?;
    if gtt_ok(&v) {
        return Ok(v);
    }
    let (code, message) = code_message(&v);
    tracing::warn!(
        status = status.as_u16(),
        code,
        "Fyers GTT refused: {}",
        message
    );
    Err(fyers_error(status.as_u16(), code, &message))
}

pub async fn place_gtt(b: &FyersBroker, auth: &AuthToken, req: &GttRequest) -> Result<GttResponse> {
    let (r, row) = adjusted(b, auth, req).await?;
    let v = send(b, auth, Method::POST, &place_body(&r, &row)).await?;
    let id = id_of(&v);
    if id.is_empty() {
        return Err(AppError::Broker(
            "Fyers accepted the GTT but returned no id. Check the GTT book.".into(),
        ));
    }
    Ok(GttResponse { trigger_id: id })
}

pub async fn modify_gtt(
    b: &FyersBroker,
    auth: &AuthToken,
    trigger_id: &str,
    req: &GttRequest,
) -> Result<GttResponse> {
    if trigger_id.is_empty() {
        return Err(AppError::Validation("trigger_id is required".into()));
    }
    let (r, _) = adjusted(b, auth, req).await?;
    let v = send(b, auth, Method::PATCH, &modify_body(trigger_id, &r)).await?;
    let id = id_of(&v);
    Ok(GttResponse {
        trigger_id: if id.is_empty() {
            trigger_id.to_string()
        } else {
            id
        },
    })
}

pub async fn cancel_gtt(
    b: &FyersBroker,
    auth: &AuthToken,
    trigger_id: &str,
) -> Result<GttResponse> {
    if trigger_id.is_empty() {
        return Err(AppError::Validation("trigger_id is required".into()));
    }
    let v = send(b, auth, Method::DELETE, &json!({ "id": trigger_id })).await?;
    let id = id_of(&v);
    Ok(GttResponse {
        trigger_id: if id.is_empty() {
            trigger_id.to_string()
        } else {
            id
        },
    })
}

/// web `GTT_STATUS_MAP`.
pub fn gtt_status(code: i64) -> &'static str {
    match code {
        1 => "cancelled",
        2 => "triggered",
        4 => "transit",
        5 => "rejected",
        6 => "active",
        _ => "unknown",
    }
}

fn int(v: &Value, k: &str) -> i64 {
    v.get(k).map(num).unwrap_or(0.0) as i64
}

fn float(v: &Value, k: &str) -> f64 {
    v.get(k).map(num).unwrap_or(0.0)
}

fn text(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// web `map_gtt_book`: one row per GTT (OCO legs inline), pending and
/// transit only, trigger prices low to high for OCO.
pub fn map_gtt_book(v: &Value, symbols: &SymbolResolver) -> Vec<GttOrder> {
    let Some(Value::Array(rows)) = v.get("orderBook") else {
        return Vec::new();
    };
    rows.iter()
        .filter(|o| o.is_object())
        .filter(|o| ACTIVE.contains(&int(o, "ord_status")))
        .map(|o| {
            let exchange = get_exchange(int(o, "exchange"), int(o, "segment"));
            let br = text(o, "symbol");
            let symbol = if br.is_empty() {
                String::new()
            } else {
                symbols.oa_symbol_or_raw(&br, &exchange)
            };
            let action = if int(o, "tran_side") == 1 {
                "BUY"
            } else {
                "SELL"
            };
            let raw_product = text(o, "product_type");
            let product = match oa_product(&raw_product).as_str() {
                "unknown" => raw_product,
                p => p.to_string(),
            };
            let mk = |price: &str, qty: &str| GttLeg {
                action: action.to_string(),
                quantity: int(o, qty),
                price: float(o, price),
                pricetype: "LIMIT".into(),
                product: product.clone(),
            };
            let oco = int(o, "gtt_oco_ind") == 2;
            let mut legs = vec![mk("price_limit", "qty")];
            let mut trigger_prices = vec![float(o, "price_trigger")];
            if oco {
                legs.push(mk("price2_limit", "qty2"));
                trigger_prices.push(float(o, "price2_trigger"));
                trigger_prices.sort_by(|a, b| a.total_cmp(b));
            }
            GttOrder {
                trigger_id: text(o, "id"),
                trigger_type: match int(o, "gtt_oco_ind") {
                    1 => "single",
                    2 => "two-leg",
                    _ => "SINGLE",
                }
                .to_string(),
                status: gtt_status(int(o, "ord_status")).to_string(),
                symbol,
                exchange,
                trigger_prices,
                last_price: float(o, "ltp"),
                legs,
                created_at: text(o, "create_time"),
                updated_at: String::new(),
                expires_at: String::new(),
            }
        })
        .collect()
}

/// The web returns active triggers only; `include_history` is accepted for
/// interface parity.
pub async fn get_gtt_book(
    b: &FyersBroker,
    auth: &AuthToken,
    _include_history: bool,
) -> Result<Vec<GttOrder>> {
    let (status, v) = b.raw(Method::GET, GTT_BOOK, auth, None).await?;
    if !gtt_ok(&v) {
        let (code, message) = code_message(&v);
        return Err(fyers_error(status.as_u16(), code, &message));
    }
    Ok(map_gtt_book(&v, b.resolver()))
}
