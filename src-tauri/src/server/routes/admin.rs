//! Web `blueprints/admin.py` JSON API: freeze quantities, market holidays
//! and timings, the error log and the diagnostics page.
//!
//! The web's remote MCP admin endpoints (`/admin/api/oauth/*`,
//! `/admin/api/mcp/*`) belong to the MCP wave and are not served here.

use crate::db::sqlite::market_calendar::{self as cal, OpenWindow};
use crate::db::sqlite::webui;
use crate::server::envelope::error;
use crate::server::routes::webui::{download, failed, no_store, ok, JsonBody};
use crate::services::market_calendar_service as cal_service;
use crate::services::system_info;
use crate::state::AppState;
use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::NaiveDate;
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;
type Q = Query<HashMap<String, String>>;

const FREEZE_UPLOAD_MAX: usize = 2 * 1024 * 1024;

fn freeze_json(f: &webui::FreezeQty) -> Value {
    json!({"id": f.id, "exchange": f.exchange, "symbol": f.symbol, "freeze_qty": f.freeze_qty})
}

/// GET /admin/api/stats
pub async fn stats(State(ctx): Ctx) -> Response {
    let r = ctx
        .sqlite
        .conn()
        .and_then(|c| Ok((webui::freeze_count(&c, None)?, cal::holiday_count(&c)?)));
    match r {
        Ok((f, h)) => ok(json!({"status": "success", "freeze_count": f, "holiday_count": h})),
        Err(e) => failed(
            "Admin stats",
            e,
            "Could not load the admin summary. Try again.",
        ),
    }
}

// ------------------------------------------------------------- freeze qty

/// GET /admin/api/freeze
pub async fn freeze_list(State(ctx): Ctx) -> Response {
    match ctx.sqlite.conn().and_then(|c| webui::freeze_list(&c)) {
        Ok(v) => {
            ok(json!({"status": "success", "data": v.iter().map(freeze_json).collect::<Vec<_>>()}))
        }
        Err(e) => failed(
            "Freeze list",
            e,
            "Could not load freeze quantities. Try again.",
        ),
    }
}

#[allow(clippy::result_large_err)]
fn qty_of(body: &JsonBody) -> Result<Option<i64>, Response> {
    match body.int("freeze_qty") {
        Ok(Some(q)) if q > 0 => Ok(Some(q)),
        Ok(None) => Ok(None),
        _ => Err(error(
            StatusCode::BAD_REQUEST,
            "Freeze quantity must be a whole number above zero.",
        )),
    }
}

/// POST /admin/api/freeze (json: exchange, symbol, freeze_qty)
pub async fn freeze_add(State(ctx): Ctx, body: JsonBody) -> Response {
    let exchange = body
        .non_empty("exchange")
        .unwrap_or_else(|| "NFO".into())
        .to_ascii_uppercase();
    let symbol = body.str("symbol").unwrap_or_default().to_ascii_uppercase();
    let qty = match qty_of(&body) {
        Ok(q) => q,
        Err(r) => return r,
    };
    let Some(qty) = qty.filter(|_| !symbol.is_empty()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "Symbol and freeze_qty are required",
        );
    };
    match ctx
        .sqlite
        .conn()
        .and_then(|c| webui::freeze_add(&c, &exchange, &symbol, qty))
    {
        Ok(Some(f)) => ok(json!({
            "status": "success",
            "message": format!("Added freeze qty for {}: {}", symbol, qty),
            "data": freeze_json(&f),
        })),
        Ok(None) => error(
            StatusCode::BAD_REQUEST,
            format!("{} already exists for {}", symbol, exchange),
        ),
        Err(e) => failed(
            "Adding freeze qty",
            e,
            "Could not save the freeze quantity. Try again.",
        ),
    }
}

/// PUT /admin/api/freeze/{id} (json: freeze_qty)
pub async fn freeze_edit(State(ctx): Ctx, Path(id): Path<i64>, body: JsonBody) -> Response {
    let conn = match ctx.sqlite.conn() {
        Ok(c) => c,
        Err(e) => {
            return failed(
                "Editing freeze qty",
                e,
                "Could not save the freeze quantity. Try again.",
            )
        }
    };
    let Ok(Some(entry)) = webui::freeze_get(&conn, id) else {
        return error(StatusCode::NOT_FOUND, "Entry not found");
    };
    let qty = match qty_of(&body) {
        Ok(Some(q)) => q,
        Ok(None) => return error(StatusCode::BAD_REQUEST, "No freeze_qty provided"),
        Err(r) => return r,
    };
    match webui::freeze_update(&conn, id, qty) {
        Ok(_) => ok(json!({
            "status": "success",
            "message": format!("Updated freeze qty for {}: {}", entry.symbol, qty),
            "data": freeze_json(&webui::FreezeQty { freeze_qty: qty, ..entry }),
        })),
        Err(e) => failed(
            "Editing freeze qty",
            e,
            "Could not save the freeze quantity. Try again.",
        ),
    }
}

