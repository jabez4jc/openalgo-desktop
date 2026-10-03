//! Holdings and T+1 settlement (web `sandbox/holdings_manager.py`).
//!
//! At 00:00 IST (and on catch-up) every CNC position created before today
//! moves into holdings: a long folds into the holding at a weighted average
//! and its margin becomes the holding; a short (a sale from holdings) reduces
//! the holding and its proceeds are credited to available cash. The holding,
//! the funds and the deleted positions commit together.

use super::clock::ts;
use super::config::SandboxConfig;
use super::core::{blocking, Core};
use super::db::{HoldingRow, PositionRow};
use super::events::{t1_settlement, Outbox};
use super::funds;
use super::positions;
use super::replies::{
    HoldingRowOut, HoldingsData, HoldingsReply, HoldingsStatistics, ANALYZE, SUCCESS,
};
use super::types::{
    dec_to_db, float, money, pct, SandboxError, SbResult, SymbolKey, AVG_DP, PCT_DP,
};
use chrono::NaiveDateTime;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;

pub fn get(
    conn: &Connection,
    user: &str,
    symbol: &str,
    exchange: &str,
) -> rusqlite::Result<Option<HoldingRow>> {
    conn.query_row(
        &format!(
            "SELECT {} FROM sandbox_holdings WHERE user_id = ?1 AND symbol = ?2 AND exchange = ?3",
            HoldingRow::COLUMNS
        ),
        params![user, symbol, exchange],
        HoldingRow::from_row,
    )
    .optional()
}

pub fn list_user(conn: &Connection, user: &str) -> rusqlite::Result<Vec<HoldingRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {} FROM sandbox_holdings WHERE user_id = ?1 AND quantity != 0 ORDER BY id",
        HoldingRow::COLUMNS
    ))?;
    let rows = stmt.query_map(params![user], HoldingRow::from_row)?;
    rows.collect()
}

/// Insert a holding (tests seed holdings directly, like the web suite).
pub fn insert(conn: &Connection, h: &HoldingRow) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO sandbox_holdings (user_id, symbol, exchange, quantity, average_price, ltp, pnl,
           pnl_percent, settlement_date, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            h.user_id,
            h.symbol,
            h.exchange,
            h.quantity,
            dec_to_db(h.average_price),
            h.ltp.map(dec_to_db),
            dec_to_db(h.pnl),
            dec_to_db(h.pnl_percent),
            h.settlement_date,
            h.created_at,
            h.updated_at
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn save(conn: &Connection, h: &HoldingRow) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE sandbox_holdings SET quantity = ?1, average_price = ?2, ltp = ?3, pnl = ?4,
           pnl_percent = ?5, updated_at = ?6 WHERE id = ?7",
        params![
            h.quantity,
            dec_to_db(h.average_price),
            h.ltp.map(dec_to_db),
            dec_to_db(h.pnl),
            dec_to_db(h.pnl_percent),
            h.updated_at,
            h.id
        ],
    )?;
    Ok(())
}

fn holding_pnl(h: &HoldingRow, ltp: Decimal) -> (Decimal, Decimal) {
    let pnl = (ltp - h.average_price) * Decimal::from(h.quantity.abs());
    let pc = if h.average_price > Decimal::ZERO {
        ((ltp - h.average_price) / h.average_price * Decimal::ONE_HUNDRED).round_dp(PCT_DP)
    } else {
        Decimal::ZERO
    };
    (pnl, pc)
}

/// The holdings book, priced with one batch quote call (web `get_holdings`).
pub(crate) async fn holdings(core: &Arc<Core>, update_mtm: bool) -> SbResult<HoldingsReply> {
    let rows = blocking(core, |c| {
        Ok(c.db.with_conn(|conn| list_user(conn, c.user()))?)
    })
    .await?;
    let rows = if update_mtm && !rows.is_empty() {
        let mut keys: Vec<SymbolKey> = rows
            .iter()
            .map(|h| SymbolKey::new(h.symbol.clone(), h.exchange.clone()))
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
        blocking(core, move |c| {
            c.db.with_tx(|tx| {
                let mut out = Vec::new();
                for h in list_user(tx, c.user())? {
                    let mut h = h;
                    if let Some(ltp) = quotes
                        .get(&SymbolKey::new(h.symbol.clone(), h.exchange.clone()))
                        .filter(|l| **l > Decimal::ZERO)
                    {
                        let (pnl, pc) = holding_pnl(&h, *ltp);
                        h.ltp = Some(*ltp);
                        h.pnl = pnl;
                        h.pnl_percent = pc;
                        tx.execute(
                            "UPDATE sandbox_holdings SET ltp = ?1, pnl = ?2, pnl_percent = ?3 WHERE id = ?4",
                            params![dec_to_db(*ltp), dec_to_db(pnl), dec_to_db(pc), h.id],
                        )?;
                    }
                    out.push(h);
                }
                Ok::<_, SandboxError>(out)
            })
        })
        .await?
    } else {
        rows
    };
    Ok(holdings_reply(&rows))
}

