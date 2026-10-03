//! Order placement and management (web `place_order_service.py`,
//! `place_smart_order_service.py`, `modify_order_service.py`,
//! `cancel_order_service.py`, `cancel_all_order_service.py`,
//! `close_position_service.py`).
//!
//! Every function takes the request as loaded by its schema (types coerced,
//! defaults filled), decides the destination once (sandbox in analyzer mode
//! unless the caller passed `force_live`), and publishes the web's event.
//! Side effects (logs, Socket.IO, alerts) are subscribers on the bus.

use super::core::{
    analyzer_request, broker_handle, f, i, is_analyze, meta, mode_of, order_failed, publish, s,
    safe_request, BrokerHandle, Reply,
};
use crate::brokers::common::mapping::{Exchange, Product};
use crate::brokers::types::{ModifyOrderRequest, OrderRequest, ResolvedModify, ResolvedOrder};
use crate::error::AppError;
use crate::events::{Event, Mode};
use crate::sandbox::types::dec_from_f64;
use crate::state::AppState;
use serde_json::{json, Value};
use std::sync::OnceLock;

/// How the caller wants an order routed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Route {
    /// The caller already decided this goes to the live broker (an exit of
    /// a position it opened live); the analyzer toggle is not consulted.
    pub force_live: bool,
}

impl Route {
    pub const API: Route = Route { force_live: false };
    pub const LIVE: Route = Route { force_live: true };

    pub fn analyze(&self, ctx: &AppState) -> bool {
        !self.force_live && is_analyze(ctx)
    }
}

/// Semi-Auto mode routes new orders to the Action Center on the web. The
/// desktop has no Action Center yet, so a new order is refused rather than
/// sent live behind the trader's back.
pub fn semi_auto_refusal(ctx: &AppState) -> Option<Reply> {
    match super::apikey_service::ApiKeyService::order_mode(ctx) {
        Ok(m) if m == "semi_auto" => Some(Reply::error(
            403,
            "Your API key is in Semi-Auto mode, which needs the Action Center to approve orders. Switch the order mode to Auto on the API Key page to place orders from this app.",
        )),
        _ => None,
    }
}

fn semi_auto(ctx: &AppState) -> bool {
    matches!(
        super::apikey_service::ApiKeyService::order_mode(ctx).as_deref(),
        Ok("semi_auto")
    )
}

/// The web's status for a broker-side failure.
pub fn broker_error_reply(e: &AppError, internal: &str) -> Reply {
    match e {
        AppError::Broker(m) | AppError::Validation(m) => Reply::error(400, m.clone()),
        AppError::NotFound(m) => Reply::error(404, m.clone()),
        AppError::Auth(m) => Reply::error(403, m.clone()),
        AppError::Unsupported(_) => Reply::error(501, e.client_message()),
        _ => {
            tracing::error!("Broker call failed: {}", e);
            Reply::error(500, internal)
        }
    }
}

/// Uppercased action as the web stores it.
fn action(req: &Value) -> String {
    s(req, "action").to_ascii_uppercase()
}

/// The broker order request for a loaded place-order body.
pub fn broker_order(req: &Value) -> OrderRequest {
    OrderRequest {
        symbol: s(req, "symbol"),
        exchange: s(req, "exchange"),
        side: action(req),
        quantity: i(req, "quantity").clamp(0, i64::from(i32::MAX)) as i32,
        price: f(req, "price"),
        order_type: {
            let p = s(req, "pricetype");
            if p.is_empty() {
                "MARKET".into()
            } else {
                p
            }
        },
        product: {
            let p = s(req, "product");
            if p.is_empty() {
                "MIS".into()
            } else {
                p
            }
        },
        validity: "DAY".into(),
        trigger_price: Some(f(req, "trigger_price")),
        disclosed_quantity: Some(i(req, "disclosed_quantity").clamp(0, i64::from(i32::MAX)) as i32),
        amo: false,
    }
}

fn opt_dec(x: f64) -> Option<rust_decimal::Decimal> {
    (x != 0.0).then(|| dec_from_f64(x))
}

