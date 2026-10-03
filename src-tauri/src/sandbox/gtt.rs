//! GTT (Good Till Triggered) in the sandbox (web `sandbox/gtt_manager.py`).
//!
//! A GTT holds one leg (`single`) or two mutually exclusive legs (`two-leg`
//! OCO). Margin is reserved at placement (the larger leg for OCO by default).
//! When the price crosses a leg's trigger, exactly one evaluator wins the
//! leg's claim (`pending -> triggering`, refused while a sibling is claimed
//! or the parent is no longer active), the parent moves to `triggered`, the
//! reservation is released and the leg's order is placed with the leg id
//! recorded on the order. A failed placement restores the reservation and
//! re-arms the GTT; if the reservation cannot be restored the GTT is marked
//! `rejected` rather than left active and unfunded.
//!
//! Statuses: GTT `active | triggered | cancelled | expired | rejected`; leg
//! `pending | triggering | triggered | cancelled`.

use super::clock::{gtt_ts, parse_ts, ts, IST};
use super::config::{OcoMarginMode, SandboxConfig};
use super::core::{blocking, Core, EngineCmd};
use super::events::{gtt_expired, gtt_triggered, Outbox};
use super::funds;
use super::locks::PositionKey;
use super::orders::{self, OrderRequest};
use super::replies::{GttEntry, GttLegOut, GttOrderbookReply, GttReply, ANALYZE, SUCCESS};
use super::types::{
    dec_from_f64, dec_to_db, money, rupees, Action, Product, SandboxError, SbResult, SymbolKey,
};
use chrono::{Duration, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;

/// A GTT with no explicit expiry rests for a year (Zerodha parity).
pub const DEFAULT_EXPIRY_DAYS: i64 = 365;

/// A GTT request, flat as the API sends it (web `PlaceGTTOrderSchema`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GttRequest {
    /// `SINGLE` or `OCO`.
    pub trigger_type: String,
    pub symbol: String,
    pub exchange: String,
    pub action: String,
    pub product: String,
    pub quantity: i64,
    /// Defaults to LIMIT.
    pub pricetype: String,
    pub price: Option<Decimal>,
    pub triggerprice_sl: Option<Decimal>,
    pub triggerprice_tg: Option<Decimal>,
    /// OCO stoploss leg limit.
    pub stoploss: Option<Decimal>,
    /// OCO target leg limit.
    pub target: Option<Decimal>,
    pub strategy: Option<String>,
    /// ISO date or datetime (offset honoured); default 365 days.
    pub expires_at: Option<String>,
}

/// A leg built from a request.
#[derive(Debug, Clone, PartialEq)]
pub struct LegSpec {
    pub trigger_price: Decimal,
    /// `below` (stoploss role, fires on a fall) or `above` (target role).
    pub direction: &'static str,
    pub action: String,
    pub quantity: i64,
    pub price: Decimal,
    pub pricetype: String,
}

fn pos(v: Option<Decimal>) -> Decimal {
    v.filter(|d| *d > Decimal::ZERO).unwrap_or(Decimal::ZERO)
}

/// Web `_build_legs`: SINGLE needs exactly one of the two triggers; OCO
/// needs both. Empty when the shape is wrong.
pub fn build_legs(r: &GttRequest) -> Vec<LegSpec> {
    let action = r.action.trim().to_ascii_uppercase();
    let pricetype = if r.pricetype.trim().is_empty() {
        "LIMIT".to_string()
    } else {
        r.pricetype.trim().to_ascii_uppercase()
    };
    let price = r.price.unwrap_or(Decimal::ZERO);
    let sl = pos(r.triggerprice_sl);
    let tg = pos(r.triggerprice_tg);
    if r.trigger_type.trim().eq_ignore_ascii_case("OCO") {
        if sl <= Decimal::ZERO || tg <= Decimal::ZERO {
            return vec![];
        }
        let stop_px = pos(r.stoploss);
        let target_px = pos(r.target);
        return vec![
            LegSpec {
                trigger_price: sl,
                direction: "below",
                action: action.clone(),
                quantity: r.quantity,
                price: if stop_px > Decimal::ZERO {
                    stop_px
                } else {
                    price
                },
                pricetype: pricetype.clone(),
            },
            LegSpec {
                trigger_price: tg,
                direction: "above",
                action,
                quantity: r.quantity,
                price: if target_px > Decimal::ZERO {
                    target_px
                } else {
                    price
                },
                pricetype,
            },
        ];
    }
    if (sl > Decimal::ZERO && tg > Decimal::ZERO) || (sl <= Decimal::ZERO && tg <= Decimal::ZERO) {
        return vec![];
    }
    vec![LegSpec {
        trigger_price: if sl > Decimal::ZERO { sl } else { tg },
        direction: if sl > Decimal::ZERO { "below" } else { "above" },
        action,
        quantity: r.quantity,
        price,
        pricetype,
    }]
}

/// Web `leg_is_triggered_by`: `above` fires at or over the trigger, anything
/// else at or under it.
pub fn leg_is_triggered_by(
    direction: &str,
    trigger: Option<Decimal>,
    ltp: Option<Decimal>,
) -> bool {
    let (Some(t), Some(p)) = (trigger, ltp) else {
        return false;
    };
    if direction.eq_ignore_ascii_case("above") {
        p >= t
    } else {
        p <= t
    }
}

/// Price a leg's margin is sized at: limit, else trigger, else LTP.
pub fn pricing_basis(price: Decimal, trigger: Decimal, last_price: Decimal) -> Decimal {
    if price > Decimal::ZERO {
        price
    } else if trigger > Decimal::ZERO {
        trigger
    } else {
        last_price
    }
}

/// `GTT-YYMMDD-<8 hex>`.
pub fn generate_gtt_id(now: NaiveDateTime) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("GTT-{}-{}", now.format("%y%m%d"), &u[..8])
}