pub fn holdings_reply(rows: &[HoldingRow]) -> HoldingsReply {
    let mut total_pnl = Decimal::ZERO;
    let mut total_value = Decimal::ZERO;
    let mut total_inv = Decimal::ZERO;
    let mut data = Vec::with_capacity(rows.len());
    for h in rows {
        let qty = Decimal::from(h.quantity.abs());
        let current = h.ltp.map(|l| qty * l).unwrap_or(Decimal::ZERO);
        total_pnl += h.pnl;
        total_value += current;
        total_inv += qty * h.average_price;
        data.push(HoldingRowOut {
            symbol: h.symbol.clone(),
            exchange: h.exchange.clone(),
            product: "CNC".to_string(),
            quantity: h.quantity,
            average_price: money(h.average_price),
            ltp: money(h.ltp.unwrap_or(Decimal::ZERO)),
            pnl: money(h.pnl),
            pnlpercent: pct(h.pnl_percent),
            current_value: money(current),
            settlement_date: h.settlement_date.clone(),
        });
    }
    let pc = if total_inv > Decimal::ZERO {
        total_pnl / total_inv * Decimal::ONE_HUNDRED
    } else {
        Decimal::ZERO
    };
    HoldingsReply {
        status: SUCCESS,
        data: HoldingsData {
            holdings: data,
            statistics: HoldingsStatistics {
                totalholdingvalue: float(total_value.round_dp(2)),
                totalinvvalue: float(total_inv.round_dp(2)),
                totalprofitandloss: float(total_pnl.round_dp(2)),
                totalpnlpercentage: float(pc.round_dp(PCT_DP)),
            },
        },
        mode: ANALYZE,
    }
}

