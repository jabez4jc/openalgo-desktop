//! History-driven tools (web `iv_chart_service.py`,
//! `straddle_chart_service.py`, `custom_straddle_service.py`,
//! `vol_surface_service.py`, `strategy_chart_service.py`,
//! `multi_strike_oi_service.py`). Broker history goes through the shared
//! bounded fan-out; the arithmetic lives in [`crate::analytics`].

use super::core::{broker_handle, float, num, BrokerHandle, Reply};
use super::market_data_service;
use super::options_service::{
    available_strikes, calculate_greeks, expiry_datetime, parse_option_symbol, today_ist,
};
use super::tools_service::{
    history_bars, many_histories, quote, resolve_reference, strategy_reference, Reference,
};
use crate::analytics::black76::Flag;
use crate::analytics::series::{
    cap_last_n_dates, closes, combined_premium, credit_tag, explicit_window, iv_series,
    trading_window, Bar,
};
use crate::analytics::straddle::{self, StrikeLegs, StrikeMap};
use crate::analytics::{closest_strike, py_round, surface};
use crate::brokers::common::master_contract::format_strike;
use crate::state::AppState;
use chrono::{NaiveDate, NaiveTime, TimeZone};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

fn option_symbol(base: &str, expiry: &str, strike: f64, ot: &str) -> String {
    format!(
        "{}{}{}{}",
        base,
        expiry.to_ascii_uppercase(),
        format_strike(strike),
        ot
    )
}

fn strikes_for(ctx: &AppState, r: &Reference, expiry: &str) -> Vec<f64> {
    let snap = ctx.symbols.snapshot();
    available_strikes(
        snap.rows(),
        &r.base,
        &expiry.to_ascii_uppercase(),
        "CE",
        &r.options_exchange,
    )
}

fn data(v: Value) -> Reply {
    Reply::ok(json!({"status": "success", "data": v}))
}

/// The latest underlying LTP for an info bar: 0 when the quote fails.
async fn info_ltp(ctx: &AppState, h: &BrokerHandle, symbol: &str, exchange: &str) -> Value {
    match quote(ctx, h, symbol, exchange).await {
        Ok(q) => num(q.ltp),
        Err(_) => json!(0),
    }
}

/// The expiry at 15:30 IST (web `_calculate_days_to_expiry`).
fn days_to_expiry(ctx: &AppState, expiry: &str) -> i64 {
    let Some(d) = super::options_service::parse_compact_expiry(expiry) else {
        return 0;
    };
    let t = NaiveTime::from_hms_opt(15, 30, 0).unwrap_or_default();
    match Kolkata.from_local_datetime(&d.and_time(t)).single() {
        Some(e) => straddle::days_to_expiry(e.timestamp(), ctx.now().timestamp()),
        None => 0,
    }
}

/// The ATM call and put for an underlying's live LTP.
struct Atm {
    ltp_json: Value,
    strike: f64,
    ce: String,
    pe: String,
}

async fn resolve_atm(
    ctx: &AppState,
    h: &BrokerHandle,
    r: &Reference,
    expiry: &str,
    quote_fail: impl Fn(&Reply) -> String,
    no_strikes: String,
) -> Result<Atm, Reply> {
    let q = quote(ctx, h, &r.quote_symbol, &r.quote_exchange)
        .await
        .map_err(|e| Reply::error(e.status, quote_fail(&e)))?;
    if q.ltp == 0.0 || !q.ltp.is_finite() {
        return Err(Reply::error(400, "Could not get underlying LTP"));
    }
    let strikes = strikes_for(ctx, r, expiry);
    if strikes.is_empty() {
        return Err(Reply::error(404, no_strikes));
    }
    let Some(k) = closest_strike(&strikes, q.ltp) else {
        return Err(Reply::error(400, "Could not determine ATM strike"));
    };
    Ok(Atm {
        ltp_json: num(q.ltp),
        strike: k,
        ce: option_symbol(&r.base, expiry, k, "CE"),
        pe: option_symbol(&r.base, expiry, k, "PE"),
    })
}

/// `POST /ivchart/api/default-symbols`.
pub async fn default_symbols(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let r = match resolve_reference(ctx, underlying, exchange) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let no_strikes = format!("No strikes found for {} {}", r.base, expiry);
    match resolve_atm(
        ctx,
        &h,
        &r,
        expiry,
        |_| "Failed to fetch underlying quote".into(),
        no_strikes,
    )
    .await
    {
        Ok(a) => data(json!({
            "ce_symbol": a.ce,
            "pe_symbol": a.pe,
            "atm_strike": float(a.strike),
            "exchange": r.options_exchange,
            "underlying_ltp": a.ltp_json,
        })),
        Err(e) => e,
    }
}

