//! Where prices come from. The engine never knows about brokers: it asks a
//! [`QuoteSource`] for snapshots (LTP, bid/ask, day range) and listens to a
//! [`TickSource`] for live LTP ticks. Production adapters wrap the feed cache
//! and the broker quote API; tests use [`StaticQuoteSource`] and
//! [`BroadcastTicks`].

use super::types::{dec_from_f64, SymbolKey};
use parking_lot::{Mutex, RwLock};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::broadcast;

/// A quote snapshot. Zero means "not available" for every field, as in the
/// web's quote dicts.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Quote {
    pub ltp: Decimal,
    pub bid: Decimal,
    pub ask: Decimal,
    pub high: Decimal,
    pub low: Decimal,
}

impl Quote {
    /// LTP only (a tick-built quote: bid and ask equal the LTP, no day range,
    /// exactly as the web's WebSocket engine builds it).
    pub fn ltp(ltp: Decimal) -> Self {
        Self {
            ltp,
            bid: ltp,
            ask: ltp,
            high: Decimal::ZERO,
            low: Decimal::ZERO,
        }
    }

    /// From float fields (adapters).
    pub fn from_f64(ltp: f64, bid: f64, ask: f64, high: f64, low: f64) -> Self {
        Self {
            ltp: dec_from_f64(ltp),
            bid: dec_from_f64(bid),
            ask: dec_from_f64(ask),
            high: dec_from_f64(high),
            low: dec_from_f64(low),
        }
    }
}

/// The stale-quote guard (web issue #1638): a quote whose LTP lies outside
/// its own day `[low, high]` contradicts itself and must not be filled at.
/// Missing or zero high/low means there is nothing to cross-check.
pub fn quote_looks_stale(q: &Quote) -> bool {
    if q.ltp <= Decimal::ZERO || q.high <= Decimal::ZERO || q.low <= Decimal::ZERO {
        return false;
    }
    !(q.low <= q.ltp && q.ltp <= q.high)
}

/// Snapshot quotes.
#[async_trait::async_trait]
pub trait QuoteSource: Send + Sync {
    /// One symbol; `None` when no price is available.
    async fn quote(&self, symbol: &str, exchange: &str) -> Option<Quote>;

    /// Many symbols in one call where the source supports it (multiquotes).
    /// Missing symbols are simply absent from the map.
    async fn quotes(&self, keys: &[SymbolKey]) -> HashMap<SymbolKey, Quote> {
        let mut out = HashMap::new();
        for k in keys {
            if let Some(q) = self.quote(&k.symbol, &k.exchange).await {
                out.insert(k.clone(), q);
            }
        }
        out
    }
}

/// A settable quote table (tests, fixtures, offline use).
#[derive(Default)]
pub struct StaticQuoteSource {
    map: RwLock<HashMap<SymbolKey, Quote>>,
    calls: AtomicUsize,
}

impl StaticQuoteSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set an LTP-only quote (bid/ask zero, no day range).
    pub fn set_ltp(&self, symbol: &str, exchange: &str, ltp: Decimal) {
        self.set(
            symbol,
            exchange,
            Quote {
                ltp,
                ..Quote::default()
            },
        );
    }

    pub fn set(&self, symbol: &str, exchange: &str, q: Quote) {
        self.map.write().insert(SymbolKey::new(symbol, exchange), q);
    }

    pub fn remove(&self, symbol: &str, exchange: &str) {
        self.map.write().remove(&SymbolKey::new(symbol, exchange));
    }

    /// How many snapshot requests were made (single or batch).
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl QuoteSource for StaticQuoteSource {
    async fn quote(&self, symbol: &str, exchange: &str) -> Option<Quote> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.map
            .read()
            .get(&SymbolKey::new(symbol, exchange))
            .copied()
            .filter(|q| q.ltp > Decimal::ZERO)
    }

    async fn quotes(&self, keys: &[SymbolKey]) -> HashMap<SymbolKey, Quote> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let map = self.map.read();
        keys.iter()
            .filter_map(|k| {
                map.get(k)
                    .filter(|q| q.ltp > Decimal::ZERO)
                    .map(|q| (k.clone(), *q))
            })
            .collect()
    }
}

