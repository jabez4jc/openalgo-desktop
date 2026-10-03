//! Orders and books (web `api/order_api.py`).

use super::mapping::{self, GrowwHolding, GrowwOrder, GrowwPosition, GrowwTrade};
use super::{groww_error, Category, GrowwCore};
use crate::brokers::common::mapping::{Exchange, PriceType, Product};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Map, Value};

/// Orders per page of `/v1/order/list` ("Maximum allowed by Groww API").
pub const PAGE_SIZE: usize = 25;
/// Pages read per segment at most (2500 orders), so a broker that never
/// returns a short page cannot loop forever.
const MAX_PAGES: usize = 100;
/// Trades per `/v1/order/trades/{id}` page (web reads page 0 only).
pub const TRADES_PAGE_SIZE: usize = 50;

/// The `/v1/order/create` body (web `direct_place_order_api`).
pub fn place_order_body(o: &ResolvedOrder, reference_id: &str) -> Value {
    let ex = o.exchange.as_str();
    let mut m = Map::new();
    m.insert("trading_symbol".into(), json!(o.brsymbol()));
    m.insert("quantity".into(), json!(o.quantity));
    m.insert("validity".into(), json!(o.validity.as_str()));
    m.insert("exchange".into(), json!(mapping::groww_exchange(ex)));
    m.insert("segment".into(), json!(mapping::groww_segment(ex)));
    m.insert("product".into(), json!(mapping::product(o.product)));
    m.insert("order_type".into(), json!(mapping::order_type(o.pricetype)));
    m.insert("transaction_type".into(), json!(o.action.as_str()));
    m.insert("order_reference_id".into(), json!(reference_id));
    // The web sends `price` for LIMIT only; Groww's STOP_LOSS_LIMIT needs its
    // limit price too, so SL carries it as well.
    if matches!(o.pricetype, PriceType::Limit | PriceType::Sl) {
        m.insert("price".into(), json!(o.price));
    }
    if matches!(o.pricetype, PriceType::Sl | PriceType::SlM) {
        m.insert("trigger_price".into(), json!(o.trigger_price));
    }
    Value::Object(m)
}

/// The `/v1/order/modify` body.
pub fn modify_order_body(m: &ResolvedModify) -> Value {
    let mut b = Map::new();
    b.insert("groww_order_id".into(), json!(m.order_id));
    b.insert("order_type".into(), json!(mapping::order_type(m.pricetype)));
    b.insert(
        "segment".into(),
        json!(mapping::groww_segment(m.exchange.as_str())),
    );
    if m.quantity > 0 {
        b.insert("quantity".into(), json!(m.quantity));
    }
    if matches!(m.pricetype, PriceType::Limit | PriceType::Sl) {
        b.insert("price".into(), json!(m.price));
    }
    if matches!(m.pricetype, PriceType::Sl | PriceType::SlM) {
        b.insert("trigger_price".into(), json!(m.trigger_price));
    }
    Value::Object(b)
}

fn validate(o: &ResolvedOrder) -> Result<()> {
    if o.quantity <= 0 {
        return Err(AppError::Validation(
            "Quantity must be greater than zero.".into(),
        ));
    }
    if o.exchange.is_index() {
        return Err(AppError::Validation(format!(
            "{} is an index and cannot be traded. Trade its futures or options instead.",
            o.symbol
        )));
    }
    if matches!(o.pricetype, PriceType::Sl | PriceType::SlM) && o.trigger_price <= 0.0 {
        return Err(AppError::Validation(
            "Trigger price is required for Stop Loss orders.".into(),
        ));
    }
    Ok(())
}

