//! `/api/v1`: the external API (Python SDK, TradingView, Amibroker...).
//!
//! This wave implements `ping`, `analyzer` and `funds` to the golden
//! fixtures. The older handlers are mounted unchanged behind the same JSON
//! envelope, per-IP limiter and body limit; they are brought to the
//! contract in the next wave.

use crate::server::envelope::{apikey_only, error, json_response, not_found, read_json_object};
use crate::server::middleware::ClientIp;
use crate::server::ratelimit::Bucket;
use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use crate::webhook::handlers as legacy;
use crate::webhook::handlers::INVALID_API_KEY;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::Response,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

type Ctx = State<Arc<AppState>>;

/// Web rule (`get_auth_token_broker`): a valid key with no live broker
/// session is reported as an invalid key. An address that keeps sending bad
/// keys stops being checked at all for a minute (same answer, no Argon2).
fn authorize(ctx: &AppState, key: &str, ip: IpAddr) -> bool {
    let now = Instant::now();
    if ctx.limiter.is_exhausted(Bucket::ApiKeyFail, ip, now) {
        return false;
    }
    if ApiKeyService::is_valid(ctx, key) && ctx.is_broker_connected() {
        true
    } else {
        let _ = ctx.limiter.check(Bucket::ApiKeyFail, ip, now);
        false
    }
}

async fn apikey_body(ctx: &AppState, ip: IpAddr, req: Request) -> Result<String, Response> {
    let body = read_json_object(req, &()).await?;
    let key = apikey_only(&body).map_err(|f| f.into_response())?;
    if !authorize(ctx, &key, ip) {
        return Err(error(StatusCode::FORBIDDEN, INVALID_API_KEY));
    }
    Ok(key)
}

/// POST /api/v1/ping
pub async fn ping(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    if let Err(r) = apikey_body(&ctx, ip, req).await {
        return r;
    }
    let broker = ctx
        .get_broker_session()
        .map(|b| b.broker_id)
        .unwrap_or_default();
    json_response(
        StatusCode::OK,
        json!({"status": "success", "data": {"message": "pong", "broker": broker}}),
    )
}

/// POST /api/v1/analyzer
pub async fn analyzer(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    if let Err(r) = apikey_body(&ctx, ip, req).await {
        return r;
    }
    match crate::services::AnalyzerService::get_status(&ctx) {
        Ok(s) => json_response(
            StatusCode::OK,
            json!({"status": "success", "data": {
                "analyze_mode": s.analyze_mode, "mode": s.mode, "total_logs": s.total_logs,
            }}),
        ),
        Err(e) => {
            tracing::error!("Analyzer status failed: {}", e);
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "An unexpected error occurred",
            )
        }
    }
}

/// Funds in the web's shapes: sandbox mode returns numbers plus `mode`;
/// live returns the broker module's strings formatted to two decimals.
pub async fn funds_payload(ctx: &AppState) -> crate::error::Result<(Value, Option<&'static str>)> {
    if ctx.sqlite.get_analyze_mode().unwrap_or(false) {
        let f = ctx.sqlite.get_sandbox_funds()?;
        let last_reset = chrono::NaiveDateTime::parse_from_str(&f.updated_at, "%Y-%m-%d %H:%M:%S")
            .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or(f.updated_at.clone());
        return Ok((
            json!({
                "availablecash": f.available_cash,
                "collateral": 0.0,
                "grossexposure": f.used_margin,
                "last_reset": last_reset,
                "m2mrealized": 0.0,
                "m2munrealized": 0.0,
                "reset_count": 0,
                "today_realized_pnl": 0.0,
                "total_realized_pnl": 0.0,
                "totalpnl": 0.0,
                "utiliseddebits": f.used_margin,
            }),
            Some("analyze"),
        ));
    }
    let session = ctx
        .get_broker_session()
        .ok_or_else(|| crate::error::AppError::Auth("Broker not connected".into()))?;
    let broker = ctx.brokers.get(&session.broker_id).ok_or_else(|| {
        crate::error::AppError::NotFound("Broker-specific module not found".into())
    })?;
    let f = broker.get_funds(session.auth_token.expose()).await?;
    Ok((
        json!({
            "availablecash": format!("{:.2}", f.available_cash),
            "collateral": format!("{:.2}", f.collateral),
            "m2mrealized": "0.00",
            "m2munrealized": "0.00",
            "utiliseddebits": format!("{:.2}", f.used_margin),
        }),
        None,
    ))
}