/// `POST /ivchart/api/iv-data`.
pub async fn iv_chart(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
    interval: &str,
    days: i64,
) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let window = trading_window(today_ist(ctx), days);
    let r = match resolve_reference(ctx, underlying, exchange) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let no_strikes = format!(
        "No strikes found for {} {} on {}",
        r.base, expiry, r.options_exchange
    );
    let a = match resolve_atm(
        ctx,
        &h,
        &r,
        expiry,
        |e| format!("Failed to fetch underlying quote: {}", e.message()),
        no_strikes,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Ok(c) = parse_option_symbol(&a.ce) else {
        return Reply::error(400, "Could not determine ATM strike");
    };
    let Some(expiry_ts) =
        expiry_datetime(&c.expiry, &r.options_exchange, None).map(|e| e.timestamp())
    else {
        return Reply::error(400, "Could not determine ATM strike");
    };
    let mut got = many_histories(
        ctx,
        vec![
            (r.quote_symbol.clone(), r.quote_exchange.clone()),
            (a.ce.clone(), r.options_exchange.clone()),
            (a.pe.clone(), r.options_exchange.clone()),
        ],
        interval,
        window,
    )
    .await
    .into_iter();
    let (u, ce, pe) = (got.next(), got.next(), got.next());
    let u = match u {
        Some(Ok(u)) => u,
        Some(Err(e)) => {
            return Reply::error(
                400,
                format!("Failed to fetch underlying history: {}", e.message()),
            )
        }
        None => return Reply::error(500, "History fetch failed"),
    };
    if u.is_empty() {
        return Reply::error(404, "No underlying history data available for today");
    }
    let und = closes(&u);
    let mut out = Vec::new();
    for (sym, ot, flag, bars) in [(&a.ce, "CE", Flag::Call, ce), (&a.pe, "PE", Flag::Put, pe)] {
        let Some(Ok(bars)) = bars else { continue };
        if bars.is_empty() {
            continue;
        }
        let pts = iv_series(&closes(&bars), &und, c.strike, expiry_ts, flag, 0.0);
        let pts = cap_last_n_dates(pts, days, |p| p.time);
        out.push(
            json!({"symbol": sym, "option_type": ot, "strike": float(c.strike), "iv_data": pts}),
        );
    }
    if out.is_empty() {
        return Reply::error(404, "No option history data available for today");
    }
    data(json!({
        "underlying": r.base,
        "underlying_ltp": a.ltp_json,
        "atm_strike": float(a.strike),
        "ce_symbol": a.ce,
        "pe_symbol": a.pe,
        "interval": interval,
        "series": out,
    }))
}

/// Underlying bars, their ATM strikes and every ATM strike's option history:
/// the shared first half of the straddle chart and the simulation.
struct StraddleInputs {
    r: Reference,
    underlying: Vec<Bar>,
    atms: Vec<Option<f64>>,
    legs: StrikeMap,
}

async fn straddle_inputs(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
    interval: &str,
    days: i64,
    messages: (&str, &str, &str),
) -> Result<StraddleInputs, Reply> {
    let window = trading_window(today_ist(ctx), days);
    let r = resolve_reference(ctx, underlying, exchange)?;
    let strikes = strikes_for(ctx, &r, expiry);
    if strikes.is_empty() {
        return Err(Reply::error(
            404,
            format!(
                "No strikes found for {} {} on {}",
                r.base, expiry, r.options_exchange
            ),
        ));
    }
    let u = history_bars(ctx, &r.quote_symbol, &r.quote_exchange, interval, window)
        .await
        .map_err(|e| {
            let m = e.message();
            Reply::error(
                400,
                format!(
                    "Failed to fetch underlying history: {}",
                    if m.is_empty() {
                        messages.0.to_string()
                    } else {
                        m
                    }
                ),
            )
        })?;
    if u.is_empty() {
        return Err(Reply::error(404, messages.1));
    }
    let atms = straddle::atm_per_bar(&u, &strikes);
    let unique = straddle::unique_strikes(&atms);
    if unique.is_empty() {
        return Err(Reply::error(400, messages.2));
    }
    let keys: Vec<(String, String)> = unique
        .iter()
        .flat_map(|k| {
            ["CE", "PE"].map(|t| {
                (
                    option_symbol(&r.base, expiry, *k, t),
                    r.options_exchange.clone(),
                )
            })
        })
        .collect();
    let fetched = many_histories(ctx, keys, interval, window).await;
    let mut legs = StrikeMap::default();
    for (i, k) in unique.iter().enumerate() {
        let get = |j: usize| match fetched.get(j) {
            Some(Ok(b)) => closes(b),
            _ => BTreeMap::new(),
        };
        legs.insert(
            *k,
            StrikeLegs {
                ce: get(2 * i),
                pe: get(2 * i + 1),
            },
        );
    }
    Ok(StraddleInputs {
        r,
        underlying: u,
        atms,
        legs,
    })
}

