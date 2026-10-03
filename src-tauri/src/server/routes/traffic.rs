//! Web `blueprints/traffic.py`: the request log written by the monitoring
//! middleware (`services::monitor`).

use crate::db::sqlite::monitor::{self as store, TrafficRow};
use crate::server::routes::webui::{csv_row, download, failed, ok};
use crate::services::security_service::web_time;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    response::Response,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// The endpoints the web's stats break out.
const ENDPOINTS: &[&str] = &[
    "placeorder",
    "placesmartorder",
    "modifyorder",
    "cancelorder",
    "quotes",
    "history",
    "depth",
    "intervals",
    "funds",
    "orderbook",
    "tradebook",
    "positionbook",
    "holdings",
    "basketorder",
    "splitorder",
    "orderstatus",
    "openposition",
];

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn row_json(t: &TrafficRow) -> Value {
    json!({
        "timestamp": web_time(&t.timestamp),
        "client_ip": t.client_ip,
        "method": t.method,
        "path": t.path,
        "status_code": t.status_code,
        "duration_ms": r2(t.duration_ms),
        "host": t.host,
        "error": t.error,
    })
}

fn drain(ctx: &AppState) {
    ctx.monitor.drain_now(ctx);
}

/// GET /traffic/api/logs?limit= (newest first, at most 1000)
pub async fn logs(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    drain(&ctx);
    let limit = q
        .get("limit")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    match ctx
        .logs
        .conn()
        .and_then(|c| store::recent_traffic(&c, Some(limit)))
    {
        Ok(rows) => ok(Value::Array(rows.iter().map(row_json).collect())),
        Err(e) => failed(
            "Reading traffic logs",
            e,
            "Could not load the traffic log. Try again.",
        ),
    }
}

/// GET /traffic/api/stats
pub async fn stats(State(ctx): Ctx) -> Response {
    drain(&ctx);
    let r = ctx.logs.conn().and_then(|c| {
        let all = store::traffic_summary(&c, None)?;
        let api = store::traffic_summary(&c, Some("/api/v1/%"))?;
        let mut endpoints = Map::new();
        for e in ENDPOINTS {
            let (t, err, avg) = store::traffic_summary(&c, Some(&format!("/api/v1/{}%", e)))?;
            endpoints.insert(
                e.to_string(),
                json!({"total": t, "errors": err, "avg_duration": r2(avg)}),
            );
        }
        Ok((all, api, endpoints))
    });
    match r {
        Ok((all, api, endpoints)) => ok(json!({
            "overall": {"total_requests": all.0, "error_requests": all.1, "avg_duration": r2(all.2)},
            "api": {"total_requests": api.0, "error_requests": api.1, "avg_duration": r2(api.2)},
            "endpoints": endpoints,
        })),
        Err(e) => failed(
            "Traffic stats",
            e,
            "Could not load traffic statistics. Try again.",
        ),
    }
}

/// GET /traffic/export
pub async fn export(State(ctx): Ctx) -> Response {
    drain(&ctx);
    let rows = match ctx
        .logs
        .conn()
        .and_then(|c| store::recent_traffic(&c, None))
    {
        Ok(r) => r,
        Err(e) => {
            return failed(
                "Exporting traffic logs",
                e,
                "Could not export the traffic log. Try again.",
            )
        }
    };
    let mut out = csv_row(
        &[
            "Timestamp",
            "Client IP",
            "Method",
            "Path",
            "Status Code",
            "Duration (ms)",
            "Host",
            "Error",
        ]
        .map(String::from),
    );
    for t in &rows {
        out.push_str(&csv_row(&[
            web_time(&t.timestamp),
            t.client_ip.clone(),
            t.method.clone(),
            t.path.clone(),
            t.status_code.to_string(),
            r2(t.duration_ms).to_string(),
            t.host.clone().unwrap_or_default(),
            t.error.clone().unwrap_or_default(),
        ]));
    }
    download(out, "text/csv", "traffic_logs.csv")
}
