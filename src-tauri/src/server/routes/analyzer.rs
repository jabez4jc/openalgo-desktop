//! Web `blueprints/analyzer.py`: the sandbox mode request log (stats and
//! requests for a date range) and its CSV export.

use crate::server::envelope::error;
use crate::server::routes::webui::{download, failed, ok};
use crate::services::analyzer_log_service as svc;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use chrono::NaiveDate;
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

#[allow(clippy::result_large_err)]
fn requests(ctx: &AppState, q: &HashMap<String, String>) -> Result<Vec<Value>, Response> {
    let parse = |k: &str| -> Result<Option<NaiveDate>, Response> {
        match q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(Some)
                .map_err(|_| error(StatusCode::BAD_REQUEST, "Choose dates as YYYY-MM-DD.")),
        }
    };
    let today = ctx.now().with_timezone(&Kolkata).date_naive();
    let (from, to) = svc::range(today, parse("start_date")?, parse("end_date")?);
    svc::rows_between(ctx, &from, &to)
        .map(|rows| rows.iter().map(svc::format_request).collect())
        .map_err(|e| {
            failed(
                "Reading analyzer logs",
                e,
                "Could not load the analyzer logs. Try again.",
            )
        })
}

/// GET /analyzer/api/data
pub async fn data(State(ctx): Ctx, Query(q): Q) -> Response {
    let reqs = match requests(&ctx, &q) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let stats = match svc::stats(&ctx, ctx.now()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Analyzer stats failed: {}", e);
            json!({"total_requests": 0, "issues": {"total": 0, "by_type": {
                "rate_limit": 0, "invalid_symbol": 0, "missing_quantity": 0,
                "invalid_exchange": 0, "other": 0}}, "symbols": [], "sources": []})
        }
    };
    ok(json!({"status": "success", "data": {"stats": stats, "requests": reqs}}))
}

/// GET /analyzer/export
pub async fn export(State(ctx): Ctx, Query(q): Q) -> Response {
    let reqs = match requests(&ctx, &q) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let stamp = ctx.now().with_timezone(&Kolkata).format("%Y%m%d_%H%M%S");
    download(
        svc::csv(&reqs),
        "text/csv",
        &format!("analyzer_logs_{}.csv", stamp),
    )
}