/// `POST /straddle/api/straddle-data`.
pub async fn straddle_chart(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
    interval: &str,
    days: i64,
) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let s = match straddle_inputs(
        ctx,
        underlying,
        exchange,
        expiry,
        interval,
        days,
        (
            "Unknown error",
            "No underlying history data available",
            "Could not determine any ATM strikes",
        ),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => return e,
    };
    let series = straddle::straddle_series(&s.underlying, &s.atms, &s.legs);
    if series.is_empty() {
        return Reply::error(
            404,
            "No straddle data available (option history may be missing)",
        );
    }
    let series = cap_last_n_dates(series, days, |p| p.time);
    data(json!({
        "underlying": s.r.base,
        "underlying_ltp": info_ltp(ctx, &h, &s.r.quote_symbol, &s.r.quote_exchange).await,
        "expiry_date": expiry.to_ascii_uppercase(),
        "interval": interval,
        "days_to_expiry": days_to_expiry(ctx, expiry),
        "series": series,
    }))
}

/// Simulation sizing.
#[derive(Debug, Clone, Copy)]
pub struct SimParams {
    pub days: i64,
    pub adjustment_points: i64,
    pub lot_size: i64,
    pub lots: i64,
}

/// `POST /straddlepnl/api/simulate`.
pub async fn custom_straddle(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiry: &str,
    interval: &str,
    p: SimParams,
) -> Reply {
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let s = match straddle_inputs(
        ctx,
        underlying,
        exchange,
        expiry,
        interval,
        p.days,
        (
            "Unknown",
            "No underlying history data",
            "Could not determine ATM strikes",
        ),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => return e,
    };
    let quantity = p.lot_size.saturating_mul(p.lots);
    let sim = straddle::simulate(
        &s.underlying,
        &s.atms,
        &s.legs,
        p.days,
        p.adjustment_points as f64,
        quantity as f64,
    );
    if sim.pnl_series.is_empty() {
        return Reply::error(404, "No simulation data (option history may be missing)");
    }
    data(json!({
        "underlying": s.r.base,
        "underlying_ltp": info_ltp(ctx, &h, &s.r.quote_symbol, &s.r.quote_exchange).await,
        "expiry_date": expiry.to_ascii_uppercase(),
        "interval": interval,
        "days_to_expiry": days_to_expiry(ctx, expiry),
        "adjustment_points": p.adjustment_points,
        "lot_size": p.lot_size,
        "lots": p.lots,
        "quantity": quantity,
        "pnl_series": sim.pnl_series,
        "trades": sim.trades,
        "summary": sim.summary,
    }))
}

