//! API key page (web `blueprints/apikey.py`) and desktop server settings.

use crate::server::envelope::{error, json_response};
use crate::server::form::FormData;
use crate::server::middleware::User;
use crate::services::apikey_service::ApiKeyService;
use crate::state::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// GET /apikey (Accept: application/json). Returns the key to the signed-in
/// session only, as the web does: the frontend sends it in `/api/v1` bodies.
pub async fn get_apikey(State(ctx): Ctx, User(u): User) -> Response {
    let key = match ApiKeyService::current(&ctx) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };
    let mode = ApiKeyService::order_mode(&ctx).unwrap_or_else(|_| "auto".into());
    json_response(
        StatusCode::OK,
        json!({
            "login_username": u.username,
            "has_api_key": key.is_some(),
            "api_key": key.as_ref().map(|k| k.expose()),
            "order_mode": mode,
        }),
    )
}

/// POST /apikey (json: user_id) -> new key.
pub async fn regenerate(State(ctx): Ctx, User(u): User, form: FormData) -> Response {
    if form.non_empty("user_id").is_none() {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({"error": "User ID is required"}),
        );
    }
    let c2 = ctx.clone();
    let name = u.username.clone();
    match tokio::task::spawn_blocking(move || ApiKeyService::regenerate(&c2, &name)).await {
        Ok(Ok(key)) => {
            tracing::info!("API key regenerated");
            json_response(
                StatusCode::OK,
                json!({
                    "message": "API key updated successfully.",
                    "api_key": key.expose(),
                    "key_id": 1,
                }),
            )
        }
        _ => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Failed to update API key"}),
        ),
    }
}

/// POST /apikey/mode (json: user_id, mode)
pub async fn set_mode(State(ctx): Ctx, form: FormData) -> Response {
    if form.non_empty("user_id").is_none() {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({"error": "User ID is required"}),
        );
    }
    let mode = form.non_empty("mode").unwrap_or_default();
    if mode != "auto" && mode != "semi_auto" {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({"error": "Invalid mode. Must be \"auto\" or \"semi_auto\""}),
        );
    }
    match ApiKeyService::set_order_mode(&ctx, &mode) {
        Ok(true) => json_response(
            StatusCode::OK,
            json!({"message": format!("Order mode updated to {}", mode), "mode": mode}),
        ),
        _ => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "Failed to update order mode"}),
        ),
    }
}

/// GET /api/desktop/settings: listener settings and status (desktop only).
pub async fn get_server_settings(State(ctx): Ctx) -> Response {
    let cfg = ctx.server_config();
    let status = ctx.server_status.read().clone();
    json_response(
        StatusCode::OK,
        json!({"status": "success", "data": {
            "http_port": cfg.http_port,
            "ws_port": cfg.ws_port,
            "bind_host": cfg.bind_host,
            "allow_lan": !cfg.is_loopback(),
            "host_server": cfg.host_server,
            "ngrok_allow": cfg.ngrok_allow,
            "session_expiry_time": format!("{:02}:{:02}", cfg.session_expiry_hour, cfg.session_expiry_minute),
            "dev_ports": cfg.dev_ports,
            "key_mode": ctx.security.mode(),
            "server_status": status,
        }}),
    )
}

/// POST /api/desktop/settings (http_port, ws_port, allow_lan). Saved now,
/// applied when the app restarts its listener.
pub async fn update_server_settings(State(ctx): Ctx, form: FormData) -> Response {
    let port = |k: &str| -> Result<Option<u16>, Response> {
        match form.non_empty(k) {
            None => Ok(None),
            Some(v) => v.parse::<u16>().map(Some).map_err(|_| {
                error(
                    StatusCode::BAD_REQUEST,
                    "Choose a port number between 1024 and 65535.",
                )
            }),
        }
    };
    let http_port = match port("http_port") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let ws_port = match port("ws_port") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let bind_host = form.get("allow_lan").map(|v| {
        if v.eq_ignore_ascii_case("true") {
            "0.0.0.0".to_string()
        } else {
            "127.0.0.1".to_string()
        }
    });
    let update = crate::config::ServerConfigUpdate {
        http_port,
        ws_port,
        bind_host,
        ..Default::default()
    };
    let res = ctx
        .sqlite
        .conn()
        .and_then(|c| crate::config::save(&c, &update));
    match res {
        Ok(()) => {
            let _ = ctx.reload_config();
            json_response(
                StatusCode::OK,
                json!({
                    "status": "success",
                    "message": "Saved. OpenAlgo will use the new settings after the server restarts.",
                    "restart_required": true,
                }),
            )
        }
        Err(crate::error::AppError::Validation(m)) => error(StatusCode::BAD_REQUEST, m),
        Err(e) => e.into_response(),
    }
}