/// Settle one CNC position into holdings, in the caller's transaction.
fn settle_one(
    conn: &Connection,
    cfg: &SandboxConfig,
    p: &PositionRow,
    today: &str,
    now: &str,
) -> rusqlite::Result<bool> {
    if !positions::claim_for_settlement(conn, p.id, p.quantity)? {
        return Ok(false);
    }
    let Some(p) = positions::get_by_id(conn, p.id)? else {
        return Ok(false);
    };
    if p.quantity == 0 {
        conn.execute("DELETE FROM sandbox_positions WHERE id = ?1", params![p.id])?;
        return Ok(true);
    }
    let qty = Decimal::from(p.quantity.abs());
    let cost = qty * p.average_price;
    let capital = cfg.starting_capital;
    match get(conn, &p.user_id, &p.symbol, &p.exchange)? {
        Some(mut h) => {
            if p.quantity > 0 {
                let total = Decimal::from(h.quantity.abs() + p.quantity.abs());
                let value = Decimal::from(h.quantity.abs()) * h.average_price + cost;
                h.quantity += p.quantity;
                if total > Decimal::ZERO {
                    h.average_price = (value / total).round_dp(AVG_DP);
                }
                let margin = p.margin_blocked;
                let r = funds::apply(conn, &p.user_id, capital, now, |f| {
                    funds::transfer_to_holdings(f, margin.min(f.used_margin), cost)
                })?;
                if let Err(r) = r {
                    tracing::error!("T+1 transfer for {} refused: {}", p.symbol, r.0);
                }
            } else {
                h.quantity += p.quantity;
                let margin = p.margin_blocked;
                let r = funds::apply(conn, &p.user_id, capital, now, |f| {
                    let f = funds::release(f, margin.min(f.used_margin), Decimal::ZERO, false)?;
                    funds::credit(&f, cost)
                })?;
                if let Err(r) = r {
                    tracing::error!("T+1 sale credit for {} refused: {}", p.symbol, r.0);
                }
            }
            h.ltp = p.ltp.or(h.ltp);
            h.updated_at = now.to_string();
            if h.quantity == 0 {
                conn.execute("DELETE FROM sandbox_holdings WHERE id = ?1", params![h.id])?;
            } else {
                save(conn, &h)?;
            }
        }
        None if p.quantity > 0 => {
            insert(
                conn,
                &HoldingRow {
                    id: 0,
                    user_id: p.user_id.clone(),
                    symbol: p.symbol.clone(),
                    exchange: p.exchange.clone(),
                    quantity: p.quantity,
                    average_price: p.average_price,
                    ltp: Some(p.ltp.unwrap_or(p.average_price)),
                    pnl: Decimal::ZERO,
                    pnl_percent: Decimal::ZERO,
                    settlement_date: today.to_string(),
                    created_at: now.to_string(),
                    updated_at: now.to_string(),
                },
            )?;
            let margin = p.margin_blocked;
            let r = funds::apply(conn, &p.user_id, capital, now, |f| {
                funds::transfer_to_holdings(f, margin.min(f.used_margin), cost)
            })?;
            if let Err(r) = r {
                tracing::error!("T+1 transfer for {} refused: {}", p.symbol, r.0);
            }
        }
        None => {
            // A CNC sale with no holding left to reduce (the account was
            // reset in between): credit the proceeds, keep no negative
            // holding.
            tracing::warn!(
                "T+1: {} sold {} shares with no holding on record; crediting the proceeds only",
                p.symbol,
                p.quantity.abs()
            );
            let margin = p.margin_blocked;
            let _ = funds::apply(conn, &p.user_id, capital, now, |f| {
                let f = funds::release(f, margin.min(f.used_margin), Decimal::ZERO, false)?;
                funds::credit(&f, cost)
            })?;
        }
    }
    conn.execute("DELETE FROM sandbox_positions WHERE id = ?1", params![p.id])?;
    Ok(true)
}

/// Run T+1 settlement for every CNC position created before `now`'s date
/// (all users). One run at a time; returns the number of positions settled.
pub(crate) async fn process_t1(core: &Arc<Core>) -> SbResult<usize> {
    let _one = core.t1_lock.lock().await;
    blocking(core, |c| {
        let now = c.now();
        let mut outbox = Outbox::new();
        let n =
            c.db.with_tx(|tx| process_t1_in_tx(tx, c, now, &mut outbox))?;
        c.publish(outbox);
        Ok(n)
    })
    .await
}

pub(crate) fn process_t1_in_tx(
    tx: &Connection,
    _core: &Core,
    now: NaiveDateTime,
    outbox: &mut Outbox,
) -> SbResult<usize> {
    let cfg = SandboxConfig::load(tx)?;
    let cutoff = ts(now.date().and_hms_opt(0, 0, 0).unwrap_or(now));
    let today = now.date().format("%Y-%m-%d").to_string();
    let now_s = ts(now);
    let candidates: Vec<PositionRow> = {
        let mut stmt = tx.prepare_cached(&format!(
            "SELECT {} FROM sandbox_positions WHERE product = 'CNC' AND created_at < ?1 ORDER BY id",
            PositionRow::COLUMNS
        ))?;
        let rows = stmt.query_map(params![cutoff], PositionRow::from_row)?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut settled = 0usize;
    let mut users = std::collections::BTreeSet::new();
    for p in &candidates {
        if settle_one(tx, &cfg, p, &today, &now_s)? && p.quantity != 0 {
            settled += 1;
            users.insert(p.user_id.clone());
        }
    }
    if settled > 0 {
        outbox.push(t1_settlement(users.len(), settled));
        tracing::info!(
            "T+1 settlement moved {} sandbox positions to holdings",
            settled
        );
    }
    Ok(settled)
}

/// Positions waiting for T+1 (catch-up check).
pub fn pending_t1(conn: &Connection, now: NaiveDateTime) -> rusqlite::Result<i64> {
    let cutoff = ts(now.date().and_hms_opt(0, 0, 0).unwrap_or(now));
    conn.query_row(
        "SELECT COUNT(*) FROM sandbox_positions WHERE product = 'CNC' AND created_at < ?1",
        params![cutoff],
        |r| r.get(0),
    )
}