/// Web `_resolve_expiry`: a supplied value is honoured (an offset is
/// converted to IST); anything unparseable falls back to the default.
pub fn resolve_expiry(supplied: Option<&str>, now: NaiveDateTime) -> NaiveDateTime {
    if let Some(s) = supplied.map(str::trim).filter(|s| !s.is_empty()) {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return dt.with_timezone(&IST).naive_local();
        }
        if let Ok(dt) = chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f%:z") {
            return dt.with_timezone(&IST).naive_local();
        }
        if let Some(dt) = parse_ts(s) {
            return dt;
        }
    }
    now + Duration::days(DEFAULT_EXPIRY_DAYS)
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct GttRow {
    pub gtt_id: String,
    pub user_id: String,
    pub strategy: Option<String>,
    pub trigger_type: String,
    pub symbol: String,
    pub exchange: String,
    pub last_price: Decimal,
    pub gtt_status: String,
    pub margin_blocked: Decimal,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl GttRow {
    fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            gtt_id: r.get("gtt_id")?,
            user_id: r.get("user_id")?,
            strategy: r.get("strategy")?,
            trigger_type: r.get("trigger_type")?,
            symbol: r.get("symbol")?,
            exchange: r.get("exchange")?,
            last_price: super::db::dec_col(r, "last_price")?,
            gtt_status: r.get("gtt_status")?,
            margin_blocked: super::db::dec_col(r, "margin_blocked")?,
            expires_at: r.get("expires_at")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegRow {
    pub id: i64,
    pub gtt_id: String,
    pub leg_number: i64,
    pub trigger_price: Decimal,
    pub trigger_direction: String,
    pub action: String,
    pub quantity: i64,
    pub price: Decimal,
    pub pricetype: String,
    pub product: String,
    pub leg_status: String,
    pub triggered_order_id: Option<String>,
    pub leg_margin: Decimal,
    pub claimed_at: Option<String>,
}

impl LegRow {
    fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            gtt_id: r.get("gtt_id")?,
            leg_number: r.get("leg_number")?,
            trigger_price: super::db::dec_col(r, "trigger_price")?,
            trigger_direction: r.get("trigger_direction")?,
            action: r.get("action")?,
            quantity: r.get("quantity")?,
            price: super::db::dec_col(r, "price")?,
            pricetype: r.get("pricetype")?,
            product: r.get("product")?,
            leg_status: r.get("leg_status")?,
            triggered_order_id: r.get("triggered_order_id")?,
            leg_margin: super::db::dec_col(r, "leg_margin")?,
            claimed_at: r.get("claimed_at")?,
        })
    }
}

pub fn get_gtt(conn: &Connection, gtt_id: &str) -> rusqlite::Result<Option<GttRow>> {
    conn.query_row(
        "SELECT * FROM sandbox_gtt WHERE gtt_id = ?1",
        params![gtt_id],
        GttRow::from_row,
    )
    .optional()
}

pub fn legs_of(conn: &Connection, gtt_id: &str) -> rusqlite::Result<Vec<LegRow>> {
    let mut stmt = conn
        .prepare_cached("SELECT * FROM sandbox_gtt_legs WHERE gtt_id = ?1 ORDER BY leg_number")?;
    let rows = stmt.query_map(params![gtt_id], LegRow::from_row)?;
    rows.collect()
}

pub fn get_leg(conn: &Connection, leg_id: i64) -> rusqlite::Result<Option<LegRow>> {
    conn.query_row(
        "SELECT * FROM sandbox_gtt_legs WHERE id = ?1",
        params![leg_id],
        LegRow::from_row,
    )
    .optional()
}

/// Pending legs of active GTTs, optionally for one symbol.
pub fn active_legs(
    conn: &Connection,
    key: Option<&SymbolKey>,
) -> rusqlite::Result<Vec<(LegRow, GttRow)>> {
    let base = "SELECT l.*, g.gtt_id AS g_gtt_id FROM sandbox_gtt_legs l
                JOIN sandbox_gtt g ON l.gtt_id = g.gtt_id
                WHERE g.gtt_status = 'active' AND l.leg_status = 'pending'";
    let mut legs = Vec::new();
    match key {
        Some(k) => {
            let mut stmt = conn.prepare_cached(&format!(
                "{base} AND g.symbol = ?1 AND g.exchange = ?2 ORDER BY l.id"
            ))?;
            for r in stmt.query_map(params![k.symbol, k.exchange], LegRow::from_row)? {
                legs.push(r?);
            }
        }
        None => {
            let mut stmt = conn.prepare_cached(&format!("{base} ORDER BY l.id"))?;
            for r in stmt.query_map([], LegRow::from_row)? {
                legs.push(r?);
            }
        }
    }
    let mut out = Vec::with_capacity(legs.len());
    for l in legs {
        if let Some(g) = get_gtt(conn, &l.gtt_id)? {
            out.push((l, g));
        }
    }
    Ok(out)
}

fn not_found(trigger_id: &str) -> SandboxError {
    SandboxError::not_found(format!("No active GTT with trigger_id '{trigger_id}'"))
}

fn leg_margin(
    cfg: &SandboxConfig,
    core: &Core,
    symbol: &str,
    exchange: &str,
    product: &str,
    leg: &LegSpec,
    last_price: Decimal,
) -> SbResult<Decimal> {
    if core.symbol(symbol, exchange).is_none() {
        return Err(SandboxError::bad_request("Symbol not found"));
    }
    let product = Product::parse(product).unwrap_or(Product::Cnc);
    let action = Action::parse(&leg.action).unwrap_or(Action::Buy);
    Ok(funds::margin_required(
        cfg,
        symbol,
        exchange,
        product,
        leg.quantity,
        pricing_basis(leg.price, leg.trigger_price, last_price),
        action,
    ))
}

fn blocked_for(cfg: &SandboxConfig, two_leg: bool, margins: &[Decimal]) -> Decimal {
    if two_leg && cfg.gtt_oco_margin_mode == OcoMarginMode::Max {
        margins.iter().copied().max().unwrap_or(Decimal::ZERO)
    } else {
        margins.iter().copied().sum()
    }
}

