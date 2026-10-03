//! Web `blueprints/sandbox.py`: sandbox settings, reset, the square-off
//! schedule, My P&L and its CSV exports, over the sandbox engine.

use crate::sandbox::SandboxError;
use crate::server::envelope::{error, json_response};
use crate::server::routes::webui::{download, ok, JsonBody};
use crate::services::sandbox_export_service::{self as export, Export};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Response,
};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

fn sandbox_error(what: &str, e: SandboxError) -> Response {
    if e.http_status >= 500 {
        tracing::error!("{} failed: {}", what, e.message);
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not {}. Try again.", what),
        );
    }
    error(
        StatusCode::from_u16(e.http_status).unwrap_or(StatusCode::BAD_REQUEST),
        e.message,
    )
}

fn ser<T: serde::Serialize>(v: &T) -> Response {
    match serde_json::to_value(v) {
        Ok(b) => ok(b),
        Err(e) => {
            tracing::error!("Could not serialise a sandbox reply: {}", e);
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong. Try again.",
            )
        }
    }
}

/// GET /sandbox/api/configs
pub async fn configs(State(ctx): Ctx) -> Response {
    match ctx.sandbox.configs().await {
        Ok(c) => ok(json!({"status": "success", "configs": c})),
        Err(e) => sandbox_error("load the sandbox settings", e),
    }
}

/// POST /sandbox/update (json: config_key, config_value)
pub async fn update(State(ctx): Ctx, body: JsonBody) -> Response {
    let key = body.non_empty("config_key");
    let value = match body.0.get("config_value") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => Some(other.to_string()),
    };
    let (Some(key), Some(value)) = (key, value) else {
        return error(
            StatusCode::BAD_REQUEST,
            "Missing config_key or config_value",
        );
    };
    match ctx.sandbox.update_config(&key, &value).await {
        Ok(m) => ser(&m),
        Err(e) => sandbox_error("save the sandbox setting", e),
    }
}

/// POST /sandbox/reset
pub async fn reset(State(ctx): Ctx) -> Response {
    match ctx.sandbox.reset().await {
        Ok(m) => ser(&m),
        Err(e) if e.http_status == 409 => error(StatusCode::CONFLICT, e.message),
        Err(e) => sandbox_error("reset the sandbox", e),
    }
}

/// POST /sandbox/reload-squareoff
pub async fn reload_squareoff(State(ctx): Ctx) -> Response {
    ser(&ctx.sandbox.reload_squareoff().await)
}

/// GET /sandbox/squareoff-status
pub async fn squareoff_status(State(ctx): Ctx) -> Response {
    ser(&ctx.sandbox.squareoff_status().await)
}

/// GET /sandbox/mypnl/api/data
pub async fn mypnl(State(ctx): Ctx) -> Response {
    match ctx.sandbox.mypnl().await {
        Ok(r) => ser(&r),
        Err(e) => sandbox_error("load your sandbox P&L", e),
    }
}

/// GET /sandbox/mypnl/export/{daily|positions|holdings|trades}
pub async fn mypnl_export(State(ctx): Ctx, Path(kind): Path<String>) -> Response {
    let Some(which) = Export::parse(&kind) else {
        return crate::server::envelope::not_found(&format!("/sandbox/mypnl/export/{kind}"));
    };
    match export::export(&ctx.sandbox, which).await {
        Ok(Some(csv)) => {
            let stamp = ctx.now().with_timezone(&Kolkata).format("%Y%m%d_%H%M%S");
            download(
                csv,
                "text/csv",
                &format!("sandbox_{}_{}.csv", which.stem(), stamp),
            )
        }
        Ok(None) => json_response(
            StatusCode::NOT_FOUND,
            json!({"status": "error", "message": which.empty_message()}),
        ),
        Err(e) => sandbox_error("export the data", e),
    }
}
