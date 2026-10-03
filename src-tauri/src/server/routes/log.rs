//! Web `blueprints/log.py`: the API order log (live `/api/v1` calls, from
//! `logs.db` `order_logs`), paged as JSON and exported as CSV. Request
//! bodies were stored without the API key; it is removed again here.

use crate::db::sqlite::monitor::{self as store, OrderLogRow};
use crate::server::envelope::error;
use crate::server::routes::webui::{csv_row, download, failed, ok};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Response,
};
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

const PER_PAGE: i64 = 20;

fn utc_iso_of_ist_midnight(d: NaiveDate) -> String {
    Kolkata
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default())
        .single()
        .map(|t| {
            t.with_timezone(&Utc)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
        .unwrap_or_default()
}

/// UTC bounds for an IST date range; today in IST when neither is given.
#[allow(clippy::result_large_err)]
fn range(ctx: &AppState, q: &HashMap<String, String>) -> Result<(String, String), Response> {
    let parse = |k: &str| -> Result<Option<NaiveDate>, Response> {
        match q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(Some)
                .map_err(|_| error(StatusCode::BAD_REQUEST, "Choose dates as YYYY-MM-DD.")),
        }
    };
    let start = parse("start_date")?;
    let end = parse("end_date")?;
    let (from, to) = match (start, end) {
        (None, None) => {
            let today = ctx.now().with_timezone(&Kolkata).date_naive();
            (Some(today), Some(today))
        }
        other => other,
    };
    let from = from
        .map(utc_iso_of_ist_midnight)
        .unwrap_or_else(|| "0000".into());
    let to = to
        .map(|d| utc_iso_of_ist_midnight(d + Duration::days(1)))
        .unwrap_or_else(|| "9999".into());
    Ok((from, to))
}

fn sanitize(v: Value) -> Value {
    crate::db::sqlite::logs::redact(v)
}

fn entry(row: &OrderLogRow) -> Value {
    let (id, api_type, req, resp, created) = row;
    let request = serde_json::from_str::<Value>(req)
        .map(sanitize)
        .unwrap_or_else(|_| json!({}));
    let response = serde_json::from_str::<Value>(resp).unwrap_or_else(|_| json!({}));
    let strategy = request
        .get("strategy")
        .and_then(|s| s.as_str())
        .unwrap_or("Unknown")
        .to_string();
    let created_at = chrono::DateTime::parse_from_rfc3339(created)
        .map(|t| {
            t.with_timezone(&Kolkata)
                .format("%Y-%m-%d %I:%M:%S %p")
                .to_string()
        })
        .unwrap_or_else(|_| created.clone());
    json!({
        "id": id,
        "api_type": api_type,
        "request_data": request,
        "response_data": response,
        "strategy": strategy,
        "created_at": created_at,
    })
}

/// GET /logs (JSON request): `{logs, total_pages, current_page}`.
pub async fn view(State(ctx): Ctx, Query(q): Q) -> Response {
    let (from, to) = match range(&ctx, &q) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let page = q
        .get("page")
        .and_then(|p| p.parse::<i64>().ok())
        .unwrap_or(1)
        .max(1);
    let search = q
        .get("search")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let r = ctx.logs.conn().and_then(|c| {
        store::order_logs_page(
            &c,
            &from,
            &to,
            search.as_deref(),
            Some(((page - 1) * PER_PAGE, PER_PAGE)),
        )
    });
    match r {
        Ok((rows, total)) => {
            let total_pages = ((total + PER_PAGE - 1) / PER_PAGE).max(1);
            ok(json!({
                "logs": rows.iter().map(entry).collect::<Vec<_>>(),
                "total_pages": total_pages,
                "current_page": page,
            }))
        }
        Err(e) => failed(
            "Reading order logs",
            e,
            "Could not load the logs. Try again.",
        ),
    }
}

/// GET /logs/export: every matching entry as CSV (web column set).
pub async fn export(State(ctx): Ctx, Query(q): Q) -> Response {
    let (from, to) = match range(&ctx, &q) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let search = q
        .get("search")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let r = ctx
        .logs
        .conn()
        .and_then(|c| store::order_logs_page(&c, &from, &to, search.as_deref(), None));
    let rows = match r {
        Ok((rows, _)) => rows,
        Err(e) => {
            return failed(
                "Exporting order logs",
                e,
                "An error occurred while exporting logs",
            )
        }
    };
    let mut out = csv_row(
        &[
            "ID",
            "Timestamp",
            "API Type",
            "Strategy",
            "Exchange",
            "Symbol",
            "Action",
            "Product",
            "Price Type",
            "Quantity",
            "Position Size",
            "Price",
            "Trigger Price",
            "Disclosed Quantity",
            "Order ID",
            "Response",
        ]
        .map(String::from),
    );
    for row in &rows {
        let e = entry(row);
        let rq = &e["request_data"];
        let f = |k: &str| match rq.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(v) => v.to_string(),
        };
        out.push_str(&csv_row(&[
            e["id"].to_string(),
            e["created_at"].as_str().unwrap_or_default().to_string(),
            e["api_type"].as_str().unwrap_or_default().to_string(),
            e["strategy"].as_str().unwrap_or_default().to_string(),
            f("exchange"),
            f("symbol"),
            f("action"),
            f("product"),
            f("pricetype"),
            f("quantity"),
            f("position_size"),
            f("price"),
            f("trigger_price"),
            f("disclosed_quantity"),
            f("orderid"),
            e["response_data"].to_string(),
        ]));
    }
    let stamp = ctx.now().with_timezone(&Kolkata).format("%Y%m%d_%H%M%S");
    download(out, "text/csv", &format!("openalgo_logs_{}.csv", stamp))
}