/// DELETE /admin/api/freeze/{id}
pub async fn freeze_delete(State(ctx): Ctx, Path(id): Path<i64>) -> Response {
    let conn = match ctx.sqlite.conn() {
        Ok(c) => c,
        Err(e) => {
            return failed(
                "Deleting freeze qty",
                e,
                "Could not delete the entry. Try again.",
            )
        }
    };
    let Ok(Some(entry)) = webui::freeze_get(&conn, id) else {
        return error(StatusCode::NOT_FOUND, "Entry not found");
    };
    match webui::freeze_delete(&conn, id) {
        Ok(_) => ok(
            json!({"status": "success", "message": format!("Deleted freeze qty for {}", entry.symbol)}),
        ),
        Err(e) => failed(
            "Deleting freeze qty",
            e,
            "Could not delete the entry. Try again.",
        ),
    }
}

/// POST /admin/api/freeze/upload (multipart: csv_file, exchange)
pub async fn freeze_upload(State(ctx): Ctx, mut mp: Multipart) -> Response {
    let mut file: Option<(String, Vec<u8>)> = None;
    let mut exchange = "NFO".to_string();
    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "The upload could not be read. Try again.",
                )
            }
        };
        match field.name().unwrap_or_default() {
            "csv_file" => {
                let name = field.file_name().unwrap_or_default().to_string();
                match field.bytes().await {
                    Ok(b) if b.len() <= FREEZE_UPLOAD_MAX => file = Some((name, b.to_vec())),
                    Ok(_) => {
                        return error(
                            StatusCode::BAD_REQUEST,
                            "The file is too large. Upload the exchange's freeze quantity CSV.",
                        )
                    }
                    Err(_) => {
                        return error(
                            StatusCode::BAD_REQUEST,
                            "The upload could not be read. Try again.",
                        )
                    }
                }
            }
            "exchange" => {
                if let Ok(t) = field.text().await {
                    let t = t.trim().to_ascii_uppercase();
                    if !t.is_empty() {
                        exchange = t;
                    }
                }
            }
            _ => {}
        }
    }
    let Some((name, bytes)) = file.filter(|(n, _)| !n.is_empty()) else {
        return error(StatusCode::BAD_REQUEST, "No file selected");
    };
    if !name.to_ascii_lowercase().ends_with(".csv") {
        return error(StatusCode::BAD_REQUEST, "Please upload a CSV file");
    }
    if !cal::SUPPORTED_EXCHANGES.contains(&exchange.as_str()) {
        return error(StatusCode::BAD_REQUEST, "Choose a supported exchange.");
    }
    let text = String::from_utf8_lossy(&bytes);
    let Some(rows) = webui::parse_freeze_csv(&text) else {
        return error(
            StatusCode::BAD_REQUEST,
            "The file needs a SYMBOL column and a freeze quantity column. Download the freeze quantity file from the exchange and upload it as it is.",
        );
    };
    let r = ctx.sqlite.conn().and_then(|c| {
        webui::freeze_replace_exchange(&c, &exchange, &rows)?;
        webui::freeze_count(&c, Some(&exchange))
    });
    match r {
        Ok(count) => ok(json!({
            "status": "success",
            "message": format!("Successfully loaded {} freeze quantities for {}", count, exchange),
            "count": count,
        })),
        Err(e) => failed("Freeze upload", e, "Error loading CSV file"),
    }
}

// ---------------------------------------------------------------- holidays

/// GET /admin/api/holidays?year=
pub async fn holidays(State(ctx): Ctx, Query(q): Q) -> Response {
    let year = q.get("year").and_then(|y| y.trim().parse::<i32>().ok());
    match cal_service::admin_holidays(&ctx, year) {
        Ok(v) => ok(v),
        Err(e) => failed("Holiday list", e, "Could not load holidays. Try again."),
    }
}