fn order_id_from(payload: &Value) -> String {
    payload
        .get("groww_order_id")
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

pub async fn place_order(
    core: &GrowwCore,
    auth: &AuthToken,
    o: &ResolvedOrder,
) -> Result<OrderResponse> {
    validate(o)?;
    let reference = mapping::new_reference_id(chrono::Local::now().date_naive());
    let body = place_order_body(o, &reference);
    let payload = core
        .call(
            Method::POST,
            "/v1/order/create",
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    let id = order_id_from(&payload);
    if id.is_empty() {
        return Err(AppError::Broker(
            "Groww accepted the request but returned no order id. Check the order book before retrying."
                .into(),
        ));
    }
    Ok(OrderResponse {
        order_id: id,
        message: payload
            .get("order_status")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// The web reported every modify and cancel as success, even on HTTP
/// errors (quirk 9.5); a refusal is an error here so the trader sees it.
pub async fn modify_order(
    core: &GrowwCore,
    auth: &AuthToken,
    m: &ResolvedModify,
) -> Result<OrderResponse> {
    let body = modify_order_body(m);
    let payload = core
        .call(
            Method::POST,
            "/v1/order/modify",
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    Ok(OrderResponse {
        order_id: m.order_id.clone(),
        message: Some(
            payload
                .get("order_status")
                .and_then(Value::as_str)
                .unwrap_or("MODIFICATION_REQUESTED")
                .to_string(),
        ),
    })
}

/// Segment of an order id (web `cancel_order` resolution order).
pub fn segment_from_id(order_id: &str) -> Option<&'static str> {
    if order_id.starts_with("GLTFO") || order_id.starts_with("GMKFO") {
        return Some(mapping::SEGMENT_FNO);
    }
    None
}

fn segment_from_order(o: &GrowwOrder) -> &'static str {
    match o.segment.to_ascii_uppercase().as_str() {
        "FNO" | "F&O" | "OPTIONS" | "FUTURES" => mapping::SEGMENT_FNO,
        _ => mapping::SEGMENT_CASH,
    }
}

pub async fn cancel_order(
    core: &GrowwCore,
    auth: &AuthToken,
    order_id: &str,
    segment: Option<&str>,
) -> Result<OrderResponse> {
    let segment = match segment.or_else(|| segment_from_id(order_id)) {
        Some(s) => s.to_string(),
        None => {
            // Look the order up; fall back to the web's id heuristics.
            let book = raw_orders(core, auth).await.unwrap_or_default();
            match book.iter().find(|o| o.groww_order_id == order_id) {
                Some(o) => segment_from_order(o).to_string(),
                None if ["CE", "PE", "FUT"].iter().any(|s| order_id.contains(s)) => {
                    mapping::SEGMENT_FNO.to_string()
                }
                None => mapping::SEGMENT_CASH.to_string(),
            }
        }
    };
    let body = json!({"segment": segment, "groww_order_id": order_id});
    let payload = core
        .call(
            Method::POST,
            "/v1/order/cancel",
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    let id = order_id_from(&payload);
    Ok(OrderResponse {
        order_id: if id.is_empty() {
            order_id.to_string()
        } else {
            id
        },
        message: payload
            .get("order_status")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Every order of one segment, page by page.
async fn segment_orders(
    core: &GrowwCore,
    auth: &AuthToken,
    segment: &str,
) -> Result<Vec<GrowwOrder>> {
    let mut out = Vec::new();
    for page in 0..MAX_PAGES {
        let path = format!(
            "/v1/order/list?segment={}&page={}&page_size={}",
            segment, page, PAGE_SIZE
        );
        let r = core
            .send(Method::GET, &path, auth, None, Category::Other, false)
            .await?;
        if !r.is_success() {
            if page == 0 && !r.status.is_success() {
                return Err(groww_error(&r));
            }
            break;
        }
        let list: Vec<GrowwOrder> = r
            .payload()
            .get("order_list")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        let n = list.len();
        out.extend(list);
        if n < PAGE_SIZE {
            break;
        }
    }
    Ok(out)
}

/// Both segments' orders (CASH then FNO), as the web concatenates them.
pub(crate) async fn raw_orders(core: &GrowwCore, auth: &AuthToken) -> Result<Vec<GrowwOrder>> {
    let mut all = segment_orders(core, auth, mapping::SEGMENT_CASH).await?;
    match segment_orders(core, auth, mapping::SEGMENT_FNO).await {
        Ok(f) => all.extend(f),
        Err(e @ AppError::Auth(_)) => return Err(e),
        Err(e) => tracing::warn!("Groww F&O order list failed: {}", e.code()),
    }
    Ok(all)
}

pub async fn get_order_book(core: &GrowwCore, auth: &AuthToken) -> Result<Vec<Order>> {
    Ok(mapping::map_orders(
        &raw_orders(core, auth).await?,
        &core.symbols,
    ))
}

pub async fn cancel_all_orders(core: &GrowwCore, auth: &AuthToken) -> Result<CancelAllResult> {
    let mut result = CancelAllResult::default();
    for o in raw_orders(core, auth).await? {
        if !mapping::is_cancellable(&o.order_status) {
            continue;
        }
        let seg = segment_from_order(&o);
        match cancel_order(core, auth, &o.groww_order_id, Some(seg)).await {
            Ok(_) => result.cancelled.push(o.groww_order_id),
            Err(e) => {
                tracing::warn!("Cancel of order {} failed: {}", o.groww_order_id, e.code());
                result.failed.push(o.groww_order_id)
            }
        }
    }
    Ok(result)
}

/// Trades of one order; `None` when Groww has none (404 / empty).
async fn order_trades(
    core: &GrowwCore,
    auth: &AuthToken,
    order: &GrowwOrder,
) -> Result<Option<Vec<GrowwTrade>>> {
    let seg = segment_from_id(&order.groww_order_id).unwrap_or(segment_from_order(order));
    let path = format!(
        "/v1/order/trades/{}?segment={}&page=0&page_size={}",
        urlencoding::encode(&order.groww_order_id),
        seg,
        TRADES_PAGE_SIZE
    );
    let r = core
        .send(Method::GET, &path, auth, None, Category::Other, false)
        .await?;
    if !r.is_success() {
        return Ok(None);
    }
    let list: Vec<GrowwTrade> = r
        .payload()
        .get("trade_list")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    Ok((!list.is_empty()).then_some(list))
}

/// Web `get_trade_book`: trades of every executed order, synthesised from
/// the order when Groww has no trade rows for a filled order.
pub async fn get_trade_book(core: &GrowwCore, auth: &AuthToken) -> Result<Vec<Trade>> {
    let orders = raw_orders(core, auth).await?;
    let mut out = Vec::new();
    for o in orders.iter().filter(|o| mapping::has_fills(o)) {
        let trades = match order_trades(core, auth, o).await {
            Ok(t) => t,
            Err(e @ AppError::Auth(_)) => return Err(e),
            Err(e) => {
                tracing::warn!("Groww trades for {} failed: {}", o.groww_order_id, e.code());
                None
            }
        };
        match trades {
            Some(ts) => out.extend(ts.iter().map(|t| mapping::map_trade(t, o, &core.symbols))),
            None if o.filled_quantity > 0 => out.push(mapping::map_trade(
                &mapping::synthetic_trade(o),
                o,
                &core.symbols,
            )),
            None => {}
        }
    }
    Ok(out)
}

/// Positions of one segment. The outer error is fatal (session gone); the
/// inner one is a refused read the caller decides whether to tolerate.
async fn segment_positions(
    core: &GrowwCore,
    auth: &AuthToken,
    segment: &str,
) -> Result<std::result::Result<Vec<Position>, AppError>> {
    let r = core
        .send(
            Method::GET,
            &format!("/v1/positions/user?segment={}", segment),
            auth,
            None,
            Category::Other,
            false,
        )
        .await?;
    if !r.is_success() {
        if mapping::says_no_positions(&r.error_message()) {
            return Ok(Ok(Vec::new()));
        }
        return Ok(Err(groww_error(&r)));
    }
    let rows: Vec<GrowwPosition> = r
        .payload()
        .get("positions")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    Ok(Ok(rows
        .iter()
        .map(|p| mapping::map_position(p, segment, &core.symbols))
        .collect()))
}

/// Both segments; a CASH failure fails the read, an FNO failure is
/// reported in `failed` (web `strict` / `failed_segments`).
pub(crate) async fn positions_with_failures(
    core: &GrowwCore,
    auth: &AuthToken,
) -> Result<(Vec<Position>, Vec<&'static str>)> {
    let mut all = segment_positions(core, auth, mapping::SEGMENT_CASH).await??;
    let mut failed = Vec::new();
    match segment_positions(core, auth, mapping::SEGMENT_FNO).await? {
        Ok(f) => all.extend(f),
        Err(e) => {
            tracing::warn!("Groww F&O positions could not be read: {}", e.code());
            failed.push(mapping::SEGMENT_FNO);
        }
    }
    Ok((all, failed))
}

pub async fn get_positions(core: &GrowwCore, auth: &AuthToken) -> Result<Vec<Position>> {
    Ok(positions_with_failures(core, auth).await?.0)
}

/// Net quantity for a smart order. A failed read of the segment the symbol
/// trades in is an error, never a flat 0 (web `PositionReadError`).
pub async fn get_open_position(
    core: &GrowwCore,
    auth: &AuthToken,
    symbol: &str,
    exchange: Exchange,
    product: Product,
) -> Result<i64> {
    let (positions, failed) = positions_with_failures(core, auth).await?;
    if failed.contains(&mapping::segment_of(exchange)) {
        return Err(AppError::Broker(
            "Groww did not return your F&O positions, so the open position cannot be read. Try again in a moment."
                .into(),
        ));
    }
    Ok(positions
        .iter()
        .find(|p| {
            p.symbol == symbol && p.exchange == exchange.as_str() && p.product == product.as_str()
        })
        .map(|p| i64::from(p.quantity))
        .unwrap_or(0))
}

pub async fn get_holdings(core: &GrowwCore, auth: &AuthToken) -> Result<Vec<Holding>> {
    let r = core
        .send(
            Method::GET,
            "/v1/holdings/user",
            auth,
            None,
            Category::Other,
            true,
        )
        .await?;
    if !r.is_success() {
        return Err(groww_error(&r));
    }
    let rows: Vec<GrowwHolding> = r
        .payload()
        .get("holdings")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    Ok(rows
        .iter()
        .map(|h| mapping::map_holding(h, &core.symbols))
        .collect())
}