// ---------------------------------------------------------------------------
// Place / modify / cancel / list
// ---------------------------------------------------------------------------

pub(crate) async fn place(core: &Arc<Core>, req: GttRequest) -> SbResult<GttReply> {
    let legs = build_legs(&req);
    if legs.is_empty() {
        return Err(SandboxError::bad_request(
            "A SINGLE GTT needs exactly one of triggerprice_sl or triggerprice_tg; an OCO needs both.",
        ));
    }
    let symbol = req.symbol.trim().to_string();
    let exchange = req.exchange.trim().to_ascii_uppercase();
    let last_price = core
        .quotes
        .quote(&symbol, &exchange)
        .await
        .map(|q| q.ltp)
        .unwrap_or(Decimal::ZERO);
    let reply = blocking(core, move |c| {
        c.db.with_tx(|tx| {
            let cfg = SandboxConfig::load(tx)?;
            let product = req.product.trim().to_ascii_uppercase();
            let mut margins = Vec::with_capacity(legs.len());
            for leg in &legs {
                margins.push(leg_margin(&cfg, c, &symbol, &exchange, &product, leg, last_price)?);
            }
            let two_leg = legs.len() == 2;
            let blocked = blocked_for(&cfg, two_leg, &margins);
            let now = c.now();
            let now_s = ts(now);
            let staged = funds::apply(tx, c.user(), cfg.starting_capital, &now_s, |f| {
                funds::margin_delta(f, blocked)
            })?;
            if let Err(r) = staged {
                return Err(SandboxError::bad_request(r.0));
            }
            let gtt_now = gtt_ts(now);
            let expires = gtt_ts(resolve_expiry(req.expires_at.as_deref(), now));
            let mut gtt_id = generate_gtt_id(now);
            for _ in 0..5 {
                let r = tx.execute(
                    "INSERT INTO sandbox_gtt (gtt_id, user_id, strategy, trigger_type, symbol, exchange,
                       last_price, gtt_status, margin_blocked, expires_at, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?9, ?10, ?10)",
                    params![
                        gtt_id,
                        c.user(),
                        req.strategy,
                        if two_leg { "two-leg" } else { "single" },
                        symbol,
                        exchange,
                        dec_to_db(last_price),
                        dec_to_db(blocked),
                        expires,
                        gtt_now
                    ],
                );
                match r {
                    Ok(_) => break,
                    Err(rusqlite::Error::SqliteFailure(e, _))
                        if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        gtt_id = generate_gtt_id(now);
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            for (i, (leg, margin)) in legs.iter().zip(margins.iter()).enumerate() {
                tx.execute(
                    "INSERT INTO sandbox_gtt_legs (gtt_id, leg_number, trigger_price, trigger_direction,
                       action, quantity, price, pricetype, product, leg_status, leg_margin,
                       created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?11, ?11)",
                    params![
                        gtt_id,
                        (i + 1) as i64,
                        dec_to_db(leg.trigger_price),
                        leg.direction,
                        leg.action,
                        leg.quantity,
                        dec_to_db(leg.price),
                        leg.pricetype,
                        product,
                        dec_to_db(*margin),
                        gtt_now
                    ],
                )?;
            }
            tracing::info!(
                "Sandbox GTT {} placed: {} {} margin {}",
                gtt_id,
                symbol,
                exchange,
                blocked
            );
            Ok(GttReply {
                status: SUCCESS,
                mode: ANALYZE,
                trigger_id: gtt_id,
            })
        })
    })
    .await?;
    core.notify_engine(EngineCmd::Rebuild);
    Ok(reply)
}

fn immutable_violation(gtt: &GttRow, legs: &[LegRow], r: &GttRequest) -> Option<String> {
    let action = r.action.trim().to_ascii_uppercase();
    if let Some(first) = legs.first() {
        if !action.is_empty() && action != first.action.to_ascii_uppercase() {
            return Some(format!(
                "action cannot be changed on an existing GTT ({} -> {action}). Cancel and re-place.",
                first.action
            ));
        }
    }
    let symbol = r.symbol.trim().to_ascii_uppercase();
    if !symbol.is_empty() && symbol != gtt.symbol.to_ascii_uppercase() {
        return Some(format!(
            "symbol cannot be changed on an existing GTT ({} -> {symbol}). Cancel and re-place.",
            gtt.symbol
        ));
    }
    let exchange = r.exchange.trim().to_ascii_uppercase();
    if !exchange.is_empty() && exchange != gtt.exchange.to_ascii_uppercase() {
        return Some(format!(
            "exchange cannot be changed on an existing GTT ({} -> {exchange}). Cancel and re-place.",
            gtt.exchange
        ));
    }
    let requested = r.trigger_type.trim().to_ascii_uppercase();
    let current = if gtt.trigger_type == "two-leg" {
        "OCO"
    } else {
        "SINGLE"
    };
    if !requested.is_empty() && requested != current {
        return Some(format!(
            "trigger_type cannot be changed ({current} -> {requested}). Cancel and re-place."
        ));
    }
    None
}

