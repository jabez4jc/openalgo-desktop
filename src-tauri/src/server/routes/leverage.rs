//! Web `blueprints/leverage.py`: one leverage value applied to crypto
//! futures orders (Delta Exchange). 0 means the broker's default.

use crate::db::sqlite::webui;
use crate::server::envelope::error;
use crate::server::routes::webui::{failed, ok, JsonBody};
use crate::state::AppState;
use axum::{extract::State, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// GET /leverage/api/current
pub async fn current(State(ctx): Ctx) -> Response {
    match ctx.sqlite.conn().and_then(|c| webui::leverage(&c)) {
        Ok(v) => ok(json!({"status": "success", "leverage": v})),
        Err(e) => failed(
            "Reading leverage",
            e,
            "Could not read the leverage setting. Try again.",
        ),
    }
}

/// POST /leverage/api/update (json: leverage)
pub async fn update(State(ctx): Ctx, body: JsonBody) -> Response {
    let bad = |m: &str| error(StatusCode::BAD_REQUEST, m);
    let v = match body.0.get("leverage") {
        None => return bad("Missing leverage field"),
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(Value::Bool(b)) => Some(*b as i64 as f64),
        Some(_) => None,
    };
    let Some(v) = v.filter(|f| f.is_finite()) else {
        return bad("Invalid leverage value");
    };
    if v < 0.0 {
        return bad("Leverage cannot be negative");
    }
    if v.fract() != 0.0 {
        return bad("Leverage must be a whole number");
    }
    if v > 1_000.0 {
        return bad("Invalid leverage value");
    }
    let lev = v as i64;
    match ctx.sqlite.conn().and_then(|c| webui::set_leverage(&c, lev)) {
        Ok(()) => {
            let label = if lev > 0 {
                format!("{}x", lev)
            } else {
                "Default".to_string()
            };
            ok(json!({"status": "success", "message": format!("Leverage set to {}", label)}))
        }
        Err(e) => failed(
            "Saving leverage",
            e,
            "Could not save the leverage setting. Try again.",
        ),
    }
}
