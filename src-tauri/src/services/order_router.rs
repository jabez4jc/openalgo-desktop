//! Semi-Auto order routing (web `services/order_router_service.py`).
//!
//! When the API key's order mode is Semi-Auto, a new order from an API-key
//! client is not executed: it is written to the Action Center
//! (`pending_orders`) and waits for the trader to approve or reject it.
//! Modify, cancel, close and every read execute immediately, as on the web
//! (the modify and cancel family are refused in Semi-Auto by their services).
//!
//! The database row is authoritative; `pending_order_created` is published
//! on the bus after the commit, and the Socket.IO subscriber pushes it.

use super::core::{publish, Reply};
use crate::db::sqlite::{action_center, user};
use crate::events::Event;
use crate::state::AppState;
use serde_json::{json, Value};

/// Operations that never queue (web `IMMEDIATE_EXECUTION_OPERATIONS`).
pub const IMMEDIATE_EXECUTION_OPERATIONS: &[&str] = &[
    "closeallpositions",
    "closeposition",
    "cancelorder",
    "cancelallorder",
    "modifyorder",
    "orderstatus",
    "orderbook",
    "tradebook",
    "positions",
    "holdings",
    "funds",
    "openposition",
    "modifygttorder",
    "cancelgttorder",
    "gttorderbook",
];

pub const QUEUED_MESSAGE: &str = "Order queued for approval in Action Center";

/// Web `should_route_to_pending`: Semi-Auto mode and a queueable operation.
/// Any failure to read the mode answers "execute" (the web's default).
pub fn should_route_to_pending(ctx: &AppState, api_type: &str) -> bool {
    if IMMEDIATE_EXECUTION_OPERATIONS.contains(&api_type.to_ascii_lowercase().as_str()) {
        return false;
    }
    match super::apikey_service::ApiKeyService::order_mode(ctx) {
        Ok(m) => m == "semi_auto",
        Err(e) => {
            tracing::error!("Could not read the order mode: {}", e);
            false
        }
    }
}

/// The owner of the API key (single-user app: the account's username).
fn owner(ctx: &AppState) -> crate::error::Result<Option<String>> {
    let conn = ctx.sqlite.conn()?;
    Ok(user::find_first(&conn)?.map(|u| u.username))
}

/// Web `queue_order`: store the request (without the key) and announce it.
pub fn queue_order(ctx: &AppState, api_type: &str, req: &Value) -> Reply {
    let user_id = match owner(ctx) {
        Ok(Some(u)) => u,
        Ok(None) => {
            tracing::warn!("Semi-Auto order with no account to queue it for");
            return Reply::error(403, "Invalid API key");
        }
        Err(e) => {
            tracing::error!("Could not read the account for a queued order: {}", e);
            return Reply::error(500, "Failed to queue order");
        }
    };
    let mut clean = req.clone();
    if let Some(m) = clean.as_object_mut() {
        m.remove("apikey");
        m.remove("api_key");
    }
    let created = ctx
        .sqlite
        .conn()
        .and_then(|c| action_center::create(&c, &user_id, api_type, &clean.to_string(), ctx.now()));
    match created {
        Ok(id) => {
            tracing::info!(
                "Order queued for approval: pending order {} ({})",
                id,
                api_type
            );
            publish(
                ctx,
                Event::PendingOrderCreated {
                    payload: json!({
                        "pending_order_id": id,
                        "user_id": user_id,
                        "api_type": api_type,
                        "message": format!("New {} order queued for approval", api_type),
                    }),
                },
            );
            Reply::ok(json!({
                "status": "success",
                "message": QUEUED_MESSAGE,
                "mode": "semi_auto",
                "pending_order_id": id,
            }))
        }
        Err(e) => {
            tracing::error!("Could not queue a Semi-Auto order: {}", e);
            Reply::error(500, "Failed to queue order")
        }
    }
}

/// Queue when Semi-Auto applies; `None` means execute now.
pub fn queue_if_semi_auto(ctx: &AppState, api_type: &str, req: &Value) -> Option<Reply> {
    should_route_to_pending(ctx, api_type).then(|| queue_order(ctx, api_type, req))
}
