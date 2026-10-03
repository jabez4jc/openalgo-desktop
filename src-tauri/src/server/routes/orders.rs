//! Web `blueprints/orders.py` session routes: order actions from the pages
//! and the Action Center. CSRF is checked by the session layer; every route
//! needs the signed-in user.

use crate::server::api_v1::send;
use crate::server::envelope::{json_response, not_found};
use crate::server::middleware::User;
use crate::server::routes::webui::JsonBody;
use crate::services::{action_center_service as ac, ui_order_service as ui};
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn field(body: &JsonBody, k: &str) -> Option<String> {
    body.non_empty(k)
}

/// POST /close_position (json: symbol, exchange, product)
pub async fn close_position(State(ctx): Ctx, body: JsonBody) -> Response {
    let (Some(symbol), Some(exchange), Some(product)) = (
        field(&body, "symbol"),
        field(&body, "exchange"),
        field(&body, "product"),
    ) else {
        return send(crate::services::core::Reply::error(
            400,
            "Missing required parameters (symbol, exchange, product)",
        ));
    };
    send(ui::close_position(&ctx, &symbol, &exchange, &product).await)
}

/// POST /close_all_positions
pub async fn close_all_positions(State(ctx): Ctx) -> Response {
    send(ui::close_all_positions(&ctx).await)
}

/// POST /cancel_all_orders
pub async fn cancel_all_orders(State(ctx): Ctx) -> Response {
    send(ui::cancel_all_orders(&ctx).await)
}

/// POST /cancel_order (json: orderid)
pub async fn cancel_order(State(ctx): Ctx, body: JsonBody) -> Response {
    send(ui::cancel_order(&ctx, &body.0).await)
}

/// POST /modify_order
pub async fn modify_order(State(ctx): Ctx, body: JsonBody) -> Response {
    send(ui::modify_order(&ctx, &body.0).await)
}

/// POST /modify_gtt_order
pub async fn modify_gtt_order(State(ctx): Ctx, body: JsonBody) -> Response {
    send(ui::modify_gtt_order(&ctx, &body.0).await)
}

/// POST /cancel_gtt_order
pub async fn cancel_gtt_order(State(ctx): Ctx, body: JsonBody) -> Response {
    send(ui::cancel_gtt_order(&ctx, &body.0).await)
}

// ------------------------------------------------------------------ Action Center

/// `<int:order_id>`: anything else is not a route (404), as in Flask.
#[allow(clippy::result_large_err)]
fn order_id(raw: &str, path: &str) -> Result<i64, Response> {
    if !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) {
        raw.parse::<i64>().map_err(|_| not_found(path))
    } else {
        Err(not_found(path))
    }
}

/// POST /action-center/approve/{id}
pub async fn approve(State(ctx): Ctx, User(u): User, Path(id): Path<String>) -> Response {
    let id = match order_id(&id, "/action-center/approve") {
        Ok(i) => i,
        Err(r) => return r,
    };
    send(ac::approve(&ctx, id, &u.username).await)
}

/// POST /action-center/reject/{id} (json: reason)
pub async fn reject(
    State(ctx): Ctx,
    User(u): User,
    Path(id): Path<String>,
    body: JsonBody,
) -> Response {
    let id = match order_id(&id, "/action-center/reject") {
        Ok(i) => i,
        Err(r) => return r,
    };
    let reason = match body.0.get("reason") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "No reason provided".to_string(),
        Some(other) => other.to_string(),
    };
    send(ac::reject(&ctx, id, &u.username, &reason))
}

/// DELETE /action-center/delete/{id}
pub async fn delete(State(ctx): Ctx, User(u): User, Path(id): Path<String>) -> Response {
    let id = match order_id(&id, "/action-center/delete") {
        Ok(i) => i,
        Err(r) => return r,
    };
    send(ac::delete(&ctx, id, &u.username))
}

/// GET /action-center/count
pub async fn count(State(ctx): Ctx, User(u): User) -> Response {
    send(ac::count(&ctx, &u.username))
}

/// POST /action-center/approve-all
pub async fn approve_all(State(ctx): Ctx, User(u): User) -> Response {
    send(ac::approve_all(&ctx, &u.username).await)
}

/// GET /action-center/api/data?status=pending|approved|rejected|all
pub async fn data(
    State(ctx): Ctx,
    User(u): User,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let status = q.get("status").map(String::as_str).unwrap_or("pending");
    let filter = (!status.is_empty() && status != "all").then_some(status);
    match ac::data(&ctx, &u.username, filter) {
        Ok(d) => json_response(StatusCode::OK, json!({"status": "success", "data": d})),
        Err(e) => {
            tracing::error!("Action Center data failed: {}", e);
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({
                    "status": "error",
                    "message": "Could not load the Action Center. Try again.",
                    "data": {"orders": [], "statistics": {
                        "total_pending": 0, "total_approved": 0, "total_rejected": 0,
                        "total_buy_orders": 0, "total_sell_orders": 0,
                    }},
                }),
            )
        }
    }
}