/// POST /admin/api/holidays
pub async fn holiday_add(State(ctx): Ctx, body: JsonBody) -> Response {
    let date_str = body.str("date").unwrap_or_default();
    let description = body.str("description").unwrap_or_default();
    let holiday_type = body
        .non_empty("holiday_type")
        .unwrap_or_else(|| "TRADING_HOLIDAY".into());
    if date_str.is_empty() || description.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Date and description are required");
    }
    if !cal::HOLIDAY_TYPES.contains(&holiday_type.as_str()) {
        return error(
            StatusCode::BAD_REQUEST,
            "Choose a holiday type from the list.",
        );
    }
    let Ok(date) = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d") else {
        return error(
            StatusCode::BAD_REQUEST,
            "Invalid date format. Use YYYY-MM-DD",
        );
    };
    if description.chars().count() > 150 {
        return error(
            StatusCode::BAD_REQUEST,
            "Keep the description under 150 characters.",
        );
    }
    let exch_ok = |e: &str| cal::SUPPORTED_EXCHANGES.contains(&e);
    let closed: Vec<String> = match body.0.get("closed_exchanges") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim().to_ascii_uppercase())
            .filter(|s| exch_ok(s))
            .collect(),
        _ => Vec::new(),
    };
    let open: Vec<OpenWindow> = match body.0.get("open_exchanges") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|o| {
                let ex = o.get("exchange")?.as_str()?.trim().to_ascii_uppercase();
                let s = o.get("start_time")?.as_i64()?;
                let e = o.get("end_time")?.as_i64()?;
                (exch_ok(&ex) && e > s).then_some(OpenWindow {
                    exchange: ex,
                    start_time: s,
                    end_time: e,
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    if holiday_type == "SPECIAL_SESSION" && open.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "Special session requires at least one exchange with timings",
        );
    }
    let new = cal::NewHoliday {
        date,
        description: &description,
        holiday_type: &holiday_type,
        closed: &closed,
        open: &open,
    };
    match ctx.sqlite.conn().and_then(|c| cal::add_holiday(&c, &new)) {
        Ok(Some(id)) => ok(json!({
            "status": "success",
            "message": format!("Added holiday: {} on {}", description, date_str),
            "data": {
                "id": id,
                "date": date_str,
                "description": description,
                "holiday_type": holiday_type,
                "closed_exchanges": closed,
                "open_exchanges": open.iter().map(|w| json!({"exchange": w.exchange, "start_time": w.start_time, "end_time": w.end_time})).collect::<Vec<_>>(),
            },
        })),
        Ok(None) => error(
            StatusCode::BAD_REQUEST,
            format!(
                "A holiday is already listed on {}. Delete it first to replace it.",
                date_str
            ),
        ),
        Err(e) => failed(
            "Adding holiday",
            e,
            "Could not save the holiday. Try again.",
        ),
    }
}

/// DELETE /admin/api/holidays/{id}
pub async fn holiday_delete(State(ctx): Ctx, Path(id): Path<i64>) -> Response {
    match ctx.sqlite.conn().and_then(|c| cal::delete_holiday(&c, id)) {
        Ok(Some(d)) => {
            ok(json!({"status": "success", "message": format!("Deleted holiday: {}", d)}))
        }
        Ok(None) => error(StatusCode::NOT_FOUND, "Holiday not found"),
        Err(e) => failed(
            "Deleting holiday",
            e,
            "Could not delete the holiday. Try again.",
        ),
    }
}

// ----------------------------------------------------------------- timings

/// GET /admin/api/timings
pub async fn timings(State(ctx): Ctx) -> Response {
    match cal_service::admin_timings(&ctx) {
        Ok(v) => ok(v),
        Err(e) => failed(
            "Timing list",
            e,
            "Could not load market timings. Try again.",
        ),
    }
}

/// PUT /admin/api/timings/{exchange} (json: start_time, end_time)
pub async fn timing_edit(
    State(ctx): Ctx,
    Path(exchange): Path<String>,
    body: JsonBody,
) -> Response {
    let start = body.str("start_time").unwrap_or_default();
    let end = body.str("end_time").unwrap_or_default();
    if start.is_empty() || end.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "Start time and end time are required",
        );
    }
    let (Some(so), Some(eo)) = (cal::parse_hhmm(&start), cal::parse_hhmm(&end)) else {
        return error(StatusCode::BAD_REQUEST, "Invalid time format. Use HH:MM");
    };
    if eo <= so {
        return error(
            StatusCode::BAD_REQUEST,
            "The close time must be after the open time.",
        );
    }
    let ex = exchange.to_ascii_uppercase();
    if !cal::SUPPORTED_EXCHANGES.contains(&ex.as_str()) {
        return error(StatusCode::BAD_REQUEST, "Choose a supported exchange.");
    }
    match ctx
        .sqlite
        .conn()
        .and_then(|c| cal::update_timing(&c, &ex, &start, &end))
    {
        Ok(()) => ok(json!({
            "status": "success",
            "message": format!("Updated timing for {}: {} - {}", exchange, start, end),
        })),
        Err(e) => failed(
            "Editing timing",
            e,
            &format!("Error updating timing for {}", exchange),
        ),
    }
}

