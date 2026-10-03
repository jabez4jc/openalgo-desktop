//! Web `blueprints/latency.py`: `/api/v1` call latency recorded by the
//! monitoring middleware (`services::monitor`), with the web's statistics
//! (numpy-style linear percentiles over the last 30 days, 30-bin histograms).

use crate::db::sqlite::monitor::{self as store, LatencyRow};
use crate::server::envelope::json_response;
use crate::server::routes::webui::{csv_row, download, failed, ok};
use crate::services::security_service::web_time;
use crate::state::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use chrono::Duration;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

type Ctx = State<Arc<AppState>>;

/// numpy `percentile` (linear interpolation) on sorted data.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = p / 100.0 * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (rank - lo as f64)
}

/// numpy `histogram(values, bins=30, range=(min, max))`.
pub fn histogram(values: &[f64]) -> Value {
    if values.is_empty() {
        return json!({"bins": [], "counts": [], "avg_rtt": 0, "min_rtt": 0, "max_rtt": 0});
    }
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let avg = values.iter().sum::<f64>() / values.len() as f64;
    let (lo, hi) = if max > min {
        (min, max)
    } else {
        (min - 0.5, max + 0.5)
    };
    let n = 30usize;
    let width = (hi - lo) / n as f64;
    let mut counts = vec![0i64; n];
    for v in values {
        let mut i = ((v - lo) / width).floor() as isize;
        if i >= n as isize {
            i = n as isize - 1;
        }
        if i >= 0 {
            counts[i as usize] += 1;
        }
    }
    let bins: Vec<String> = (0..n)
        .map(|i| format!("{:.1}", lo + width * i as f64))
        .collect();
    json!({"bins": bins, "counts": counts, "avg_rtt": avg, "min_rtt": min, "max_rtt": max})
}

fn row_json(l: &LatencyRow) -> Value {
    json!({
        "timestamp": crate::services::health_service::iso_ist(&l.timestamp),
        "id": l.id,
        "order_id": l.order_id,
        "broker": l.broker,
        "symbol": l.symbol,
        "order_type": l.order_type,
        "rtt_ms": l.rtt_ms,
        "validation_latency_ms": l.validation_latency_ms,
        "response_latency_ms": l.response_latency_ms,
        "overhead_ms": l.overhead_ms,
        "total_latency_ms": l.total_latency_ms,
        "status": l.status,
        "error": l.error,
    })
}

/// Web `OrderLatency.get_latency_stats` plus `broker_histograms`.
pub fn stats_value(ctx: &AppState) -> crate::error::Result<Value> {
    ctx.monitor.drain_now(ctx);
    let points = {
        let c = ctx.logs.conn()?;
        store::latency_points(&c)?
    };
    let cutoff = store::ts(ctx.now() - Duration::days(30));
    let total = points.len();
    let failed_n = points.iter().filter(|p| p.3).count();
    let avg = |f: &dyn Fn(&store::LatencyPoint) -> f64, ps: &[&store::LatencyPoint]| {
        if ps.is_empty() {
            0.0
        } else {
            ps.iter().map(|p| f(p)).sum::<f64>() / ps.len() as f64
        }
    };
    let all: Vec<&_> = points.iter().collect();
    let under = |ms: f64| points.iter().filter(|p| p.2 < ms).count() as f64;
    let pct = |n: f64| {
        if total > 0 {
            n / total as f64 * 100.0
        } else {
            0.0
        }
    };
    let mut recent: Vec<f64> = points
        .iter()
        .filter(|p| p.4 >= cutoff)
        .map(|p| p.2)
        .collect();
    recent.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut by_broker: BTreeMap<String, Vec<&_>> = BTreeMap::new();
    for p in &points {
        if let Some(b) = &p.0 {
            by_broker.entry(b.clone()).or_default().push(p);
        }
    }
    let mut broker_stats = Map::new();
    let mut histograms = Map::new();
    for (b, ps) in &by_broker {
        let n = ps.len();
        let mut lat: Vec<f64> = ps.iter().filter(|p| p.4 >= cutoff).map(|p| p.2).collect();
        lat.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let u150 = ps.iter().filter(|p| p.2 < 150.0).count() as f64;
        broker_stats.insert(
            b.clone(),
            json!({
                "total_orders": n,
                "failed_orders": ps.iter().filter(|p| p.3).count(),
                "avg_rtt": avg(&|p| p.1, ps),
                "avg_overhead": 0.0,
                "avg_total": avg(&|p| p.2, ps),
                "p50_total": percentile(&lat, 50.0),
                "p99_total": percentile(&lat, 99.0),
                "sla_150ms": if n > 0 { u150 / n as f64 * 100.0 } else { 0.0 },
            }),
        );
        let rtts: Vec<f64> = ps.iter().map(|p| p.1).collect();
        histograms.insert(b.clone(), histogram(&rtts));
    }
    Ok(json!({
        "total_orders": total,
        "failed_orders": failed_n,
        "success_rate": if total > 0 { (total - failed_n) as f64 / total as f64 * 100.0 } else { 0.0 },
        "avg_rtt": avg(&|p| p.1, &all),
        "avg_overhead": 0.0,
        "avg_total": avg(&|p| p.2, &all),
        "p50_total": percentile(&recent, 50.0),
        "p90_total": percentile(&recent, 90.0),
        "p95_total": percentile(&recent, 95.0),
        "p99_total": percentile(&recent, 99.0),
        "sla_100ms": pct(under(100.0)),
        "sla_150ms": pct(under(150.0)),
        "sla_200ms": pct(under(200.0)),
        "broker_stats": broker_stats,
        "broker_histograms": histograms,
    }))
}

