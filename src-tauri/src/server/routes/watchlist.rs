//! Web `blueprints/watchlist.py` and `blueprints/alerts.py`: the charting
//! terminal's watchlists and its alert firing log, per signed-in user.

use crate::db::sqlite::{alert_log, watchlist as store};
use crate::server::envelope::{error, json_response, not_found};
use crate::server::middleware::User;
use crate::server::routes::webui::{failed, ok, JsonBody};
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

pub const MAX_NAME_LENGTH: usize = 64;

#[allow(clippy::result_large_err)]
fn name_from(body: &JsonBody) -> Result<String, Response> {
    let name = match body.0.get("name") {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    };
    if name.is_empty() {
        return Err(error(StatusCode::BAD_REQUEST, "Name is required"));
    }
    if name.chars().count() > MAX_NAME_LENGTH {
        return Err(error(
            StatusCode::BAD_REQUEST,
            format!("Name must be {} characters or fewer", MAX_NAME_LENGTH),
        ));
    }
    Ok(name)
}

fn int_id(raw: &str) -> Option<i64> {
    (!raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))
        .then(|| raw.parse().ok())
        .flatten()
}

fn db_failed(what: &str, e: crate::error::AppError) -> Response {
    failed(what, e, "Could not update your watchlists. Try again.")
}

/// GET /watchlist/api/lists
pub async fn lists(State(ctx): Ctx, User(u): User) -> Response {
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::lists(&c, &u.username))
    {
        Ok(l) => ok(json!({"status": "success", "data": l})),
        Err(e) => {
            tracing::error!("Reading watchlists failed: {}", e);
            ok(json!({"status": "success", "data": []}))
        }
    }
}

/// POST /watchlist/api/lists (json: name, items?)
pub async fn create(State(ctx): Ctx, User(u): User, body: JsonBody) -> Response {
    let name = match name_from(&body) {
        Ok(n) => n,
        Err(r) => return r,
    };
    let items: Vec<(String, String)> = match body.0.get("items") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|i| {
                let s = |k: &str| {
                    i.get(k)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                };
                (s("symbol"), s("exchange"))
            })
            .collect(),
        Some(_) => return error(StatusCode::BAD_REQUEST, "items must be a list"),
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::create(&c, &u.username, &name, &items))
    {
        Ok(Some(l)) => json_response(StatusCode::CREATED, json!({"status": "success", "data": l})),
        Ok(None) => error(
            StatusCode::CONFLICT,
            format!("A list named \"{}\" already exists", name),
        ),
        Err(e) => db_failed("Creating a watchlist", e),
    }
}

/// PATCH /watchlist/api/lists/{id} (json: name)
pub async fn rename(
    State(ctx): Ctx,
    User(u): User,
    Path(id): Path<String>,
    body: JsonBody,
) -> Response {
    let Some(id) = int_id(&id) else {
        return not_found("/watchlist/api/lists");
    };
    let name = match name_from(&body) {
        Ok(n) => n,
        Err(r) => return r,
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::rename(&c, &u.username, id, &name))
    {
        Ok(true) => ok(json!({"status": "success"})),
        Ok(false) => error(
            StatusCode::CONFLICT,
            "List not found, or that name is already used",
        ),
        Err(e) => db_failed("Renaming a watchlist", e),
    }
}

/// DELETE /watchlist/api/lists/{id}
pub async fn delete(State(ctx): Ctx, User(u): User, Path(id): Path<String>) -> Response {
    let Some(id) = int_id(&id) else {
        return not_found("/watchlist/api/lists");
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::delete(&c, &u.username, id))
    {
        Ok(true) => ok(json!({"status": "success"})),
        Ok(false) => error(StatusCode::NOT_FOUND, "List not found"),
        Err(e) => db_failed("Deleting a watchlist", e),
    }
}

/// POST /watchlist/api/lists/{id}/clear
pub async fn clear(State(ctx): Ctx, User(u): User, Path(id): Path<String>) -> Response {
    let Some(id) = int_id(&id) else {
        return not_found("/watchlist/api/lists");
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::clear(&c, &u.username, id))
    {
        Ok(true) => ok(json!({"status": "success"})),
        Ok(false) => error(StatusCode::NOT_FOUND, "List not found"),
        Err(e) => db_failed("Clearing a watchlist", e),
    }
}

