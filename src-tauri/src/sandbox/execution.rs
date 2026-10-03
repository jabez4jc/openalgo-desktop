//! Fill decisions and fills (web `sandbox/execution_engine.py`).
//!
//! [`decide`] is pure: an order and a quote in, a decision out. A fill is one
//! transaction: the order row is claimed with a conditional UPDATE on the
//! pending statuses (so of any number of simultaneous evaluators exactly one
//! fills), the terms the decision was made against are re-checked (a modify
//! that landed in between wins), the trade is written and the position and
//! funds are netted. Events go out after the commit.

use super::config::SandboxConfig;
use super::core::{blocking, Core};
use super::db::{OrderRow, OrderTerms};
use super::events::{fill_events, order_update, Outbox};
use super::funds;
use super::locks::PositionKey;
use super::orders;
use super::positions;
use super::quotes::{quote_looks_stale, Quote};
use super::types::{dec_to_db, Action, OrderStatus, PriceType, SbResult, SymbolKey};
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// What to do with a resting order at a quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Not now.
    Wait,
    /// Fill at this price.
    Fill(Decimal),
    /// An SL whose trigger fired but whose limit is not reachable yet:
    /// `trigger pending` becomes `open`.
    ReleaseToOpen,
}

/// The web's fill rules (`_process_order`, `_process_trigger_pending_order`).
pub fn decide(order: &OrderRow, q: &Quote) -> Decision {
    let ltp = q.ltp;
    if ltp <= Decimal::ZERO || quote_looks_stale(q) {
        return Decision::Wait;
    }
    let buy = order.action == Action::Buy;
    let price = order.price.unwrap_or(Decimal::ZERO);
    let trigger = order.trigger_price.unwrap_or(Decimal::ZERO);
    let triggered = if buy { ltp >= trigger } else { ltp <= trigger };
    let limit_ok = if buy { ltp <= price } else { ltp >= price };

    match order.order_status {
        OrderStatus::TriggerPending => {
            if !triggered {
                return Decision::Wait;
            }
            match order.price_type {
                PriceType::SlM => Decision::Fill(ltp),
                PriceType::Sl if limit_ok => Decision::Fill(ltp),
                PriceType::Sl => Decision::ReleaseToOpen,
                // A MARKET/LIMIT order never rests in trigger pending; treat
                // it like an open one.
                _ => decide_open(order, q, buy, price, trigger),
            }
        }
        OrderStatus::Open => decide_open(order, q, buy, price, trigger),
        _ => Decision::Wait,
    }
}

fn decide_open(
    order: &OrderRow,
    q: &Quote,
    buy: bool,
    price: Decimal,
    trigger: Decimal,
) -> Decision {
    let ltp = q.ltp;
    match order.price_type {
        PriceType::Market => {
            let side = if buy { q.ask } else { q.bid };
            Decision::Fill(if side > Decimal::ZERO { side } else { ltp })
        }
        PriceType::Limit => {
            if (buy && ltp <= price) || (!buy && ltp >= price) {
                // A resting limit fills at its limit price.
                Decision::Fill(price)
            } else {
                Decision::Wait
            }
        }
        PriceType::Sl => {
            let fire = if buy {
                ltp >= trigger && ltp <= price
            } else {
                ltp <= trigger && ltp >= price
            };
            if fire {
                Decision::Fill(ltp)
            } else {
                Decision::Wait
            }
        }
        PriceType::SlM => {
            let fire = if buy { ltp >= trigger } else { ltp <= trigger };
            if fire {
                Decision::Fill(ltp)
            } else {
                Decision::Wait
            }
        }
    }
}

/// `TRADE-YYYYMMDD-HHMMSS-<8 hex>`.
pub fn generate_trade_id(now: chrono::NaiveDateTime) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("TRADE-{}-{}", now.format("%Y%m%d-%H%M%S"), &u[..8])
}