/// `POST /volsurface/api/surface-data`.
pub async fn vol_surface(
    ctx: &AppState,
    underlying: &str,
    exchange: &str,
    expiries: &[String],
    strike_count: usize,
) -> Reply {
    if expiries.is_empty() {
        return Reply::error(400, "At least one expiry is required");
    }
    let h = match broker_handle(ctx) {
        Ok(h) => h,
        Err(r) => return r,
    };
    let r = match resolve_reference(ctx, underlying, exchange) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let q = match quote(ctx, &h, &r.quote_symbol, &r.quote_exchange).await {
        Ok(q) => q,
        Err(e) => {
            return Reply::error(
                e.status,
                format!("Failed to fetch LTP for {}: {}", r.base, e.message()),
            )
        }
    };
    if q.ltp == 0.0 || !q.ltp.is_finite() {
        return Reply::error(500, format!("No LTP for {}", r.base));
    }
    let mut per_expiry: Vec<(String, f64, Vec<f64>)> = Vec::new();
    for e in expiries {
        let strikes = strikes_for(ctx, &r, e);
        if strikes.is_empty() {
            tracing::warn!("No strikes found for {} expiry {}, skipping", r.base, e);
            continue;
        }
        if let Some((atm, w)) = surface::strike_window(&strikes, q.ltp, strike_count) {
            per_expiry.push((e.clone(), atm, w));
        }
    }
    if per_expiry.is_empty() {
        return Reply::error(404, "No valid expiry data found");
    }
    let windows: Vec<Vec<f64>> = per_expiry.iter().map(|(_, _, w)| w.clone()).collect();
    let grid = surface::common_grid(&windows);
    let atm = per_expiry[0].1;
    let now = ctx.now();
    let mut grid_rows = Vec::new();
    let mut info = Vec::new();
    for (e, _, _) in &per_expiry {
        let syms: Vec<(String, String)> = grid
            .iter()
            .map(|k| {
                (
                    option_symbol(&r.base, e, *k, surface::otm_side(*k, atm)),
                    r.options_exchange.clone(),
                )
            })
            .collect();
        // One batched quote call per expiry.
        let mq = market_data_service::multiquotes(ctx, &syms).await;
        let ltps: HashMap<String, f64> = mq
            .body
            .get("results")
            .and_then(Value::as_array)
            .map(|rs| {
                rs.iter()
                    .filter_map(|x| {
                        let s = x.get("symbol")?.as_str()?.to_string();
                        let d = x.get("data").unwrap_or(x);
                        Some((s, d.get("ltp").and_then(Value::as_f64).unwrap_or(0.0)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let row: Vec<Value> = syms
            .iter()
            .map(|(sym, _)| {
                let px = ltps.get(sym).copied().unwrap_or(0.0);
                if px <= 0.0 {
                    return Value::Null;
                }
                let Ok(c) = parse_option_symbol(sym) else {
                    return Value::Null;
                };
                let g = calculate_greeks(sym, &r.options_exchange, &c, q.ltp, px, None, None, now);
                g.body
                    .get("implied_volatility")
                    .and_then(Value::as_f64)
                    .filter(|v| g.is_success() && *v > 0.0)
                    .map(|v| float(py_round(v, 2)))
                    .unwrap_or(Value::Null)
            })
            .collect();
        grid_rows.push(row);
        let dte = grid
            .first()
            .and_then(|k| parse_option_symbol(&option_symbol(&r.base, e, *k, "CE")).ok())
            .and_then(|c| expiry_datetime(&c.expiry, &r.options_exchange, None))
            .map(|x| {
                let d = ((x - now).num_milliseconds() as f64 / 86_400_000.0).max(0.0);
                float(py_round(d, 1))
            })
            .unwrap_or(json!(0));
        info.push(json!({"date": e, "dte": dte}));
    }
    data(json!({
        "underlying": r.base,
        "underlying_ltp": num(q.ltp),
        "atm_strike": float(atm),
        "strikes": grid,
        "expiries": info,
        "surface": grid_rows,
    }))
}

// ------------------------------------------------------- strategy builder

/// A Strategy Builder leg that contributes (web `_normalize_leg`): active,
/// an option, with symbol, side and exchange.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartLeg {
    pub symbol: String,
    pub exchange: String,
    pub side: String,
    pub sign: f64,
    pub entry_price: f64,
}

pub fn normalize_leg(leg: &Value) -> Option<ChartLeg> {
    let m = leg.as_object()?;
    if m.get("active") == Some(&Value::Bool(false)) {
        return None;
    }
    let text = |k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let segment = m
        .get("segment")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    if !segment.unwrap_or("OPTION").eq_ignore_ascii_case("OPTION") {
        return None;
    }
    let symbol = text("symbol");
    let side = text("side").to_ascii_uppercase();
    let exchange = text("exchange").to_ascii_uppercase();
    if symbol.is_empty() || !(side == "BUY" || side == "SELL") || exchange.is_empty() {
        return None;
    }
    let entry_price = match m.get("price") {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        Some(Value::Bool(true)) => 1.0,
        _ => 0.0,
    };
    Some(ChartLeg {
        sign: if side == "SELL" { 1.0 } else { -1.0 },
        symbol,
        exchange,
        side,
        entry_price,
    })
}

/// A Strategy Builder chart request.
#[derive(Debug, Clone)]
pub struct ChartRequest {
    pub underlying: String,
    pub exchange: String,
    pub underlying_symbol: Option<String>,
    pub underlying_exchange: Option<String>,
    pub interval: String,
    pub days: i64,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub legs: Vec<Value>,
}

struct ChartPrologue {
    base: String,
    legs: Vec<ChartLeg>,
    window: (NaiveDate, NaiveDate),
    explicit: bool,
    reference: (String, String),
    h: BrokerHandle,
}

fn chart_prologue(ctx: &AppState, req: &ChartRequest) -> Result<ChartPrologue, Reply> {
    let h = broker_handle(ctx)?;
    let today = today_ist(ctx);
    let explicit = explicit_window(req.start_date.as_deref(), req.end_date.as_deref(), today)
        .map_err(|m| Reply::error(400, m))?;
    let window = explicit.unwrap_or_else(|| trading_window(today, req.days));
    let base = req.underlying.trim().to_ascii_uppercase();
    if base.is_empty() {
        return Err(Reply::error(400, "underlying is required"));
    }
    let legs: Vec<ChartLeg> = req.legs.iter().filter_map(normalize_leg).collect();
    if legs.is_empty() {
        return Err(Reply::error(400, "No active option legs provided"));
    }
    let Some(reference) = strategy_reference(
        ctx,
        &base,
        &req.exchange,
        req.underlying_symbol.as_deref(),
        req.underlying_exchange.as_deref(),
    ) else {
        return Err(Reply::error(
            404,
            format!(
                "No unexpired futures found for {} on {}",
                base,
                req.exchange.to_ascii_uppercase()
            ),
        ));
    };
    Ok(ChartPrologue {
        base,
        legs,
        window,
        explicit: explicit.is_some(),
        reference,
        h,
    })
}

/// Underlying bars (`None` when the broker gave none) and each unique leg's
/// bars, keyed by (symbol, exchange).
async fn chart_histories(
    ctx: &AppState,
    p: &ChartPrologue,
    interval: &str,
) -> (Option<Vec<Bar>>, HashMap<(String, String), Vec<Bar>>) {
    let mut keys = vec![p.reference.clone()];
    for l in &p.legs {
        let k = (l.symbol.clone(), l.exchange.clone());
        if !keys[1..].contains(&k) {
            keys.push(k);
        }
    }
    let mut got = many_histories(ctx, keys.clone(), interval, p.window)
        .await
        .into_iter();
    let underlying = match got.next() {
        Some(Ok(b)) if !b.is_empty() => Some(b),
        Some(Err(e)) => {
            tracing::info!(
                "Strategy chart: underlying history unavailable for {}:{} @ {} ({}); continuing with legs only",
                p.reference.1,
                p.reference.0,
                interval,
                e.message()
            );
            None
        }
        _ => None,
    };
    let legs = keys[1..]
        .iter()
        .cloned()
        .zip(got)
        .map(|(k, r)| (k, r.unwrap_or_default()))
        .collect();
    (underlying, legs)
}

/// `POST /strategybuilder/api/strategy-chart`.
pub async fn strategy_chart(ctx: &AppState, req: &ChartRequest) -> Reply {
    let p = match chart_prologue(ctx, req) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let (underlying, leg_bars) = chart_histories(ctx, &p, &req.interval).await;
    let prices: HashMap<(String, String), BTreeMap<i64, f64>> = leg_bars
        .iter()
        .map(|(k, b)| (k.clone(), closes(b)))
        .collect();
    let empty = BTreeMap::new();
    let legs: Vec<(f64, &BTreeMap<i64, f64>)> = p
        .legs
        .iter()
        .map(|l| {
            (
                l.sign,
                prices
                    .get(&(l.symbol.clone(), l.exchange.clone()))
                    .unwrap_or(&empty),
            )
        })
        .collect();
    let mut points = combined_premium(underlying.as_deref(), &legs);
    if points.is_empty() {
        return Reply::error(
            404,
            "No overlapping history across legs — option data may be unavailable for the selected range",
        );
    }
    if !p.explicit {
        points = cap_last_n_dates(points, req.days, |x| x.time);
    }
    let entry_net: f64 = p.legs.iter().map(|l| l.sign * l.entry_price).sum();
    data(json!({
        "underlying": p.base,
        "underlying_ltp": info_ltp(ctx, &p.h, &p.reference.0, &p.reference.1).await,
        "interval": req.interval,
        "tag": credit_tag(entry_net),
        "entry_net_premium": float(py_round(entry_net, 2)),
        "entry_abs_premium": float(py_round(entry_net.abs(), 2)),
        "legs_used": p.legs.len(),
        "underlying_available": underlying.is_some(),
        "series": points,
    }))
}

/// `POST /strategybuilder/api/multi-strike-oi`.
pub async fn multi_strike_oi(ctx: &AppState, req: &ChartRequest) -> Reply {
    let p = match chart_prologue(ctx, req) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let (underlying, leg_bars) = chart_histories(ctx, &p, &req.interval).await;
    let point = |t: i64, v: f64| json!({"time": t, "value": float(py_round(v, 2))});
    let cap = |v: Vec<Value>| {
        if p.explicit {
            v
        } else {
            cap_last_n_dates(v, req.days, |x| x["time"].as_i64().unwrap_or(0))
        }
    };
    let underlying_series: Vec<Value> = underlying
        .as_ref()
        .map(|u| u.iter().map(|b| point(b.time, b.close)).collect())
        .unwrap_or_default();
    let mut legs_out = Vec::new();
    for raw in &req.legs {
        let Some(l) = normalize_leg(raw) else {
            continue;
        };
        let series: Vec<Value> = leg_bars
            .get(&(l.symbol.clone(), l.exchange.clone()))
            .map(|b| b.iter().map(|x| point(x.time, x.oi)).collect())
            .unwrap_or_default();
        let has_oi = series
            .iter()
            .any(|x| x["value"].as_f64().unwrap_or(0.0) > 0.0);
        legs_out.push(json!({
            "symbol": l.symbol,
            "exchange": l.exchange,
            "side": l.side,
            "strike": raw.get("strike").cloned().unwrap_or(Value::Null),
            "option_type": raw.get("optionType").cloned().unwrap_or(Value::Null),
            "expiry": raw.get("expiry").cloned().unwrap_or(Value::Null),
            "has_oi": has_oi,
            "series": cap(series),
        }));
    }
    data(json!({
        "underlying": p.base,
        "underlying_ltp": info_ltp(ctx, &p.h, &p.reference.0, &p.reference.1).await,
        "interval": req.interval,
        "underlying_available": underlying.is_some(),
        "underlying_series": cap(underlying_series),
        "legs": legs_out,
    }))
}

/// `GET /straddlepnl/api/lotsize`: the first positive lot size of any
/// contract whose symbol starts with `underlying` on `exchange`.
pub fn lot_size(ctx: &AppState, underlying: &str, exchange: &str) -> Option<i64> {
    let snap = ctx.symbols.snapshot();
    snap.rows()
        .iter()
        .find(|r| r.exchange == exchange && r.symbol.starts_with(underlying) && r.lot_size > 0)
        .map(|r| i64::from(r.lot_size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legs_normalize_like_the_web() {
        let l = normalize_leg(&json!({"symbol": " NIFTY30OCT2625000CE ", "exchange": "nfo", "side": "sell", "price": "12.5"})).unwrap();
        assert_eq!(
            l,
            ChartLeg {
                symbol: "NIFTY30OCT2625000CE".into(),
                exchange: "NFO".into(),
                side: "SELL".into(),
                sign: 1.0,
                entry_price: 12.5
            }
        );
        assert_eq!(
            normalize_leg(
                &json!({"symbol": "X", "exchange": "NFO", "side": "BUY", "price": "abc"})
            )
            .unwrap()
            .entry_price,
            0.0
        );
        assert!(normalize_leg(
            &json!({"symbol": "X", "exchange": "NFO", "side": "BUY", "active": false})
        )
        .is_none());
        assert!(normalize_leg(
            &json!({"symbol": "X", "exchange": "NFO", "side": "BUY", "segment": "FUTURE"})
        )
        .is_none());
        assert!(
            normalize_leg(&json!({"symbol": "X", "exchange": "NFO", "side": "HOLD"})).is_none()
        );
        assert!(normalize_leg(&json!({"symbol": "", "exchange": "NFO", "side": "BUY"})).is_none());
        assert!(normalize_leg(&json!("leg")).is_none());
        // An empty segment is an option, like `(segment or "OPTION")`.
        assert!(normalize_leg(
            &json!({"symbol": "X", "exchange": "NFO", "side": "BUY", "segment": ""})
        )
        .is_some());
    }
}
