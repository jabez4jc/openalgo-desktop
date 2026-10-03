//! Order-update stream: the `order.update` bus topic relayed to feed clients
//! that sent `subscribe_orders` (web `subscribers/wsproxy_subscriber.py`).
//!
//! The bus has no unsubscribe, so the relay is registered once per bus and
//! outlives feed-server restarts; each running server takes its own
//! receiver from the relay's bounded broadcast channel.

use crate::events::{Event, EventBus, Lane, OrderUpdate, Subscriber, Topic};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Capacity of the relay channel; a lagging server logs and skips.
pub const ORDER_RELAY_CAP: usize = 1024;

pub struct OrderRelay {
    tx: broadcast::Sender<Arc<OrderUpdate>>,
}

impl OrderRelay {
    pub fn new() -> Arc<Self> {
        let (tx, _) = broadcast::channel(ORDER_RELAY_CAP);
        Arc::new(Self { tx })
    }

    /// Create a relay and register it on `bus` (call once per bus).
    pub fn register(bus: &EventBus) -> Arc<Self> {
        let relay = Self::new();
        bus.subscribe(relay.clone(), Lane::BestEffort);
        relay
    }

    pub fn receiver(&self) -> broadcast::Receiver<Arc<OrderUpdate>> {
        self.tx.subscribe()
    }

    /// Push an update directly (what the bus worker does).
    pub fn send(&self, u: OrderUpdate) -> usize {
        self.tx.send(Arc::new(u)).unwrap_or(0)
    }
}

#[async_trait::async_trait]
impl Subscriber for OrderRelay {
    fn name(&self) -> &'static str {
        "feed_order_updates"
    }

    fn topics(&self) -> Vec<Topic> {
        vec![Topic::OrderUpdate]
    }

    async fn handle(&self, event: Arc<Event>) {
        if let Event::OrderUpdate(u) = &*event {
            // No receivers simply means no feed server is running.
            let _ = self.tx.send(Arc::new(u.clone()));
        }
    }
}

/// The 18-key `order_update` frame, in the web's key order.
#[derive(Serialize)]
struct OrderUpdateFrame<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    user_id: &'a str,
    mode: &'a str,
    broker: &'a str,
    orderid: &'a str,
    symbol: &'a str,
    exchange: &'a str,
    action: &'a str,
    quantity: i64,
    price: f64,
    trigger_price: f64,
    pricetype: &'a str,
    product: &'a str,
    order_status: &'a str,
    filled_quantity: i64,
    pending_quantity: i64,
    average_price: f64,
    rejection_reason: &'a str,
}

pub fn order_update_frame(u: &OrderUpdate, user_id: &str) -> String {
    super::protocol::to_json(&OrderUpdateFrame {
        kind: "order_update",
        user_id,
        mode: &u.mode,
        broker: &u.broker,
        orderid: &u.orderid,
        symbol: &u.symbol,
        exchange: &u.exchange,
        action: &u.action,
        quantity: u.quantity,
        price: u.price,
        trigger_price: u.trigger_price,
        pricetype: &u.pricetype,
        product: &u.product,
        order_status: &u.order_status,
        filled_quantity: u.filled_quantity,
        pending_quantity: u.pending_quantity,
        average_price: u.average_price,
        rejection_reason: &u.rejection_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_has_the_eighteen_web_keys() {
        let v: serde_json::Value =
            serde_json::from_str(&order_update_frame(&OrderUpdate::default(), "u")).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        let mut keys = keys;
        keys.sort_unstable();
        let mut want = vec![
            "type",
            "user_id",
            "mode",
            "broker",
            "orderid",
            "symbol",
            "exchange",
            "action",
            "quantity",
            "price",
            "trigger_price",
            "pricetype",
            "product",
            "order_status",
            "filled_quantity",
            "pending_quantity",
            "average_price",
            "rejection_reason",
        ];
        want.sort_unstable();
        assert_eq!(keys, want);
    }
}