/// Fill inside the caller's transaction. Returns `false` (nothing written)
/// when the order is no longer pending or its terms changed.
pub(crate) fn execute_in_tx(
    tx: &Connection,
    core: &Core,
    order_id: i64,
    price: Decimal,
    expected: Option<OrderTerms>,
    outbox: &mut Outbox,
) -> rusqlite::Result<bool> {
    let Some(order) = orders::get_by_id(tx, order_id)? else {
        return Ok(false);
    };
    if !order.order_status.is_pending() {
        return Ok(false);
    }
    if let Some(t) = expected {
        if order.terms() != t {
            tracing::info!(
                "Order {} was modified while its fill was being decided; deciding again on the next price",
                order.orderid
            );
            return Ok(false);
        }
    }
    let now = core.now();
    let now_s = super::clock::ts(now);
    let claimed = tx.execute(
        "UPDATE sandbox_orders SET order_status = 'complete', average_price = ?1,
           filled_quantity = quantity, pending_quantity = 0, update_timestamp = ?2
         WHERE id = ?3 AND order_status IN ('open','trigger pending')",
        params![dec_to_db(price), now_s, order.id],
    )?;
    if claimed != 1 {
        return Ok(false);
    }
    let mut tradeid = generate_trade_id(now);
    for _ in 0..5 {
        let r = tx.execute(
            "INSERT INTO sandbox_trades (tradeid, orderid, user_id, symbol, exchange, action,
               quantity, price, product, strategy, trade_timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                tradeid,
                order.orderid,
                order.user_id,
                order.symbol,
                order.exchange,
                order.action.as_str(),
                order.quantity,
                dec_to_db(price),
                order.product.as_str(),
                order.strategy,
                now_s
            ],
        );
        match r {
            Ok(_) => break,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                tradeid = generate_trade_id(now);
            }
            Err(e) => return Err(e),
        }
    }
    let cfg = SandboxConfig::load(tx)?;
    let cv = core.contract_value(&order.symbol, &order.exchange);
    positions::apply_fill(tx, &cfg, &order, price, cv, &now_s)?;
    let discrepancy = funds::reconcile(tx, &order.user_id, true, &now_s)?;
    if discrepancy != Decimal::ZERO {
        tracing::warn!(
            "Sandbox margin reconciled by {} after filling {}",
            discrepancy,
            order.orderid
        );
    }
    let mut filled = order.clone();
    filled.average_price = Some(price);
    for e in fill_events(&filled, &tradeid, price) {
        outbox.push(e);
    }
    tracing::info!(
        "Sandbox order {} filled: {} {} {} @ {}",
        order.orderid,
        order.symbol,
        order.action.as_str(),
        order.quantity,
        price
    );
    Ok(true)
}

/// Mark an order rejected after a fill failed for a technical reason, and
/// return its margin. Only from a pending status.
fn reject_after_error(core: &Core, order_id: i64) {
    let mut outbox = Outbox::new();
    let reason = "Execution error: the sandbox could not record this fill";
    let r: SbResult<()> = core.db.with_tx(|tx| {
        let Some(order) = orders::get_by_id(tx, order_id)? else {
            return Ok(());
        };
        let n = tx.execute(
            "UPDATE sandbox_orders SET order_status = 'rejected', rejection_reason = ?1,
               update_timestamp = ?2
             WHERE id = ?3 AND order_status IN ('open','trigger pending')",
            params![reason, core.now_ts(), order.id],
        )?;
        if n == 1 {
            if order.margin_blocked > Decimal::ZERO {
                let cfg = SandboxConfig::load(tx)?;
                let amount = order.margin_blocked;
                let _ = funds::apply(
                    tx,
                    &order.user_id,
                    cfg.starting_capital,
                    &core.now_ts(),
                    |f| funds::release(f, amount.min(f.used_margin), Decimal::ZERO, true),
                )?;
            }
            outbox.push(order_update(&order, OrderStatus::Rejected, reason));
        }
        Ok(())
    });
    match r {
        Ok(()) => core.publish(outbox),
        Err(e) => tracing::error!("Could not mark sandbox order {} rejected: {}", order_id, e),
    }
}

/// Fill an order at `price` (immediate execution and engine fills).
pub(crate) async fn execute(
    core: &Arc<Core>,
    order_id: i64,
    price: Decimal,
    expected: Option<OrderTerms>,
) -> SbResult<bool> {
    blocking(core, move |c| {
        let mut outbox = Outbox::new();
        let r: SbResult<bool> = c.db.with_tx(|tx| {
            Ok(execute_in_tx(
                tx,
                c,
                order_id,
                price,
                expected,
                &mut outbox,
            )?)
        });
        match r {
            Ok(filled) => {
                c.publish(outbox);
                Ok(filled)
            }
            Err(e) => {
                tracing::error!("Sandbox fill of order {} failed: {}", order_id, e);
                reject_after_error(c, order_id);
                Ok(false)
            }
        }
    })
    .await
}

/// Decide and act for one order at one quote. Returns `true` when the order
/// filled.
pub(crate) async fn process(core: &Arc<Core>, order: &OrderRow, q: &Quote) -> SbResult<bool> {
    match decide(order, q) {
        Decision::Wait => Ok(false),
        Decision::Fill(price) => execute(core, order.id, price, Some(order.terms())).await,
        Decision::ReleaseToOpen => {
            let id = order.id;
            blocking(core, move |c| {
                let mut outbox = Outbox::new();
                let r: SbResult<()> = c.db.with_tx(|tx| {
                    let n = tx.execute(
                        "UPDATE sandbox_orders SET order_status = 'open', update_timestamp = ?1
                         WHERE id = ?2 AND order_status = 'trigger pending'",
                        params![c.now_ts(), id],
                    )?;
                    if n == 1 {
                        if let Some(o) = orders::get_by_id(tx, id)? {
                            outbox.push(order_update(&o, OrderStatus::Open, ""));
                        }
                    }
                    Ok(())
                });
                r?;
                c.publish(outbox);
                Ok(false)
            })
            .await
        }
    }
}