pub(crate) async fn modify(
    core: &Arc<Core>,
    trigger_id: &str,
    req: GttRequest,
) -> SbResult<GttReply> {
    let trigger_id = trigger_id.to_string();
    blocking(core, move |c| {
        c.db.with_tx(|tx| {
            let Some(gtt) = get_gtt(tx, &trigger_id)?
                .filter(|g| g.user_id == c.user() && g.gtt_status == "active")
            else {
                return Err(not_found(&trigger_id));
            };
            let existing = legs_of(tx, &trigger_id)?;
            if let Some(m) = immutable_violation(&gtt, &existing, &req) {
                return Err(SandboxError::bad_request(m));
            }
            let legs = build_legs(&req);
            if legs.is_empty() || legs.len() != existing.len() {
                return Err(SandboxError::bad_request(format!(
                    "Modify must keep the same trigger shape: this GTT has {} leg(s).",
                    existing.len()
                )));
            }
            let cfg = SandboxConfig::load(tx)?;
            let product = if req.product.trim().is_empty() {
                existing.first().map(|l| l.product.clone()).unwrap_or_default()
            } else {
                req.product.trim().to_ascii_uppercase()
            };
            let mut margins = Vec::with_capacity(legs.len());
            for leg in &legs {
                margins.push(leg_margin(&cfg, c, &gtt.symbol, &gtt.exchange, &product, leg, gtt.last_price)?);
            }
            let new_blocked = blocked_for(&cfg, gtt.trigger_type == "two-leg", &margins);
            let delta = new_blocked - gtt.margin_blocked;
            let now = c.now();
            let n = tx.execute(
                "UPDATE sandbox_gtt SET margin_blocked = ?1, updated_at = ?2
                 WHERE gtt_id = ?3 AND user_id = ?4 AND gtt_status = 'active'",
                params![dec_to_db(new_blocked), gtt_ts(now), trigger_id, c.user()],
            )?;
            if n != 1 {
                return Err(not_found(&trigger_id));
            }
            let staged = funds::apply(tx, c.user(), cfg.starting_capital, &ts(now), |f| {
                funds::margin_delta(f, delta)
            })?;
            if let Err(r) = staged {
                let status = if delta > Decimal::ZERO { 400 } else { 500 };
                return Err(SandboxError::new(status, format!("{}. The GTT is unchanged.", r.0))
                    .with_trigger_id(trigger_id.clone()));
            }
            for ((old, new), margin) in existing.iter().zip(legs.iter()).zip(margins.iter()) {
                tx.execute(
                    "UPDATE sandbox_gtt_legs SET trigger_price = ?1, trigger_direction = ?2, quantity = ?3,
                       price = ?4, pricetype = ?5, action = ?6, leg_margin = ?7, updated_at = ?8
                     WHERE id = ?9",
                    params![
                        dec_to_db(new.trigger_price),
                        new.direction,
                        new.quantity,
                        dec_to_db(new.price),
                        new.pricetype,
                        new.action,
                        dec_to_db(*margin),
                        gtt_ts(now),
                        old.id
                    ],
                )?;
            }
            Ok(GttReply {
                status: SUCCESS,
                mode: ANALYZE,
                trigger_id: trigger_id.clone(),
            })
        })
    })
    .await
}

pub(crate) async fn cancel(core: &Arc<Core>, trigger_id: &str) -> SbResult<GttReply> {
    let trigger_id = trigger_id.to_string();
    blocking(core, move |c| {
        c.db.with_tx(|tx| {
            let Some(gtt) = get_gtt(tx, &trigger_id)?
                .filter(|g| g.user_id == c.user() && g.gtt_status == "active")
            else {
                return Err(not_found(&trigger_id));
            };
            let released = gtt.margin_blocked;
            let now = c.now();
            let n = tx.execute(
                "UPDATE sandbox_gtt SET gtt_status = 'cancelled', margin_blocked = '0', updated_at = ?1
                 WHERE gtt_id = ?2 AND user_id = ?3 AND gtt_status = 'active'",
                params![gtt_ts(now), trigger_id, c.user()],
            )?;
            if n != 1 {
                return Err(not_found(&trigger_id));
            }
            tx.execute(
                "UPDATE sandbox_gtt_legs SET leg_status = 'cancelled', claimed_at = NULL, updated_at = ?1
                 WHERE gtt_id = ?2 AND leg_status IN ('pending','triggering')",
                params![gtt_ts(now), trigger_id],
            )?;
            if released > Decimal::ZERO {
                let cfg = SandboxConfig::load(tx)?;
                let staged = funds::apply(tx, c.user(), cfg.starting_capital, &ts(now), |f| {
                    funds::margin_delta(f, -released)
                })?;
                if let Err(r) = staged {
                    return Err(SandboxError::internal(format!(
                        "Could not release {} margin ({}). The GTT is unchanged - retry the cancel.",
                        rupees(released),
                        r.0
                    ))
                    .with_trigger_id(trigger_id.clone()));
                }
            }
            Ok(GttReply {
                status: SUCCESS,
                mode: ANALYZE,
                trigger_id: trigger_id.clone(),
            })
        })
    })
    .await
}

/// GTT order book, newest first. `status`: `Some("active")` (the web
/// default) or `None` for every status.
pub(crate) async fn list(core: &Arc<Core>, status: Option<String>) -> SbResult<GttOrderbookReply> {
    blocking(core, move |c| {
        c.db.with_conn(|conn| {
            let mut rows = Vec::new();
            {
                let (sql, filter) = match &status {
                    Some(s) => (
                        "SELECT * FROM sandbox_gtt WHERE user_id = ?1 AND gtt_status = ?2
                         ORDER BY created_at DESC, id DESC",
                        Some(s.clone()),
                    ),
                    None => (
                        "SELECT * FROM sandbox_gtt WHERE user_id = ?1 ORDER BY created_at DESC, id DESC",
                        None,
                    ),
                };
                let mut stmt = conn.prepare_cached(sql)?;
                let it = match &filter {
                    Some(f) => stmt.query_map(params![c.user(), f], GttRow::from_row)?,
                    None => stmt.query_map(params![c.user()], GttRow::from_row)?,
                };
                for r in it {
                    rows.push(r?);
                }
            }
            let mut data = Vec::with_capacity(rows.len());
            for g in rows {
                let legs = legs_of(conn, &g.gtt_id)?;
                data.push(GttEntry {
                    trigger_id: g.gtt_id.clone(),
                    trigger_type: g.trigger_type.clone(),
                    status: g.gtt_status.clone(),
                    symbol: g.symbol.clone(),
                    exchange: g.exchange.clone(),
                    trigger_prices: legs.iter().map(|l| money(l.trigger_price)).collect(),
                    last_price: money(g.last_price),
                    legs: legs
                        .iter()
                        .map(|l| GttLegOut {
                            action: l.action.clone(),
                            quantity: l.quantity,
                            price: money(l.price),
                            pricetype: l.pricetype.clone(),
                            product: l.product.clone(),
                            triggered_order_id: l.triggered_order_id.clone(),
                        })
                        .collect(),
                    created_at: g.created_at.clone(),
                    updated_at: g.updated_at.clone(),
                    expires_at: g.expires_at.clone().unwrap_or_default(),
                    strategy: g.strategy.clone(),
                    margin_blocked: money(g.margin_blocked),
                });
            }
            Ok::<_, SandboxError>(GttOrderbookReply {
                status: SUCCESS,
                mode: ANALYZE,
                data,
            })
        })
    })
    .await
}

