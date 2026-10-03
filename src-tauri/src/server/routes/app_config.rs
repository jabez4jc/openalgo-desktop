//! Web `app.py` `/api/config/host`: the base address shown in webhook URLs.

use crate::server::routes::webui::ok;
use crate::state::AppState;
use axum::{extract::State, response::Response};
use serde_json::json;
use std::sync::Arc;

/// GET /api/config/host
pub async fn host(State(ctx): State<Arc<AppState>>) -> Response {
    let cfg = ctx.server_config();
    let host_server = match &cfg.host_server {
        Some(h) => h.trim_end_matches('/').to_string(),
        None => format!("http://127.0.0.1:{}", ctx.listening_port()),
    };
    let lower = host_server.to_ascii_lowercase();
    let is_localhost = ["localhost", "127.0.0.1", "0.0.0.0"]
        .iter()
        .any(|l| lower.contains(l));
    ok(json!({"host_server": host_server, "is_localhost": is_localhost}))
}