/// POST /admin/api/timings/check (json: date)
pub async fn timing_check(State(ctx): Ctx, body: JsonBody) -> Response {
    let date_str = body.str("date").unwrap_or_default();
    if date_str.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Date is required");
    }
    let Ok(date) = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d") else {
        return error(
            StatusCode::BAD_REQUEST,
            "Invalid date format. Use YYYY-MM-DD",
        );
    };
    match cal_service::timings_for(&ctx, date) {
        Ok(w) => ok(json!({
            "status": "success",
            "date": date_str,
            "timings": cal_service::windows_hhmm(&w),
        })),
        Err(e) => failed(
            "Checking timings",
            e,
            "Could not check market timings. Try again.",
        ),
    }
}

// --------------------------------------------------------------- errors

/// GET /admin/api/errors?limit=&level=&q=
pub async fn errors(State(ctx): Ctx, Query(q): Q) -> Response {
    let limit = q
        .get("limit")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, system_info::MAX_LIMIT);
    let level = q
        .get("level")
        .map(|l| l.trim().to_ascii_uppercase())
        .filter(|l| !l.is_empty());
    if let Some(l) = &level {
        if !system_info::LEVELS.contains(&l.as_str()) {
            return error(StatusCode::BAD_REQUEST, "Invalid level");
        }
    }
    let search: Option<String> = q
        .get("q")
        .map(|s| s.trim().chars().take(200).collect())
        .filter(|s: &String| !s.is_empty());
    match system_info::errors_list(&ctx, limit, level.as_deref(), search.as_deref()) {
        Ok(v) => no_store(ok(v)),
        Err(e) => failed("Reading error log", e, "Failed to read error log"),
    }
}

/// POST /admin/api/errors/client: a browser error report.
pub async fn errors_client(State(ctx): Ctx, body: JsonBody) -> Response {
    let now = ctx.now();
    match crate::services::error_log::record_client_report(&ctx, &body.0, now) {
        Err(msg) => error(StatusCode::BAD_REQUEST, msg),
        Ok(Ok(())) => ok(json!({"status": "success"})),
        Ok(Err(e)) => failed("Recording a browser error", e, "Failed to record"),
    }
}

/// GET /admin/api/errors/stats
pub async fn errors_stats(State(ctx): Ctx) -> Response {
    match system_info::errors_stats(&ctx) {
        Ok(v) => no_store(ok(v)),
        Err(e) => failed("Reading error stats", e, "Failed to read error log"),
    }
}

/// GET /admin/api/errors/groups?limit=
pub async fn errors_groups(State(ctx): Ctx, Query(q): Q) -> Response {
    let limit = q
        .get("limit")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(50)
        .clamp(1, system_info::MAX_LIMIT);
    match system_info::errors_groups(&ctx, limit) {
        Ok(v) => no_store(ok(v)),
        Err(e) => failed("Grouping errors", e, "Failed to group errors"),
    }
}

// ----------------------------------------------------------------- system

fn payload(ctx: &AppState) -> Value {
    let broker = ctx.get_broker_session().map(|b| b.broker_id);
    system_info::system_payload(ctx, broker, true)
}

/// GET /admin/api/system
pub async fn system(State(ctx): Ctx) -> Response {
    let c = ctx.clone();
    match tokio::task::spawn_blocking(move || payload(&c)).await {
        Ok(v) => no_store(ok(json!({"status": "success", "data": v}))),
        Err(e) => failed("System info", e, "Failed to build system info"),
    }
}

/// POST /admin/api/system/diagnostics
pub async fn diagnostics(State(ctx): Ctx) -> Response {
    let c = ctx.clone();
    match tokio::task::spawn_blocking(move || system_info::diagnostics(&c)).await {
        Ok(v) => no_store(ok(v)),
        Err(e) => failed("Diagnostics", e, "Failed to run diagnostics"),
    }
}

/// GET /admin/api/system/report?format=md|txt
pub async fn report(State(ctx): Ctx, Query(q): Q) -> Response {
    let md = !matches!(
        q.get("format")
            .map(|f| f.trim().to_ascii_lowercase())
            .as_deref(),
        Some("txt")
    );
    let c = ctx.clone();
    let built = tokio::task::spawn_blocking(move || {
        let p = payload(&c);
        system_info::render_report(&c, &p, md)
    })
    .await;
    match built {
        Ok(Ok(body)) => {
            let stamp = ctx.now().with_timezone(&Kolkata).format("%Y%m%d-%H%M%S");
            let ext = if md { "md" } else { "txt" };
            let mime = if md {
                "text/markdown; charset=utf-8"
            } else {
                "text/plain; charset=utf-8"
            };
            let mut r = download(body, mime, "report");
            if let Ok(v) = HeaderValue::from_str(&format!(
                "attachment; filename=\"openalgo-system-report-{}.{}\"",
                stamp, ext
            )) {
                r.headers_mut().insert(header::CONTENT_DISPOSITION, v);
            }
            r.into_response()
        }
        Ok(Err(e)) => failed("System report", e, "Failed to generate report"),
        Err(e) => failed("System report", e, "Failed to generate report"),
    }
}