// ---------------------------------------------------------------------------
// Claim / fire
// ---------------------------------------------------------------------------

/// Claim a leg for firing: one conditional UPDATE, refused when the leg is
/// not pending, a sibling is claimed or fired (OCO exclusivity), or the
/// parent is no longer active. Exactly one caller wins.
pub fn try_claim(conn: &Connection, leg_id: i64, now: NaiveDateTime) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "UPDATE sandbox_gtt_legs SET leg_status = 'triggering', claimed_at = ?1
         WHERE id = ?2 AND leg_status = 'pending'
           AND NOT EXISTS (
             SELECT 1 FROM sandbox_gtt_legs s
             WHERE s.gtt_id = (SELECT gtt_id FROM sandbox_gtt_legs WHERE id = ?2)
               AND s.id != ?2 AND s.leg_status IN ('triggering','triggered'))
           AND EXISTS (
             SELECT 1 FROM sandbox_gtt g
             WHERE g.gtt_id = (SELECT gtt_id FROM sandbox_gtt_legs WHERE id = ?2)
               AND g.gtt_status = 'active')",
        params![gtt_ts(now), leg_id],
    )?;
    Ok(n == 1)
}

fn revert_claim(conn: &Connection, leg_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sandbox_gtt_legs SET leg_status = 'pending', claimed_at = NULL
         WHERE id = ?1 AND leg_status = 'triggering'",
        params![leg_id],
    )?;
    Ok(())
}

pub(crate) async fn claim(core: &Arc<Core>, leg_id: i64) -> SbResult<bool> {
    blocking(core, move |c| {
        let now = c.now();
        Ok(c.db.with_tx(|tx| try_claim(tx, leg_id, now))?)
    })
    .await
}

/// Place the order for a leg this caller claimed. Returns `true` when the
/// order was placed.
pub(crate) async fn fire(core: &Arc<Core>, leg_id: i64, price: Decimal) -> SbResult<bool> {
    let loaded = blocking(core, move |c| {
        Ok(c.db.with_conn(|conn| -> rusqlite::Result<_> {
            let Some(leg) = get_leg(conn, leg_id)? else {
                return Ok(None);
            };
            let gtt = get_gtt(conn, &leg.gtt_id)?;
            Ok(gtt.map(|g| (leg, g)))
        })?)
    })
    .await?;
    let Some((leg, gtt)) = loaded else {
        return Ok(false);
    };
    if leg.leg_status != "triggering" {
        return Ok(false);
    }
    // Never wait on the tick path: if another order on this position is in
    // flight, hand the claim back and fire on the next price.
    let Some(guard) = core.locks.try_lock(PositionKey::new(
        &gtt.user_id,
        &gtt.exchange,
        &gtt.symbol,
        &leg.product,
    )) else {
        blocking(core, move |c| {
            Ok(c.db.with_tx(|tx| revert_claim(tx, leg_id))?)
        })
        .await?;
        return Ok(false);
    };

    // Take the parent and release its reservation in one transaction.
    let gtt_id = gtt.gtt_id.clone();
    let released = blocking(core, move |c| {
        let r: SbResult<Option<Decimal>> = c.db.with_tx(|tx| {
            let now = c.now();
            let n = tx.execute(
                "UPDATE sandbox_gtt SET gtt_status = 'triggered', updated_at = ?1
                 WHERE gtt_id = ?2 AND gtt_status = 'active'",
                params![gtt_ts(now), gtt_id],
            )?;
            if n != 1 {
                revert_claim(tx, leg_id)?;
                return Ok(None);
            }
            let Some(g) = get_gtt(tx, &gtt_id)? else {
                return Ok(None);
            };
            let released = g.margin_blocked;
            if released > Decimal::ZERO {
                let cfg = SandboxConfig::load(tx)?;
                let staged = funds::apply(tx, &g.user_id, cfg.starting_capital, &ts(now), |f| {
                    funds::margin_delta(f, -released)
                })?;
                if let Err(r) = staged {
                    return Err(SandboxError::internal(r.0));
                }
                tx.execute(
                    "UPDATE sandbox_gtt SET margin_blocked = '0' WHERE gtt_id = ?1",
                    params![gtt_id],
                )?;
            }
            Ok(Some(released))
        });
        match r {
            Ok(v) => Ok(v),
            Err(e) => {
                tracing::warn!(
                    "GTT {} not fired: its margin could not be released ({})",
                    gtt_id,
                    e.message
                );
                c.db.with_tx(|tx| revert_claim(tx, leg_id))?;
                Ok(None)
            }
        }
    })
    .await?;
    let Some(released) = released else {
        return Ok(false);
    };

    let req = OrderRequest {
        symbol: gtt.symbol.clone(),
        exchange: gtt.exchange.clone(),
        action: leg.action.clone(),
        quantity: leg.quantity,
        price: Some(leg.price),
        trigger_price: None,
        price_type: leg.pricetype.clone(),
        product: leg.product.clone(),
        strategy: gtt
            .strategy
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "GTT".to_string()),
    };
    let placed = orders::place_locked(core, &guard, req, None, Some(leg.id)).await;
    drop(guard);
    let gtt_id = gtt.gtt_id.clone();
    match placed {
        Ok(p) => {
            let orderid = p.orderid.clone();
            let two_leg = gtt.trigger_type == "two-leg";
            let g2 = gtt.clone();
            blocking(core, move |c| {
                let mut outbox = Outbox::new();
                c.db.with_tx(|tx| -> SbResult<()> {
                    let now = gtt_ts(c.now());
                    tx.execute(
                        "UPDATE sandbox_gtt_legs SET leg_status = 'triggered', triggered_order_id = ?1,
                           claimed_at = NULL, updated_at = ?2 WHERE id = ?3",
                        params![orderid, now, leg_id],
                    )?;
                    if two_leg {
                        tx.execute(
                            "UPDATE sandbox_gtt_legs SET leg_status = 'cancelled', claimed_at = NULL, updated_at = ?1
                             WHERE gtt_id = ?2 AND id != ?3 AND leg_status IN ('pending','triggering')",
                            params![now, gtt_id, leg_id],
                        )?;
                    }
                    let prices: Vec<f64> = legs_of(tx, &gtt_id)?
                        .iter()
                        .map(|l| money(l.trigger_price))
                        .collect();
                    outbox.push(gtt_triggered(
                        &g2.gtt_id,
                        &g2.symbol,
                        &g2.exchange,
                        g2.strategy.as_deref().unwrap_or(""),
                        &prices,
                        &orderid,
                    ));
                    Ok(())
                })?;
                c.publish(outbox);
                tracing::info!("Sandbox GTT {} fired at {}; order {}", gtt_id, price, orderid);
                Ok(true)
            })
            .await
        }
        Err(e) => {
            tracing::warn!("Sandbox GTT {} leg order refused: {}", gtt_id, e.message);
            blocking(core, move |c| {
                c.db.with_tx(|tx| -> SbResult<()> {
                    let cfg = SandboxConfig::load(tx)?;
                    let now = c.now();
                    let mut restored = true;
                    if released > Decimal::ZERO {
                        let r = funds::apply(tx, c.user(), cfg.starting_capital, &ts(now), |f| {
                            funds::margin_delta(f, released)
                        })?;
                        restored = r.is_ok();
                    }
                    if restored {
                        tx.execute(
                            "UPDATE sandbox_gtt SET gtt_status = 'active', margin_blocked = ?1, updated_at = ?2
                             WHERE gtt_id = ?3 AND gtt_status = 'triggered'",
                            params![dec_to_db(released), gtt_ts(now), gtt_id],
                        )?;
                    } else {
                        tracing::warn!(
                            "GTT {} could not get its margin back after a refused order; marking it rejected",
                            gtt_id
                        );
                        tx.execute(
                            "UPDATE sandbox_gtt SET gtt_status = 'rejected', margin_blocked = '0', updated_at = ?1
                             WHERE gtt_id = ?2",
                            params![gtt_ts(now), gtt_id],
                        )?;
                    }
                    revert_claim(tx, leg_id)?;
                    Ok(())
                })?;
                Ok(false)
            })
            .await
        }
    }
}

