//! Per-position keyed locks (web `sandbox/position_locks.py`).
//!
//! An order path that reads a position and then decides (the CNC sell check,
//! margin netting, a smart order's delta, a close) holds the lock of that
//! position from the read through the order's commit and any immediate fill,
//! so a second order on the same position decides on what the first left
//! behind. Tick-driven fills and GTT firing only *try* the lock and retry on
//! the next tick, so the feed never waits behind an order.
//!
//! The map holds weak references and an entry is removed when its last guard
//! drops, so it is bounded by the positions being worked on right now, never
//! by history.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, Weak};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// `(user_id, exchange, symbol, product)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PositionKey {
    pub user_id: String,
    pub exchange: String,
    pub symbol: String,
    pub product: String,
}

impl PositionKey {
    pub fn new(user_id: &str, exchange: &str, symbol: &str, product: &str) -> Self {
        Self {
            user_id: user_id.to_string(),
            exchange: exchange.to_string(),
            symbol: symbol.to_string(),
            product: product.to_string(),
        }
    }
}

type Slot = AsyncMutex<()>;

#[derive(Default)]
struct Inner {
    map: Mutex<HashMap<PositionKey, Weak<Slot>>>,
}

/// The lock table.
#[derive(Default, Clone)]
pub struct PositionLocks {
    inner: Arc<Inner>,
}

/// Holding this means holding the position's lock.
pub struct PositionGuard {
    guard: Option<OwnedMutexGuard<()>>,
    slot: Option<Arc<Slot>>,
    key: PositionKey,
    inner: Arc<Inner>,
}

impl PositionGuard {
    pub fn key(&self) -> &PositionKey {
        &self.key
    }
}

impl Drop for PositionGuard {
    fn drop(&mut self) {
        // Release the lock first, then forget the slot if nobody else holds
        // or waits on it.
        self.guard.take();
        if let Some(slot) = self.slot.take() {
            let mut map = self.inner.map.lock();
            if Arc::strong_count(&slot) == 1 {
                if let Some(w) = map.get(&self.key) {
                    if w.as_ptr() == Arc::as_ptr(&slot) {
                        map.remove(&self.key);
                    }
                }
            }
        }
    }
}

impl PositionLocks {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&self, key: &PositionKey) -> Arc<Slot> {
        let mut map = self.inner.map.lock();
        if let Some(existing) = map.get(key).and_then(|w| w.upgrade()) {
            return existing;
        }
        let slot = Arc::new(AsyncMutex::new(()));
        map.insert(key.clone(), Arc::downgrade(&slot));
        slot
    }

    /// Wait for the position's lock.
    pub async fn lock(&self, key: PositionKey) -> PositionGuard {
        let slot = self.slot(&key);
        let guard = slot.clone().lock_owned().await;
        PositionGuard {
            guard: Some(guard),
            slot: Some(slot),
            key,
            inner: self.inner.clone(),
        }
    }

    /// Take the lock only if it is free right now.
    pub fn try_lock(&self, key: PositionKey) -> Option<PositionGuard> {
        let slot = self.slot(&key);
        match slot.clone().try_lock_owned() {
            Ok(guard) => Some(PositionGuard {
                guard: Some(guard),
                slot: Some(slot),
                key,
                inner: self.inner.clone(),
            }),
            Err(_) => {
                // Drop our extra reference without leaving a stale entry.
                drop(PositionGuard {
                    guard: None,
                    slot: Some(slot),
                    key,
                    inner: self.inner.clone(),
                });
                None
            }
        }
    }

    /// Entries currently tracked (tests: the table does not grow).
    pub fn len(&self) -> usize {
        self.inner.map.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> PositionKey {
        PositionKey::new("u", "NSE", s, "MIS")
    }

    #[tokio::test]
    async fn a_second_holder_waits_and_the_table_empties_afterwards() {
        let locks = PositionLocks::new();
        let g = locks.lock(k("A")).await;
        assert!(locks.try_lock(k("A")).is_none());
        let other = locks.try_lock(k("B"));
        assert!(other.is_some(), "a different position is not blocked");
        drop(other);
        drop(g);
        assert!(locks.is_empty());
        for _ in 0..100 {
            let g = locks.lock(k("A")).await;
            drop(g);
        }
        assert!(locks.is_empty(), "the lock table must not grow with use");
    }

    #[tokio::test]
    async fn waiters_are_served_in_turn() {
        let locks = PositionLocks::new();
        let g = locks.lock(k("A")).await;
        let l2 = locks.clone();
        let h = tokio::spawn(async move {
            let _g = l2.lock(k("A")).await;
            7
        });
        tokio::task::yield_now().await;
        assert!(!h.is_finished());
        drop(g);
        assert_eq!(h.await.unwrap(), 7);
        assert!(locks.is_empty());
    }
}