/// The sandbox order request for a loaded place-order body.
pub fn sandbox_order(req: &Value) -> crate::sandbox::OrderRequest {
    crate::sandbox::OrderRequest {
        symbol: s(req, "symbol"),
        exchange: s(req, "exchange"),
        action: action(req),
        quantity: i(req, "quantity"),
        price: opt_dec(f(req, "price")),
        trigger_price: opt_dec(f(req, "trigger_price")),
        price_type: s(req, "pricetype"),
        product: s(req, "product"),
        strategy: s(req, "strategy"),
    }
}

fn placed_event(mode: Mode, api_type: &str, req: &Value, request: Value, reply: &Reply) -> Event {
    Event::OrderPlaced {
        meta: meta(mode, api_type, request, &reply.body),
        strategy: s(req, "strategy"),
        symbol: s(req, "symbol"),
        exchange: s(req, "exchange"),
        action: action(req),
        quantity: i(req, "quantity"),
        pricetype: s(req, "pricetype"),
        product: s(req, "product"),
        orderid: reply
            .body
            .get("orderid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

/// Place one order on the live broker, resolving the OpenAlgo symbol once.
pub async fn place_live(h: &BrokerHandle, ctx: &AppState, req: &Value) -> Reply {
    let order = match ResolvedOrder::resolve(&broker_order(req), &ctx.symbols) {
        Ok(o) => o,
        Err(e) => return broker_error_reply(&e, "Failed to place order due to internal error"),
    };
    match h.broker.place_order(&h.auth, &order).await {
        Ok(r) if !r.order_id.is_empty() => {
            Reply::ok(json!({"status": "success", "orderid": r.order_id}))
        }
        Ok(r) => Reply::error(
            400,
            r.message
                .unwrap_or_else(|| "Failed to place order".to_string()),
        ),
        Err(e) => broker_error_reply(&e, "Failed to place order due to internal error"),
    }
}

/// `placeorder`.
pub async fn place_order(ctx: &AppState, req: &Value, route: Route) -> Reply {
    place_order_with(ctx, req, route, true).await
}

/// `placeorder`, optionally without the `order.placed` / `order.failed`
/// event (split legs of an options order report one completion event).
pub async fn place_order_with(ctx: &AppState, req: &Value, route: Route, emit: bool) -> Reply {
    if let Some(r) = semi_auto_refusal(ctx) {
        return r;
    }
    if route.analyze(ctx) {
        let reply = match ctx.sandbox.place_order(sandbox_order(req)).await {
            Ok(p) => Reply::from_ser(&p),
            Err(e) => Reply::sandbox(&e),
        };
        if emit {
            publish(
                ctx,
                placed_event(Mode::Analyze, "placeorder", req, safe_request(req), &reply),
            );
        }
        return reply;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let reply = place_live(&h, ctx, req).await;
    if !emit {
        return reply;
    }
    if reply.is_success() {
        publish(
            ctx,
            placed_event(Mode::Live, "placeorder", req, safe_request(req), &reply),
        );
    } else {
        order_failed(
            ctx,
            "placeorder",
            req,
            &reply,
            &s(req, "symbol"),
            &s(req, "exchange"),
        );
    }
    reply
}

// ------------------------------------------------------------------ smart

/// What a smart order should do to reach `position_size`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmartDecision {
    Place { action: String, quantity: i64 },
    NoAction(&'static str),
}

pub const NO_OPEN_POSITION: &str = "No OpenPosition Found. Not placing Exit order.";
pub const ALREADY_MATCHED: &str = "Positions Already Matched. No Action needed.";

/// The web's smart-order decision table (broker `place_smartorder_api`).
pub fn smart_decision(current: i64, target: i64, quantity: i64, action: &str) -> SmartDecision {
    if target == 0 && current == 0 && quantity != 0 {
        return SmartDecision::Place {
            action: action.to_ascii_uppercase(),
            quantity,
        };
    }
    if target == current {
        return SmartDecision::NoAction(if quantity == 0 {
            NO_OPEN_POSITION
        } else {
            ALREADY_MATCHED
        });
    }
    let (a, q) = if target == 0 && current > 0 {
        ("SELL", current.abs())
    } else if target == 0 && current < 0 {
        ("BUY", current.abs())
    } else if current == 0 {
        (if target > 0 { "BUY" } else { "SELL" }, target.abs())
    } else if target > current {
        ("BUY", target - current)
    } else {
        ("SELL", current - target)
    };
    SmartDecision::Place {
        action: a.to_string(),
        quantity: q,
    }
}

/// Striped locks so two smart orders on one position never both read the
/// same open quantity (bounded: a fixed number of stripes).
const STRIPES: usize = 64;

fn stripe(key: &str) -> &'static tokio::sync::Mutex<()> {
    static LOCKS: OnceLock<Vec<tokio::sync::Mutex<()>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| (0..STRIPES).map(|_| tokio::sync::Mutex::new(())).collect());
    let mut h: u64 = 1469598103934665603;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(1099511628211);
    }
    &locks[(h % STRIPES as u64) as usize]
}

fn no_action_event(mode: Mode, req: &Value, request: Value, reply: &Reply) -> Event {
    Event::OrderNoAction {
        meta: meta(mode, "placesmartorder", request, &reply.body),
        symbol: s(req, "symbol"),
        exchange: s(req, "exchange"),
        message: reply.message(),
    }
}

/// `placesmartorder`.
pub async fn place_smart_order(ctx: &AppState, req: &Value, route: Route) -> Reply {
    if let Some(r) = semi_auto_refusal(ctx) {
        return r;
    }
    let target = f(req, "position_size").trunc() as i64;
    if route.analyze(ctx) {
        let sreq = crate::sandbox::SmartOrderRequest {
            symbol: s(req, "symbol"),
            exchange: s(req, "exchange"),
            product: s(req, "product"),
            action: action(req),
            quantity: i(req, "quantity"),
            position_size: target,
            price: opt_dec(f(req, "price")),
            trigger_price: opt_dec(f(req, "trigger_price")),
            price_type: s(req, "pricetype"),
            strategy: s(req, "strategy"),
        };
        let request = analyzer_request(req, "placesmartorder");
        return match ctx.sandbox.place_smart_order(sreq).await {
            Ok(crate::sandbox::replies::SmartOrderReply::Placed(p)) => {
                let reply = Reply::from_ser(&p);
                publish(
                    ctx,
                    placed_event(Mode::Analyze, "placesmartorder", req, request, &reply),
                );
                reply
            }
            Ok(crate::sandbox::replies::SmartOrderReply::NoAction(m)) => {
                let reply = Reply::from_ser(&m);
                publish(ctx, no_action_event(Mode::Analyze, req, request, &reply));
                reply
            }
            Err(e) => {
                let reply = Reply::sandbox(&e);
                publish(
                    ctx,
                    placed_event(Mode::Analyze, "placesmartorder", req, request, &reply),
                );
                reply
            }
        };
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let symbol = s(req, "symbol");
    let (Ok(exchange), Ok(product)) = (
        s(req, "exchange").parse::<Exchange>(),
        s(req, "product").parse::<Product>(),
    ) else {
        return Reply::error(400, "Invalid exchange or product");
    };
    // Decide once, under the position's lock: read, decide, dispatch.
    let key = format!("{}:{}:{}", exchange, symbol, product);
    let _guard = stripe(&key).lock().await;
    let current = match h
        .broker
        .get_open_position(&h.auth, &symbol, exchange, product)
        .await
    {
        Ok(q) => q,
        Err(e) => {
            let reply = broker_error_reply(&e, "Failed to place smart order due to internal error");
            order_failed(
                ctx,
                "placesmartorder",
                req,
                &reply,
                &symbol,
                exchange.as_str(),
            );
            return reply;
        }
    };
    match smart_decision(current, target, i(req, "quantity"), &action(req)) {
        SmartDecision::NoAction(msg) => {
            let reply = Reply::ok(json!({"status": "success", "message": msg}));
            publish(
                ctx,
                no_action_event(Mode::Live, req, safe_request(req), &reply),
            );
            reply
        }
        SmartDecision::Place { action, quantity } => {
            let mut order = req.clone();
            if let Some(m) = order.as_object_mut() {
                m.insert("action".into(), json!(action));
                m.insert("quantity".into(), json!(quantity));
            }
            let reply = place_live(&h, ctx, &order).await;
            if reply.is_success() {
                publish(
                    ctx,
                    placed_event(
                        Mode::Live,
                        "placesmartorder",
                        req,
                        safe_request(req),
                        &reply,
                    ),
                );
            } else {
                order_failed(
                    ctx,
                    "placesmartorder",
                    req,
                    &reply,
                    &symbol,
                    exchange.as_str(),
                );
            }
            reply
        }
    }
}

// ------------------------------------------------------------------ modify / cancel

/// `modifyorder`.
pub async fn modify_order(ctx: &AppState, req: &Value, route: Route) -> Reply {
    let orderid = s(req, "orderid");
    let symbol = s(req, "symbol");
    let analyze = route.analyze(ctx);
    let modify_failed = |mode: Mode, request: Value, reply: &Reply| Event::OrderModifyFailed {
        meta: meta(mode, "modifyorder", request, &reply.body),
        symbol: symbol.clone(),
        orderid: orderid.clone(),
        error_message: reply.message(),
    };
    if !analyze && semi_auto(ctx) {
        let reply = Reply::error(
            403,
            "Modify order operation is not allowed in Semi-Auto mode. Please switch to Auto mode to modify orders.",
        );
        publish(ctx, modify_failed(Mode::Live, safe_request(req), &reply));
        return reply;
    }
    if analyze {
        let m = crate::sandbox::ModifyRequest {
            quantity: Some(i(req, "quantity")),
            price: Some(dec_from_f64(f(req, "price"))),
            trigger_price: Some(dec_from_f64(f(req, "trigger_price"))),
        };
        let request = analyzer_request(req, "modifyorder");
        return match ctx.sandbox.modify_order(&orderid, m).await {
            Ok(r) => {
                let reply = Reply::from_ser(&r);
                publish(
                    ctx,
                    Event::OrderModified {
                        meta: meta(Mode::Analyze, "modifyorder", request, &reply.body),
                        symbol: symbol.clone(),
                        exchange: s(req, "exchange"),
                        orderid: orderid.clone(),
                    },
                );
                reply
            }
            Err(e) => {
                let reply = Reply::sandbox(&e);
                publish(ctx, modify_failed(Mode::Analyze, request, &reply));
                reply
            }
        };
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let m = ModifyOrderRequest {
        symbol: symbol.clone(),
        exchange: s(req, "exchange"),
        action: action(req),
        product: s(req, "product"),
        pricetype: s(req, "pricetype"),
        quantity: i(req, "quantity").clamp(0, i64::from(i32::MAX)) as i32,
        price: f(req, "price"),
        trigger_price: f(req, "trigger_price"),
        disclosed_quantity: i(req, "disclosed_quantity").clamp(0, i64::from(i32::MAX)) as i32,
    };
    let result = match ResolvedModify::resolve(&orderid, &m, &ctx.symbols) {
        Ok(r) => h.broker.modify_order(&h.auth, &r).await,
        Err(e) => Err(e),
    };
    let reply = match result {
        Ok(_) => Reply::ok(json!({"status": "success", "orderid": orderid})),
        Err(e) => broker_error_reply(&e, "Failed to modify order due to internal error"),
    };
    if reply.is_success() {
        publish(
            ctx,
            Event::OrderModified {
                meta: meta(Mode::Live, "modifyorder", safe_request(req), &reply.body),
                symbol: symbol.clone(),
                exchange: s(req, "exchange"),
                orderid: orderid.clone(),
            },
        );
    } else {
        publish(ctx, modify_failed(Mode::Live, safe_request(req), &reply));
    }
    reply
}

/// `cancelorder`.
pub async fn cancel_order(ctx: &AppState, req: &Value, route: Route) -> Reply {
    let orderid = s(req, "orderid");
    let analyze = route.analyze(ctx);
    let failed = |mode: Mode, request: Value, reply: &Reply| Event::OrderCancelFailed {
        meta: meta(mode, "cancelorder", request, &reply.body),
        orderid: orderid.clone(),
        error_message: reply.message(),
    };
    if orderid.is_empty() {
        let reply = Reply::error(400, "Order ID is missing");
        publish(ctx, failed(mode_of(analyze), safe_request(req), &reply));
        return reply;
    }
    if !analyze && semi_auto(ctx) {
        let reply = Reply::error(
            403,
            "Cancel order operation is not allowed in Semi-Auto mode. Please switch to Auto mode to cancel orders.",
        );
        publish(ctx, failed(Mode::Live, safe_request(req), &reply));
        return reply;
    }
    if analyze {
        let request = analyzer_request(req, "cancelorder");
        return match ctx.sandbox.cancel_order(&orderid).await {
            Ok(r) => {
                let reply = Reply::from_ser(&r);
                publish(
                    ctx,
                    Event::OrderCancelled {
                        meta: meta(Mode::Analyze, "cancelorder", request, &reply.body),
                        orderid: orderid.clone(),
                        status: "success".into(),
                    },
                );
                reply
            }
            Err(e) => {
                let reply = Reply::sandbox(&e);
                publish(ctx, failed(Mode::Analyze, request, &reply));
                reply
            }
        };
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let reply = match h.broker.cancel_order(&h.auth, &orderid).await {
        Ok(_) => Reply::ok(json!({"status": "success", "orderid": orderid})),
        Err(e) => broker_error_reply(&e, "Failed to cancel order due to internal error"),
    };
    if reply.is_success() {
        publish(
            ctx,
            Event::OrderCancelled {
                meta: meta(Mode::Live, "cancelorder", safe_request(req), &reply.body),
                orderid: orderid.clone(),
                status: "success".into(),
            },
        );
    } else {
        publish(ctx, failed(Mode::Live, safe_request(req), &reply));
    }
    reply
}

fn all_cancelled_event(mode: Mode, request: Value, reply: &Reply) -> Event {
    let count = |k: &str| {
        reply
            .body
            .get(k)
            .and_then(Value::as_array)
            .map(|a| a.len() as i64)
            .unwrap_or(0)
    };
    Event::AllOrdersCancelled {
        meta: meta(mode, "cancelallorder", request, &reply.body),
        canceled_count: count("canceled_orders"),
        failed_count: count("failed_cancellations"),
    }
}

/// `cancelallorder`.
pub async fn cancel_all_orders(ctx: &AppState, req: &Value, route: Route) -> Reply {
    let analyze = route.analyze(ctx);
    if !analyze && semi_auto(ctx) {
        let reply = Reply::error(
            403,
            "Cancel all orders operation is not allowed in Semi-Auto mode. Please switch to Auto mode to cancel orders.",
        );
        publish(
            ctx,
            all_cancelled_event(Mode::Live, safe_request(req), &reply),
        );
        return reply;
    }
    if analyze {
        let reply = match ctx.sandbox.cancel_all_orders().await {
            Ok(r) => Reply::from_ser(&r),
            Err(e) => Reply::sandbox(&e),
        };
        publish(
            ctx,
            all_cancelled_event(
                Mode::Analyze,
                analyzer_request(req, "cancelallorder"),
                &reply,
            ),
        );
        return reply;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    match h.broker.cancel_all_orders(&h.auth).await {
        Ok(r) => {
            let reply = Reply::ok(json!({
                "status": "success",
                "canceled_orders": r.cancelled,
                "failed_cancellations": r.failed,
                "message": format!(
                    "Canceled {} orders. Failed to cancel {} orders.",
                    r.cancelled.len(),
                    r.failed.len()
                ),
            }));
            publish(
                ctx,
                all_cancelled_event(Mode::Live, safe_request(req), &reply),
            );
            reply
        }
        Err(e) => {
            let reply = broker_error_reply(&e, "Failed to cancel all orders due to internal error");
            order_failed(ctx, "cancelallorder", req, &reply, "", "");
            reply
        }
    }
}

/// `closeposition`.
pub async fn close_position(ctx: &AppState, req: &Value, route: Route) -> Reply {
    let analyze = route.analyze(ctx);
    let closed = |mode: Mode, request: Value, reply: &Reply| Event::PositionClosed {
        meta: meta(mode, "closeposition", request, &reply.body),
        message: Some(reply.message()),
    };
    if !analyze && semi_auto(ctx) {
        let reply = Reply::error(
            403,
            "Close position operation is not allowed in Semi-Auto mode. Please switch to Auto mode to close positions.",
        );
        publish(ctx, closed(Mode::Live, safe_request(req), &reply));
        return reply;
    }
    if analyze {
        let reply = match ctx.sandbox.close_all_positions().await {
            Ok(r) => Reply::from_ser(&r),
            Err(e) => Reply::sandbox(&e),
        };
        publish(
            ctx,
            closed(
                Mode::Analyze,
                analyzer_request(req, "closeposition"),
                &reply,
            ),
        );
        return reply;
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let reply = match h.broker.close_all_positions(&h.auth).await {
        Ok(r) if r.failed.is_empty() => {
            Reply::ok(json!({"status": "success", "message": "All Open Positions Squared Off"}))
        }
        Ok(r) => Reply::error(500, r.message()),
        Err(e) => broker_error_reply(&e, "Failed to close positions due to internal error"),
    };
    publish(ctx, closed(Mode::Live, safe_request(req), &reply));
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(a: &str, q: i64) -> SmartDecision {
        SmartDecision::Place {
            action: a.into(),
            quantity: q,
        }
    }

    /// The web's smart-order table, row by row.
    #[test]
    fn smart_order_decision_table() {
        // (current, target, quantity, action) -> decision
        type Row = ((i64, i64, i64, &'static str), SmartDecision);
        let rows: Vec<Row> = vec![
            ((0, 0, 5, "buy"), place("BUY", 5)),
            ((0, 0, 0, "BUY"), SmartDecision::NoAction(NO_OPEN_POSITION)),
            ((5, 5, 0, "BUY"), SmartDecision::NoAction(NO_OPEN_POSITION)),
            (
                (-3, -3, 3, "SELL"),
                SmartDecision::NoAction(ALREADY_MATCHED),
            ),
            ((0, 10, 10, "BUY"), place("BUY", 10)),
            ((0, -3, 3, "SELL"), place("SELL", 3)),
            ((10, 15, 5, "BUY"), place("BUY", 5)),
            ((15, 5, 10, "SELL"), place("SELL", 10)),
            ((5, 0, 0, "SELL"), place("SELL", 5)),
            ((-4, 0, 0, "BUY"), place("BUY", 4)),
            ((-4, 2, 6, "BUY"), place("BUY", 6)),
            ((4, -2, 6, "SELL"), place("SELL", 6)),
        ];
        for ((c, t, q, a), want) in rows {
            assert_eq!(
                smart_decision(c, t, q, a),
                want,
                "current {} target {}",
                c,
                t
            );
        }
    }

    #[test]
    fn stripes_are_stable_and_bounded() {
        let a = stripe("NSE:SBIN:MIS") as *const _;
        let b = stripe("NSE:SBIN:MIS") as *const _;
        assert_eq!(a, b);
    }

    #[test]
    fn broker_errors_map_to_web_statuses() {
        assert_eq!(
            broker_error_reply(&AppError::Broker("x".into()), "i").status,
            400
        );
        assert_eq!(
            broker_error_reply(&AppError::Unsupported("gtt"), "i").status,
            501
        );
        let r = broker_error_reply(&AppError::Internal("db".into()), "internal");
        assert_eq!(r.status, 500);
        assert_eq!(r.message(), "internal");
    }
}