/// Process one order under a *tried* position lock: when another order on
/// the same position is in flight the order is left for the next price.
async fn process_try_locked(core: &Arc<Core>, order: &OrderRow, q: &Quote) -> SbResult<bool> {
    if decide(order, q) == Decision::Wait {
        return Ok(false);
    }
    let key = PositionKey::new(
        &order.user_id,
        &order.exchange,
        &order.symbol,
        order.product.as_str(),
    );
    let Some(_guard) = core.locks.try_lock(key) else {
        tracing::debug!(
            "Order {} waits: another order on its position is in flight",
            order.orderid
        );
        return Ok(false);
    };
    process(core, order, q).await
}

/// Ticks: evaluate every pending order on this symbol at `ltp` (quote built
/// as the web's WebSocket engine does: bid = ask = LTP, no day range).
/// Returns how many filled.
pub(crate) async fn on_price(core: &Arc<Core>, key: &SymbolKey, ltp: Decimal) -> SbResult<usize> {
    let k = key.clone();
    let pending = blocking(core, move |c| {
        Ok(c.db.with_conn(|conn| orders::pending(conn, Some(&k)))?)
    })
    .await?;
    let q = Quote::ltp(ltp);
    let mut filled = 0;
    for order in &pending {
        if process_try_locked(core, order, &q).await? {
            filled += 1;
        }
    }
    Ok(filled)
}

/// What one polling pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PollStats {
    pub pending: usize,
    pub symbols: usize,
    pub filled: usize,
    pub gtt_fired: usize,
}

/// The polling engine's pass (web `check_and_execute_pending_orders`): every
/// pending order, quotes fetched in one batch per pass, then GTT legs.
pub(crate) async fn poll_once(core: &Arc<Core>) -> SbResult<PollStats> {
    let pending = blocking(core, |c| {
        Ok(c.db.with_conn(|conn| orders::pending(conn, None))?)
    })
    .await?;
    let mut by_symbol: BTreeMap<SymbolKey, Vec<OrderRow>> = BTreeMap::new();
    for o in pending.iter() {
        by_symbol
            .entry(SymbolKey::new(o.symbol.clone(), o.exchange.clone()))
            .or_default()
            .push(o.clone());
    }
    let mut stats = PollStats {
        pending: pending.len(),
        symbols: by_symbol.len(),
        ..Default::default()
    };
    if !by_symbol.is_empty() {
        let keys: Vec<SymbolKey> = by_symbol.keys().cloned().collect();
        let quotes: HashMap<SymbolKey, Quote> = core.quotes.quotes(&keys).await;
        for (key, orders) in by_symbol {
            let Some(q) = quotes.get(&key) else {
                continue;
            };
            for o in &orders {
                if process_try_locked(core, o, q).await? {
                    stats.filled += 1;
                }
            }
        }
    }
    stats.gtt_fired = super::gtt::poll_active(core).await?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn order(
        action: Action,
        pt: PriceType,
        status: OrderStatus,
        price: Option<Decimal>,
        trig: Option<Decimal>,
    ) -> OrderRow {
        OrderRow {
            id: 1,
            orderid: "1".into(),
            user_id: "u".into(),
            strategy: None,
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action,
            quantity: 1,
            price,
            trigger_price: trig,
            price_type: pt,
            product: super::super::types::Product::Mis,
            order_status: status,
            average_price: None,
            filled_quantity: 0,
            pending_quantity: 1,
            rejection_reason: None,
            margin_blocked: Decimal::ZERO,
            gtt_leg_id: None,
            order_timestamp: String::new(),
            update_timestamp: String::new(),
        }
    }

    #[test]
    fn market_uses_the_far_side_when_present() {
        let q = Quote {
            ltp: dec!(100),
            bid: dec!(99.9),
            ask: dec!(100.1),
            ..Quote::default()
        };
        let b = order(
            Action::Buy,
            PriceType::Market,
            OrderStatus::Open,
            None,
            None,
        );
        let s = order(
            Action::Sell,
            PriceType::Market,
            OrderStatus::Open,
            None,
            None,
        );
        assert_eq!(decide(&b, &q), Decision::Fill(dec!(100.1)));
        assert_eq!(decide(&s, &q), Decision::Fill(dec!(99.9)));
        assert_eq!(
            decide(
                &b,
                &Quote {
                    ltp: dec!(100),
                    ..Quote::default()
                }
            ),
            Decision::Fill(dec!(100))
        );
    }

    #[test]
    fn stale_or_zero_quotes_wait() {
        let b = order(
            Action::Buy,
            PriceType::Market,
            OrderStatus::Open,
            None,
            None,
        );
        assert_eq!(decide(&b, &Quote::default()), Decision::Wait);
        let stale = Quote {
            ltp: dec!(90),
            high: dec!(110),
            low: dec!(100),
            ..Quote::default()
        };
        assert_eq!(decide(&b, &stale), Decision::Wait);
    }
}