/// POST /api/v1/funds
pub async fn funds(State(ctx): Ctx, ClientIp(ip): ClientIp, req: Request) -> Response {
    if let Err(r) = apikey_body(&ctx, ip, req).await {
        return r;
    }
    match funds_payload(&ctx).await {
        Ok((data, Some(mode))) => json_response(
            StatusCode::OK,
            json!({"status": "success", "data": data, "mode": mode}),
        ),
        Ok((data, None)) => {
            json_response(StatusCode::OK, json!({"status": "success", "data": data}))
        }
        Err(crate::error::AppError::NotFound(m)) => error(StatusCode::NOT_FOUND, m),
        Err(e) => {
            tracing::error!("Funds request failed: {}", e);
            error(StatusCode::INTERNAL_SERVER_ERROR, e.client_message())
        }
    }
}

/// 404 JSON for unknown `/api/v1` paths and wrong methods (web contract:
/// wrong method is 404, not 405).
pub async fn api_not_found(req: Request) -> Response {
    not_found(req.uri().path())
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/v1/ping", post(ping))
        .route("/api/v1/analyzer", post(analyzer))
        .route("/api/v1/funds", post(funds))
        .route("/api/v1/analyzer/toggle", post(legacy::toggle_analyzer))
        .route("/api/v1/placeorder", post(legacy::place_order))
        .route("/api/v1/placesmartorder", post(legacy::place_smart_order))
        .route("/api/v1/modifyorder", post(legacy::modify_order))
        .route("/api/v1/cancelorder", post(legacy::cancel_order))
        .route("/api/v1/cancelallorder", post(legacy::cancel_all_orders))
        .route("/api/v1/closeposition", post(legacy::close_position))
        .route("/api/v1/basketorder", post(legacy::place_basket_order))
        .route("/api/v1/splitorder", post(legacy::place_split_order))
        .route("/api/v1/orderstatus", post(legacy::get_order_status))
        .route("/api/v1/openposition", post(legacy::get_open_position))
        .route("/api/v1/orderbook", post(legacy::get_orderbook))
        .route("/api/v1/tradebook", post(legacy::get_tradebook))
        .route("/api/v1/positionbook", post(legacy::get_positionbook))
        .route("/api/v1/holdings", post(legacy::get_holdings))
        .route("/api/v1/quotes", post(legacy::get_quotes))
        .route("/api/v1/depth", post(legacy::get_depth))
        .route("/api/v1/symbol", post(legacy::get_symbol))
        .route("/api/v1/history", post(legacy::get_history))
        .route("/api/v1/intervals", post(legacy::get_intervals))
        .route("/api/v1/multiquotes", post(legacy::get_multiquotes))
        .route("/api/v1/search", post(legacy::search_symbols))
        .route("/api/v1/expiry", post(legacy::get_expiry))
        .route("/api/v1/instruments", get(legacy::get_instruments))
        .route(
            "/api/v1/syntheticfuture",
            post(legacy::get_synthetic_future),
        )
        .route("/api/v1/margin", post(legacy::get_margin))
        .route("/api/v1/optionchain", post(legacy::get_option_chain))
        .route("/api/v1/optiongreeks", post(legacy::get_option_greeks))
        .route("/api/v1/optionsorder", post(legacy::place_options_order))
        .route("/api/v1/optionsymbol", post(legacy::get_option_symbol))
        .route(
            "/api/v1/optionsmultiorder",
            post(legacy::place_options_multi_order),
        )
}