/// GET /latency/api/logs?limit=
pub async fn logs(State(ctx): Ctx, Query(q): Query<HashMap<String, String>>) -> Response {
    ctx.monitor.drain_now(&ctx);
    let limit = q
        .get("limit")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    match ctx
        .logs
        .conn()
        .and_then(|c| store::recent_latency(&c, Some(limit)))
    {
        Ok(rows) => ok(Value::Array(rows.iter().map(row_json).collect())),
        Err(e) => failed(
            "Reading latency logs",
            e,
            "Could not load the latency log. Try again.",
        ),
    }
}

/// GET /latency/api/stats
pub async fn stats(State(ctx): Ctx) -> Response {
    match stats_value(&ctx) {
        Ok(v) => ok(v),
        Err(e) => failed(
            "Latency stats",
            e,
            "Could not load latency statistics. Try again.",
        ),
    }
}

/// GET /latency/api/broker/{broker}/stats
pub async fn broker_stats(State(ctx): Ctx, Path(broker): Path<String>) -> Response {
    let v = match stats_value(&ctx) {
        Ok(v) => v,
        Err(e) => {
            return failed(
                "Latency stats",
                e,
                "Could not load latency statistics. Try again.",
            )
        }
    };
    match v["broker_stats"].get(&broker) {
        Some(Value::Object(m)) => {
            let mut m = m.clone();
            m.insert("histogram".into(), v["broker_histograms"][&broker].clone());
            ok(Value::Object(m))
        }
        _ => json_response(StatusCode::NOT_FOUND, json!({"error": "Broker not found"})),
    }
}

/// GET /latency/export
pub async fn export(State(ctx): Ctx) -> Response {
    ctx.monitor.drain_now(&ctx);
    let rows = match ctx
        .logs
        .conn()
        .and_then(|c| store::recent_latency(&c, None))
    {
        Ok(r) => r,
        Err(e) => {
            return failed(
                "Exporting latency logs",
                e,
                "Could not export the latency log. Try again.",
            )
        }
    };
    let mut out = csv_row(
        &[
            "Date & Time (IST)",
            "Broker",
            "Order ID",
            "Symbol",
            "Order Type",
            "Broker Confirmation (ms)",
            "Platform Overhead (ms)",
            "Total Latency (ms)",
            "Status",
            "Error (if any)",
        ]
        .map(String::from),
    );
    let r2 = |v: f64| ((v * 100.0).round() / 100.0).to_string();
    for l in &rows {
        out.push_str(&csv_row(&[
            web_time(&l.timestamp),
            l.broker.clone().unwrap_or_else(|| "N/A".into()),
            l.order_id.clone(),
            l.symbol.clone().unwrap_or_else(|| "N/A".into()),
            l.order_type.clone(),
            r2(l.rtt_ms),
            r2(l.overhead_ms),
            r2(l.total_latency_ms),
            l.status.clone(),
            l.error.clone().unwrap_or_default(),
        ]));
    }
    download(out, "text/csv", "latency_logs.csv")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numpy_percentiles_and_histogram() {
        let v = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&v, 50.0), 2.5);
        assert_eq!(percentile(&v, 100.0), 4.0);
        let h = histogram(&[10.0, 20.0, 40.0]);
        assert_eq!(h["counts"].as_array().unwrap().len(), 30);
        assert_eq!(
            h["counts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_i64().unwrap())
                .sum::<i64>(),
            3
        );
        assert_eq!(h["bins"][0], "10.0");
    }
}