/// Evaluate a price against the pending legs of one symbol.
pub(crate) async fn on_price(core: &Arc<Core>, key: &SymbolKey, ltp: Decimal) -> SbResult<usize> {
    let k = key.clone();
    let legs = blocking(core, move |c| {
        Ok(c.db.with_conn(|conn| active_legs(conn, Some(&k)))?)
    })
    .await?;
    let mut fired = 0;
    for (leg, _g) in legs {
        if leg_is_triggered_by(&leg.trigger_direction, Some(leg.trigger_price), Some(ltp))
            && claim(core, leg.id).await?
            && fire(core, leg.id, ltp).await?
        {
            fired += 1;
        }
    }
    Ok(fired)
}

/// Evaluate every pending leg against one batch of quotes (polling engine
/// and catch-up).
pub(crate) async fn poll_active(core: &Arc<Core>) -> SbResult<usize> {
    let legs = blocking(core, |c| {
        Ok(c.db.with_conn(|conn| active_legs(conn, None))?)
    })
    .await?;
    if legs.is_empty() {
        return Ok(0);
    }
    let mut keys: Vec<SymbolKey> = legs
        .iter()
        .map(|(_, g)| SymbolKey::new(g.symbol.clone(), g.exchange.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    let quotes: HashMap<SymbolKey, Decimal> = core
        .quotes
        .quotes(&keys)
        .await
        .into_iter()
        .map(|(k, q)| (k, q.ltp))
        .collect();
    let mut fired = 0;
    for (leg, g) in legs {
        let Some(ltp) = quotes.get(&SymbolKey::new(g.symbol.clone(), g.exchange.clone())) else {
            continue;
        };
        if leg_is_triggered_by(&leg.trigger_direction, Some(leg.trigger_price), Some(*ltp))
            && claim(core, leg.id).await?
            && fire(core, leg.id, *ltp).await?
        {
            fired += 1;
        }
    }
    Ok(fired)
}

// ---------------------------------------------------------------------------
// Maintenance
// ---------------------------------------------------------------------------

/// Revert legs stuck in `triggering` past the claim timeout, then recover
/// parents an interrupted fire left `triggered`. Returns legs reverted.
pub(crate) fn reclaim_stranded(conn: &Connection, core: &Core) -> SbResult<usize> {
    let cfg = SandboxConfig::load(conn)?;
    let now = core.now();
    let cutoff = gtt_ts(now - Duration::seconds(cfg.gtt_claim_timeout_sec));
    let n = conn.execute(
        "UPDATE sandbox_gtt_legs SET leg_status = 'pending', claimed_at = NULL
         WHERE leg_status = 'triggering' AND claimed_at IS NOT NULL AND claimed_at < ?1",
        params![cutoff],
    )?;
    if n > 0 {
        tracing::warn!(
            "Reclaimed {} GTT leg(s) left mid-fire by an interrupted run",
            n
        );
    }
    // Parents left 'triggered' with no placed child order.
    let stranded: Vec<GttRow> = {
        let mut stmt =
            conn.prepare_cached("SELECT * FROM sandbox_gtt WHERE gtt_status = 'triggered'")?;
        let rows = stmt.query_map([], GttRow::from_row)?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for g in stranded {
        let legs = legs_of(conn, &g.gtt_id)?;
        if legs.iter().any(|l| l.triggered_order_id.is_some()) {
            continue;
        }
        let mut placed: Option<(i64, String)> = None;
        for l in &legs {
            let o: Option<String> = conn
                .query_row(
                    "SELECT orderid FROM sandbox_orders WHERE gtt_leg_id = ?1 AND order_status != 'rejected'",
                    params![l.id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(o) = o {
                placed = Some((l.id, o));
                break;
            }
        }
        if let Some((leg_id, orderid)) = placed {
            conn.execute(
                "UPDATE sandbox_gtt_legs SET leg_status = 'triggered', triggered_order_id = ?1, claimed_at = NULL
                 WHERE id = ?2",
                params![orderid, leg_id],
            )?;
            continue;
        }
        if !legs
            .iter()
            .any(|l| l.leg_status == "pending" || l.leg_status == "triggering")
        {
            continue;
        }
        let (required, reblock) = if g.margin_blocked > Decimal::ZERO {
            (g.margin_blocked, false)
        } else {
            (
                legs.iter()
                    .map(|l| l.leg_margin)
                    .max()
                    .unwrap_or(Decimal::ZERO),
                true,
            )
        };
        let mut restored = true;
        if reblock && required > Decimal::ZERO {
            restored = funds::apply(conn, &g.user_id, cfg.starting_capital, &ts(now), |f| {
                funds::margin_delta(f, required)
            })?
            .is_ok();
        }
        if !restored {
            conn.execute(
                "UPDATE sandbox_gtt SET gtt_status = 'rejected' WHERE gtt_id = ?1",
                params![g.gtt_id],
            )?;
            continue;
        }
        conn.execute(
            "UPDATE sandbox_gtt SET gtt_status = 'active', margin_blocked = ?1
             WHERE gtt_id = ?2 AND gtt_status = 'triggered'",
            params![dec_to_db(required), g.gtt_id],
        )?;
        conn.execute(
            "UPDATE sandbox_gtt_legs SET leg_status = 'pending', claimed_at = NULL
             WHERE gtt_id = ?1 AND leg_status = 'triggering'",
            params![g.gtt_id],
        )?;
    }
    Ok(n)
}

/// Expire past-due active GTTs and release their margin. Returns how many.
pub(crate) fn expire_due(conn: &Connection, core: &Core, outbox: &mut Outbox) -> SbResult<usize> {
    let now = core.now();
    let now_g = gtt_ts(now);
    let due: Vec<GttRow> = {
        let mut stmt = conn.prepare_cached(
            "SELECT * FROM sandbox_gtt WHERE gtt_status = 'active' AND expires_at IS NOT NULL AND expires_at < ?1",
        )?;
        let rows = stmt.query_map(params![now_g], GttRow::from_row)?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let cfg = SandboxConfig::load(conn)?;
    let mut n = 0;
    for g in due {
        conn.execute_batch("SAVEPOINT gtt_expire")?;
        let claimed = conn.execute(
            "UPDATE sandbox_gtt SET gtt_status = 'expired', margin_blocked = '0', updated_at = ?1
             WHERE gtt_id = ?2 AND gtt_status = 'active'",
            params![now_g, g.gtt_id],
        )?;
        if claimed != 1 {
            conn.execute_batch("RELEASE gtt_expire")?;
            continue;
        }
        conn.execute(
            "UPDATE sandbox_gtt_legs SET leg_status = 'cancelled', claimed_at = NULL
             WHERE gtt_id = ?1 AND leg_status = 'pending'",
            params![g.gtt_id],
        )?;
        let released = g.margin_blocked;
        let ok = if released > Decimal::ZERO {
            funds::apply(conn, &g.user_id, cfg.starting_capital, &ts(now), |f| {
                funds::margin_delta(f, -released)
            })?
            .is_ok()
        } else {
            true
        };
        if !ok {
            conn.execute_batch("ROLLBACK TO gtt_expire; RELEASE gtt_expire")?;
            tracing::warn!(
                "GTT {} past expiry but its margin could not be released; retrying later",
                g.gtt_id
            );
            continue;
        }
        conn.execute_batch("RELEASE gtt_expire")?;
        outbox.push(gtt_expired(
            &g.gtt_id,
            &g.symbol,
            &g.exchange,
            g.strategy.as_deref().unwrap_or(""),
        ));
        n += 1;
    }
    Ok(n)
}

/// Run reclaim and expiry in one transaction.
pub(crate) async fn maintain(core: &Arc<Core>) -> SbResult<(usize, usize)> {
    blocking(core, |c| {
        let mut outbox = Outbox::new();
        let r = c.db.with_tx(|tx| {
            let reclaimed = reclaim_stranded(tx, c)?;
            let expired = expire_due(tx, c, &mut outbox)?;
            Ok::<_, SandboxError>((reclaimed, expired))
        })?;
        c.publish(outbox);
        Ok(r)
    })
    .await
}

/// Parse a float from the API into a decimal (adapters).
pub fn dec(v: f64) -> Decimal {
    dec_from_f64(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn single(action: &str, sl: Option<Decimal>, tg: Option<Decimal>) -> GttRequest {
        GttRequest {
            trigger_type: "SINGLE".into(),
            action: action.into(),
            quantity: 10,
            price: Some(dec!(100)),
            triggerprice_sl: sl,
            triggerprice_tg: tg,
            ..GttRequest::default()
        }
    }

    // TestTriggerEvaluation
    #[test]
    fn test_direction() {
        for (dir, t, l, exp) in [
            ("above", dec!(100), dec!(101), true),
            ("above", dec!(100), dec!(100), true),
            ("above", dec!(100), dec!(99), false),
            ("below", dec!(100), dec!(99), true),
            ("below", dec!(100), dec!(100), true),
            ("below", dec!(100), dec!(101), false),
        ] {
            assert_eq!(
                leg_is_triggered_by(dir, Some(t), Some(l)),
                exp,
                "{dir} {t} {l}"
            );
        }
    }

    #[test]
    fn test_case_insensitive() {
        assert!(leg_is_triggered_by(
            "ABOVE",
            Some(dec!(100)),
            Some(dec!(101))
        ));
    }

    #[test]
    fn test_missing_values_never_trigger() {
        assert!(!leg_is_triggered_by("below", None, Some(dec!(100))));
        assert!(!leg_is_triggered_by("below", Some(dec!(100)), None));
        assert!(!leg_is_triggered_by("below", None, None));
    }

    #[test]
    fn test_decimal_and_float_compare_exactly() {
        assert!(leg_is_triggered_by(
            "above",
            Some(dec!(100.05)),
            Some(dec_from_f64(100.05))
        ));
        assert!(!leg_is_triggered_by(
            "above",
            Some(dec!(100.05)),
            Some(dec_from_f64(100.04))
        ));
    }

    #[test]
    fn test_unknown_direction_defaults_to_below() {
        assert!(leg_is_triggered_by("", Some(dec!(100)), Some(dec!(99))));
        assert!(!leg_is_triggered_by("", Some(dec!(100)), Some(dec!(101))));
    }

    // TestDocumentedScenarios
    #[test]
    fn test_buy_on_dip_fires_when_price_falls() {
        let leg = &build_legs(&single("BUY", Some(dec!(95)), None))[0];
        assert_eq!(leg.direction, "below");
        assert!(leg_is_triggered_by(
            leg.direction,
            Some(dec!(95)),
            Some(dec!(94))
        ));
        assert!(!leg_is_triggered_by(
            leg.direction,
            Some(dec!(95)),
            Some(dec!(96))
        ));
    }

    #[test]
    fn test_sell_at_target_fires_when_price_rises() {
        let leg = &build_legs(&single("SELL", None, Some(dec!(110))))[0];
        assert_eq!(leg.direction, "above");
        assert!(leg_is_triggered_by(
            leg.direction,
            Some(dec!(110)),
            Some(dec!(111))
        ));
        assert!(!leg_is_triggered_by(
            leg.direction,
            Some(dec!(110)),
            Some(dec!(109))
        ));
    }

    #[test]
    fn test_sell_stoploss_fires_when_price_falls() {
        let leg = &build_legs(&single("SELL", Some(dec!(95)), None))[0];
        assert_eq!(leg.direction, "below");
    }

    #[test]
    fn test_buy_breakout_fires_when_price_rises() {
        let leg = &build_legs(&single("BUY", None, Some(dec!(110))))[0];
        assert_eq!(leg.direction, "above");
    }

    #[test]
    fn test_oco_legs_get_opposite_directions() {
        let legs = build_legs(&GttRequest {
            trigger_type: "OCO".into(),
            action: "SELL".into(),
            quantity: 10,
            price: Some(dec!(100)),
            triggerprice_sl: Some(dec!(95)),
            stoploss: Some(dec!(94)),
            triggerprice_tg: Some(dec!(110)),
            target: Some(dec!(111)),
            ..GttRequest::default()
        });
        assert_eq!(
            legs.iter().map(|l| l.direction).collect::<Vec<_>>(),
            vec!["below", "above"]
        );
        assert_eq!(legs[0].price, dec!(94));
        assert_eq!(legs[1].price, dec!(111));
    }

    // TestLegConstruction
    #[test]
    fn test_single_with_both_triggers_is_rejected() {
        assert!(build_legs(&single("BUY", Some(dec!(95)), Some(dec!(110)))).is_empty());
    }

    #[test]
    fn test_single_without_any_trigger_is_rejected() {
        assert!(build_legs(&single("BUY", None, None)).is_empty());
    }

    #[test]
    fn test_oco_requires_both_triggers() {
        let mut r = single("SELL", Some(dec!(95)), None);
        r.trigger_type = "OCO".into();
        assert!(build_legs(&r).is_empty());
    }

    #[test]
    fn test_oco_falls_back_to_price_when_a_limit_is_absent() {
        let mut r = single("SELL", Some(dec!(95)), Some(dec!(110)));
        r.trigger_type = "OCO".into();
        let legs = build_legs(&r);
        assert_eq!(legs[0].price, dec!(100));
        assert_eq!(legs[1].price, dec!(100));
    }

    // TestIdFormat
    #[test]
    fn test_gtt_id_shape_and_uniqueness() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 3)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap();
        let id = generate_gtt_id(now);
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts[0], "GTT");
        assert_eq!(parts[1], "261003");
        assert_eq!(parts[2].len(), 8);
        let set: std::collections::HashSet<String> =
            (0..200).map(|_| generate_gtt_id(now)).collect();
        assert_eq!(set.len(), 200);
    }

    // TestSuppliedExpiry / TestTimezoneAwareExpiry
    #[test]
    fn test_expiry_resolution() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 3)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap();
        assert_eq!(resolve_expiry(None, now), now + Duration::days(365));
        assert_eq!(
            resolve_expiry(Some("garbage"), now),
            now + Duration::days(365)
        );
        assert_eq!(
            resolve_expiry(Some("2026-12-31"), now),
            chrono::NaiveDate::from_ymd_opt(2026, 12, 31)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
        );
        assert_eq!(
            resolve_expiry(Some("2026-12-31T13:00:00Z"), now),
            chrono::NaiveDate::from_ymd_opt(2026, 12, 31)
                .unwrap()
                .and_hms_opt(18, 30, 0)
                .unwrap(),
            "an offset is converted to IST, not ignored"
        );
        assert_eq!(
            resolve_expiry(Some("2026-12-31T13:00:00"), now),
            chrono::NaiveDate::from_ymd_opt(2026, 12, 31)
                .unwrap()
                .and_hms_opt(13, 0, 0)
                .unwrap()
        );
    }

    #[test]
    fn test_pricing_basis() {
        assert_eq!(pricing_basis(dec!(10), dec!(9), dec!(8)), dec!(10));
        assert_eq!(pricing_basis(dec!(0), dec!(9), dec!(8)), dec!(9));
        assert_eq!(pricing_basis(dec!(0), dec!(0), dec!(8)), dec!(8));
    }
}