/// POST /watchlist/api/lists/{id}/items (json: symbol, exchange)
pub async fn add_item(
    State(ctx): Ctx,
    User(u): User,
    Path(id): Path<String>,
    body: JsonBody,
) -> Response {
    let Some(id) = int_id(&id) else {
        return not_found("/watchlist/api/lists");
    };
    let s = |k: &str| match body.0.get(k) {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    };
    let (symbol, exchange) = (s("symbol"), s("exchange"));
    if symbol.is_empty() || exchange.is_empty() {
        return error(StatusCode::BAD_REQUEST, "symbol and exchange are required");
    }
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::add_item(&c, &u.username, id, &symbol, &exchange))
    {
        Ok(Some(item)) => json_response(
            StatusCode::CREATED,
            json!({"status": "success", "data": item}),
        ),
        Ok(None) => error(
            StatusCode::CONFLICT,
            format!(
                "List not found, or it already holds {} instruments",
                store::MAX_ITEMS_PER_LIST
            ),
        ),
        Err(e) => db_failed("Adding to a watchlist", e),
    }
}

/// DELETE /watchlist/api/lists/{id}/items/{item_id}
pub async fn remove_item(
    State(ctx): Ctx,
    User(u): User,
    Path((id, item)): Path<(String, String)>,
) -> Response {
    let (Some(id), Some(item)) = (int_id(&id), int_id(&item)) else {
        return not_found("/watchlist/api/lists");
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::remove_item(&c, &u.username, id, item))
    {
        Ok(true) => ok(json!({"status": "success"})),
        Ok(false) => error(StatusCode::NOT_FOUND, "Instrument not found"),
        Err(e) => db_failed("Removing from a watchlist", e),
    }
}

/// PUT /watchlist/api/lists/{id}/items/order (json: order)
pub async fn reorder(
    State(ctx): Ctx,
    User(u): User,
    Path(id): Path<String>,
    body: JsonBody,
) -> Response {
    let Some(id) = int_id(&id) else {
        return not_found("/watchlist/api/lists");
    };
    let Some(Value::Array(order)) = body.0.get("order") else {
        return error(StatusCode::BAD_REQUEST, "order must be a list of ids");
    };
    let ids: Vec<i64> = order.iter().filter_map(Value::as_i64).collect();
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::reorder(&c, &u.username, id, &ids))
    {
        Ok(true) => ok(json!({"status": "success"})),
        Ok(false) => error(StatusCode::NOT_FOUND, "List not found"),
        Err(e) => db_failed("Reordering a watchlist", e),
    }
}

// ------------------------------------------------------------------ alerts

/// POST /alerts/fired
pub async fn alert_fired(State(ctx): Ctx, User(u): User, body: JsonBody) -> Response {
    let delivered: Vec<Value> = match body.0.get("delivered") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let Some(fire) = alert_log::Fire::from_payload(&body.0, &delivered) else {
        return error(
            StatusCode::BAD_REQUEST,
            "That alert could not be identified",
        );
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| alert_log::record(&c, &u.username, &fire, ctx.now()))
    {
        Ok(row) => ok(json!({"status": "success", "fire": row})),
        Err(e) => failed(
            "Recording an alert",
            e,
            "The alert fired but could not be added to the log",
        ),
    }
}

/// GET /alerts/log?limit=
pub async fn alert_log_list(
    State(ctx): Ctx,
    User(u): User,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let limit = q.get("limit").map(String::as_str);
    match ctx
        .sqlite
        .conn()
        .and_then(|c| alert_log::list(&c, &u.username, limit))
    {
        Ok(rows) => ok(json!({"status": "success", "fires": rows})),
        Err(e) => {
            tracing::error!("Reading the alert log failed: {}", e);
            ok(json!({"status": "success", "fires": []}))
        }
    }
}

/// DELETE /alerts/log?alertId=
pub async fn alert_log_clear(
    State(ctx): Ctx,
    User(u): User,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let alert = q.get("alertId").map(String::as_str);
    let removed = ctx
        .sqlite
        .conn()
        .and_then(|c| alert_log::clear(&c, &u.username, alert))
        .unwrap_or_else(|e| {
            tracing::error!("Clearing the alert log failed: {}", e);
            0
        });
    ok(json!({"status": "success", "removed": removed}))
}