/// A live LTP tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub symbol: String,
    pub exchange: String,
    pub ltp: Decimal,
}

impl Tick {
    pub fn new(symbol: &str, exchange: &str, ltp: Decimal) -> Self {
        Self {
            symbol: symbol.to_string(),
            exchange: exchange.to_string(),
            ltp,
        }
    }

    pub fn key(&self) -> SymbolKey {
        SymbolKey::new(self.symbol.clone(), self.exchange.clone())
    }
}

/// Live ticks. The engine owns one receiver and asks the source to stream
/// exactly the symbols it needs (pending orders, GTT legs, open positions).
pub trait TickSource: Send + Sync {
    /// A new receiver of every tick the source streams. Bounded: a slow
    /// receiver sees `Lagged`, which the engine answers with a poll.
    fn subscribe_ticks(&self) -> broadcast::Receiver<Tick>;
    /// Start streaming a symbol (LTP mode). Idempotent.
    fn watch(&self, key: &SymbolKey);
    /// Stop streaming a symbol. Idempotent.
    fn unwatch(&self, key: &SymbolKey);
}

/// A broadcast-channel tick source: production adapters push parsed feed
/// ticks into it, tests push synthetic ones.
pub struct BroadcastTicks {
    tx: broadcast::Sender<Tick>,
    watched: Mutex<HashSet<SymbolKey>>,
}

impl BroadcastTicks {
    /// `capacity` bounds the per-receiver backlog.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        Self {
            tx,
            watched: Mutex::new(HashSet::new()),
        }
    }

    /// Deliver a tick to every receiver. Returns how many received it.
    pub fn send(&self, tick: Tick) -> usize {
        self.tx.send(tick).unwrap_or(0)
    }

    /// Symbols currently being watched (tests: subscription bookkeeping).
    pub fn watched(&self) -> HashSet<SymbolKey> {
        self.watched.lock().clone()
    }
}

impl TickSource for BroadcastTicks {
    fn subscribe_ticks(&self) -> broadcast::Receiver<Tick> {
        self.tx.subscribe()
    }

    fn watch(&self, key: &SymbolKey) {
        self.watched.lock().insert(key.clone());
    }

    fn unwatch(&self, key: &SymbolKey) {
        self.watched.lock().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn q(ltp: Decimal, high: Decimal, low: Decimal) -> Quote {
        Quote {
            ltp,
            high,
            low,
            ..Quote::default()
        }
    }

    // Port of test/sandbox/test_stale_quote_guard.py
    #[test]
    fn test_the_reported_fill_is_deferred() {
        assert!(quote_looks_stale(&q(dec!(1047.60), dec!(1345), dec!(1262))));
    }

    #[test]
    fn test_a_coherent_quote_still_fills() {
        assert!(!quote_looks_stale(&q(dec!(1296), dec!(1345), dec!(1262))));
    }

    #[test]
    fn test_either_side_of_the_range_counts() {
        assert!(quote_looks_stale(&q(dec!(900), dec!(1345), dec!(1262))));
        assert!(quote_looks_stale(&q(dec!(1400), dec!(1345), dec!(1262))));
    }

    #[test]
    fn test_the_range_is_inclusive() {
        assert!(!quote_looks_stale(&q(dec!(1262), dec!(1345), dec!(1262))));
        assert!(!quote_looks_stale(&q(dec!(1345), dec!(1345), dec!(1262))));
    }

    #[test]
    fn test_a_symbol_that_has_not_traded_today_is_not_caught() {
        assert!(!quote_looks_stale(&q(dec!(1047.60), dec!(0), dec!(0))));
    }

    #[test]
    fn test_a_tick_built_quote_without_ohlc_still_fills() {
        assert!(!quote_looks_stale(&Quote::ltp(dec!(1296))));
    }

    #[test]
    fn test_a_malformed_quote_never_blocks_a_fill() {
        assert!(!quote_looks_stale(&Quote::default()));
        assert!(!quote_looks_stale(&q(dec!(-5), dec!(1), dec!(0))));
        assert!(!quote_looks_stale(&Quote::from_f64(
            f64::NAN,
            1.0,
            1.0,
            1.0,
            0.0
        )));
    }
}
