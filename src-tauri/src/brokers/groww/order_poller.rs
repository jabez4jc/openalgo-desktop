//! Order updates by polling the order book (web
//! `websocket_proxy/order_adapter.py` `PollingOrderUpdateAdapter`; Groww
//! has no order socket).
//!
//! One owned task polls every `interval` (clamped to 1..=60 s, web default
//! 5 s). The first poll seeds the snapshot silently; later polls publish an
//! `OrderUpdate` for every order whose `(status, filled quantity)` changed
//! or that is new. The snapshot is rebuilt from each book, so it is bounded
//! by the current order book. Updates go into a bounded channel; when the
//! receiver is dropped the task ends. `OrderPoller::stop` and `Drop` abort
//! the task.

use super::{orders, GrowwCore};
use crate::brokers::common::streaming::OrderUpdate;
use crate::brokers::types::{AuthToken, Order};
use crate::error::{AppError, Result};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);
pub const MAX_INTERVAL: Duration = Duration::from_secs(60);
/// Updates buffered for a slow consumer before the poller waits.
pub const CHANNEL_CAPACITY: usize = 256;

pub fn clamp_interval(d: Duration) -> Duration {
    d.clamp(MIN_INTERVAL, MAX_INTERVAL)
}

/// Normalised order update from an order-book row.
pub fn to_update(o: &Order) -> OrderUpdate {
    OrderUpdate {
        orderid: o.order_id.clone(),
        symbol: o.symbol.clone(),
        exchange: o.exchange.clone(),
        action: o.side.clone(),
        quantity: i64::from(o.quantity),
        price: o.price,
        trigger_price: o.trigger_price,
        pricetype: o.order_type.clone(),
        product: o.product.clone(),
        order_status: o.status.clone(),
        filled_quantity: i64::from(o.filled_quantity),
        pending_quantity: i64::from(o.pending_quantity),
        average_price: o.average_price,
        rejection_reason: o.rejection_reason.clone().unwrap_or_default(),
    }
}

type Snapshot = HashMap<String, (String, i32)>;

/// Changes between two polls; returns the new snapshot.
pub fn diff(previous: Option<&Snapshot>, book: &[Order]) -> (Snapshot, Vec<OrderUpdate>) {
    let mut next = Snapshot::with_capacity(book.len());
    let mut changed = Vec::new();
    for o in book {
        let state = (o.status.clone(), o.filled_quantity);
        if let Some(prev) = previous {
            if prev.get(&o.order_id) != Some(&state) {
                changed.push(to_update(o));
            }
        }
        next.insert(o.order_id.clone(), state);
    }
    (next, changed)
}

pub struct OrderPoller {
    task: JoinHandle<()>,
}

impl OrderPoller {
    pub(crate) fn start(
        core: GrowwCore,
        auth: AuthToken,
        interval: Duration,
    ) -> Result<(Self, mpsc::Receiver<OrderUpdate>)> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| AppError::Internal("Order updates need the async runtime".into()))?;
        let interval = clamp_interval(interval);
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let task = handle.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut snapshot: Option<Snapshot> = None;
            loop {
                ticker.tick().await;
                if tx.is_closed() {
                    break;
                }
                let book = match orders::get_order_book(&core, &auth).await {
                    Ok(b) => b,
                    Err(AppError::Auth(_)) => {
                        tracing::warn!("Groww order updates stopped: the session has expired");
                        break;
                    }
                    Err(e) => {
                        tracing::debug!("Groww order poll failed: {}", e.code());
                        continue;
                    }
                };
                let (next, changed) = diff(snapshot.as_ref(), &book);
                snapshot = Some(next);
                for u in changed {
                    if tx.send(u).await.is_err() {
                        return;
                    }
                }
            }
        });
        Ok((Self { task }, rx))
    }

    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    pub fn stop(self) {
        // Drop aborts.
    }
}

impl Drop for OrderPoller {
    fn drop(&mut self) {
        self.task.abort();
    }
}
