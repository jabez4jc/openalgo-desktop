//! Web `blueprints/strategy_portfolio.py`: saved Strategy Builder
//! strategies in the `mytrades` and `simulation` watchlists.

use crate::db::sqlite::strategy_portfolio::{self as store, Entry, WATCHLISTS};
use crate::server::envelope::{error, not_found};
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

const SAVE_FAILED: &str = "Could not save the strategy. Try again.";

fn int_id(raw: &str) -> Option<i64> {
    (!raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))
        .then(|| raw.parse().ok())
        .flatten()
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// A scalar as the text the web would store.
fn as_text(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    }
}

/// Web `_validate_payload`, then the entry to save.
#[allow(clippy::result_large_err)]
fn entry_from(b: &JsonBody) -> Result<Entry, Response> {
    let bad = |m: String| error(StatusCode::BAD_REQUEST, m);
    for field in ["name", "watchlist", "underlying", "exchange"] {
        if !truthy(b.0.get(field)) {
            return Err(bad(format!("'{}' is required", field)));
        }
    }
    let watchlist = b.0.get("watchlist").and_then(Value::as_str).unwrap_or("");
    if !WATCHLISTS.contains(&watchlist) {
        return Err(bad(
            "watchlist must be one of ['mytrades', 'simulation']".into()
        ));
    }
    let legs = match b.0.get("legs") {
        Some(Value::Array(a)) if !a.is_empty() => Value::Array(a.clone()),
        _ => return Err(bad("at least one leg is required".into())),
    };
    let Some(Value::String(name)) = b.0.get("name") else {
        return Err(bad("Give the strategy a name.".into()));
    };
    if name.chars().count() > 120 {
        return Err(bad("name too long (max 120 chars)".into()));
    }
    Ok(Entry {
        watchlist: watchlist.to_string(),
        name: name.trim().to_string(),
        underlying: as_text(b.0.get("underlying")).unwrap_or_default(),
        exchange: as_text(b.0.get("exchange")).unwrap_or_default(),
        expiry: as_text(b.0.get("expiry")),
        legs,
        notes: as_text(b.0.get("notes")),
    })
}

/// GET /api/strategy-portfolio?watchlist=
pub async fn list(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    let wl = q
        .get("watchlist")
        .map(String::as_str)
        .filter(|s| !s.is_empty());
    if let Some(w) = wl {
        if !WATCHLISTS.contains(&w) {
            return error(StatusCode::BAD_REQUEST, "invalid watchlist");
        }
    }
    let items = match ctx.sqlite.conn().and_then(|c| store::list(&c, wl)) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Reading the strategy portfolio failed: {}", e);
            Vec::new()
        }
    };
    ok(json!({"status": "success", "items": items}))
}

/// GET /api/strategy-portfolio/{id}
pub async fn get(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    let Some(id) = int_id(&id) else {
        return not_found("/api/strategy-portfolio");
    };
    match ctx.sqlite.conn().and_then(|c| store::get(&c, id)) {
        Ok(Some(item)) => ok(json!({"status": "success", "item": item})),
        Ok(None) => error(StatusCode::NOT_FOUND, "not found"),
        Err(e) => {
            tracing::error!("Reading strategy {} failed: {}", id, e);
            error(StatusCode::NOT_FOUND, "not found")
        }
    }
}

/// POST /api/strategy-portfolio
pub async fn create(State(ctx): Ctx, body: JsonBody) -> Response {
    let e = match entry_from(&body) {
        Ok(e) => e,
        Err(r) => return r,
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::save(&c, None, &e, ctx.now()))
    {
        Ok(Some(item)) => ok(json!({"status": "success", "item": item})),
        Ok(None) => error(StatusCode::INTERNAL_SERVER_ERROR, SAVE_FAILED),
        Err(err) => failed("Saving a strategy", err, SAVE_FAILED),
    }
}

/// PUT /api/strategy-portfolio/{id}
pub async fn update(State(ctx): Ctx, Path(id): Path<String>, body: JsonBody) -> Response {
    let Some(id) = int_id(&id) else {
        return not_found("/api/strategy-portfolio");
    };
    let e = match entry_from(&body) {
        Ok(e) => e,
        Err(r) => return r,
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| store::save(&c, Some(id), &e, ctx.now()))
    {
        Ok(Some(item)) => ok(json!({"status": "success", "item": item})),
        Ok(None) => error(StatusCode::NOT_FOUND, "not found"),
        Err(err) => failed("Updating a strategy", err, SAVE_FAILED),
    }
}

/// DELETE /api/strategy-portfolio/{id}
pub async fn delete(State(ctx): Ctx, Path(id): Path<String>) -> Response {
    let Some(id) = int_id(&id) else {
        return not_found("/api/strategy-portfolio");
    };
    match ctx.sqlite.conn().and_then(|c| store::delete(&c, id)) {
        Ok(true) => ok(json!({"status": "success"})),
        Ok(false) => error(StatusCode::NOT_FOUND, "not found"),
        Err(err) => failed(
            "Deleting a strategy",
            err,
            "Could not delete the strategy. Try again.",
        ),
    }
}
