//! Adapter between the feed server and the broker streaming layer.
//!
//! The server talks only to [`MarketDataSource`]. This bridge implements it
//! for the running app:
//! * symbols resolve against the loaded master contract (`AppState` symbol
//!   cache), so an unknown symbol gets the web's "Token not found" refusal;
//! * `subscribe` / `unsubscribe` record the desired set and wake whoever
//!   drives the broker connection; [`BrokerBridge::desired`] is the complete
//!   set the broker should be streaming at any moment (already reference
//!   counted across feed clients), so the driver can reconcile after a
//!   reconnect without replaying history;
//! * [`BrokerBridge::publish`] is where the broker side pushes normalized
//!   updates (`brokers::common::streaming::{NormalizedTick, NormalizedDepth}`
//!   converted to [`MarketUpdate`]).

use super::source::{InstrumentKey, MarketDataSource, MarketUpdate, Mode, DEFAULT_DEPTH};
use crate::state::AppState;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{broadcast, Notify};

/// Capacity of the update channel between the broker side and the server.
pub const BRIDGE_UPDATE_CAP: usize = 8192;

/// One entry of the desired broker subscription set.
pub type DesiredKey = (InstrumentKey, Mode, u8);

/// Depth levels per exchange for the connected broker.
pub type DepthCapability = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

pub struct BrokerBridge {
    ctx: Arc<AppState>,
    tx: broadcast::Sender<Arc<MarketUpdate>>,
    desired: Mutex<HashSet<DesiredKey>>,
    changed: Notify,
    depths: Mutex<DepthCapability>,
}

impl BrokerBridge {
    pub fn new(ctx: Arc<AppState>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(BRIDGE_UPDATE_CAP);
        Arc::new(Self {
            ctx,
            tx,
            desired: Mutex::new(HashSet::new()),
            changed: Notify::new(),
            depths: Mutex::new(Arc::new(|_| vec![DEFAULT_DEPTH])),
        })
    }

    /// Install the connected broker's depth capability (called on broker
    /// login; the default is 5 levels everywhere, as on the web).
    pub fn set_depth_capability(&self, f: DepthCapability) {
        *self.depths.lock() = f;
    }

    /// Push a normalized update from the broker side to feed clients.
    /// Returns the number of running feed servers that received it.
    pub fn publish(&self, update: MarketUpdate) -> usize {
        self.tx.send(Arc::new(update)).unwrap_or(0)
    }

    /// What the broker connection should be streaming right now.
    pub fn desired(&self) -> Vec<DesiredKey> {
        self.desired.lock().iter().cloned().collect()
    }

    /// Resolves after the desired set changes.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    /// Drop the desired set (broker logout or feed restart).
    pub fn clear(&self) {
        self.desired.lock().clear();
        self.changed.notify_one();
    }
}

impl MarketDataSource for BrokerBridge {
    fn subscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) {
        if self.desired.lock().insert((key.clone(), mode, depth)) {
            self.changed.notify_one();
        }
    }

    fn unsubscribe(&self, key: &InstrumentKey, mode: Mode, depth: u8) {
        if self.desired.lock().remove(&(key.clone(), mode, depth)) {
            self.changed.notify_one();
        }
    }

    fn updates(&self) -> broadcast::Receiver<Arc<MarketUpdate>> {
        self.tx.subscribe()
    }

    fn resolve(&self, key: &InstrumentKey) -> bool {
        self.ctx.symbol_exists(&key.exchange, &key.symbol)
    }

    fn supported_depths(&self, exchange: &str) -> Vec<u8> {
        let f = self.depths.lock().clone();
        f(exchange)
    }
}
