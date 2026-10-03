//! Broker adapters module

pub mod angel;
pub mod catalog;
pub mod fyers;
#[cfg(test)]
pub mod mock;
pub mod types;
pub mod zerodha;

use crate::error::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use types::*;

/// Broker trait that all broker implementations must implement
#[async_trait]
pub trait Broker: Send + Sync {
    /// Broker ID (e.g., "angel", "zerodha", "fyers")
    fn id(&self) -> &'static str;

    /// Broker display name
    fn name(&self) -> &'static str;

    /// Broker logo path
    fn logo(&self) -> &'static str;

    /// Whether this broker requires TOTP for login
    fn requires_totp(&self) -> bool;

    /// Authenticate with broker
    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse>;

    /// Place a new order
    async fn place_order(&self, auth_token: &str, order: OrderRequest) -> Result<OrderResponse>;

    /// Modify an existing order
    async fn modify_order(
        &self,
        auth_token: &str,
        order_id: &str,
        order: ModifyOrderRequest,
    ) -> Result<OrderResponse>;

    /// Cancel an order
    async fn cancel_order(
        &self,
        auth_token: &str,
        order_id: &str,
        variety: Option<&str>,
    ) -> Result<()>;

    /// Get order book
    async fn get_order_book(&self, auth_token: &str) -> Result<Vec<Order>>;

    /// Get trade book
    async fn get_trade_book(&self, auth_token: &str) -> Result<Vec<Order>>;

    /// Get positions
    async fn get_positions(&self, auth_token: &str) -> Result<Vec<Position>>;

    /// Get holdings
    async fn get_holdings(&self, auth_token: &str) -> Result<Vec<Holding>>;

    /// Get funds/margin
    async fn get_funds(&self, auth_token: &str) -> Result<Funds>;

    /// Get quote for symbols
    async fn get_quote(
        &self,
        auth_token: &str,
        symbols: Vec<(String, String)>,
    ) -> Result<Vec<Quote>>;

    /// Get market depth
    async fn get_market_depth(
        &self,
        auth_token: &str,
        exchange: &str,
        symbol: &str,
    ) -> Result<MarketDepth>;

    /// Download master contract
    async fn download_master_contract(&self, auth_token: &str) -> Result<Vec<SymbolData>>;
}

/// Broker credentials for authentication. `Debug` is redacted.
#[derive(Clone, Default, serde::Deserialize)]
pub struct BrokerCredentials {
    pub api_key: String,
    pub api_secret: Option<String>,
    pub client_id: Option<String>,
    pub password: Option<String>,
    pub totp: Option<String>,
    pub request_token: Option<String>,
    pub auth_code: Option<String>,
}

impl std::fmt::Debug for BrokerCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerCredentials")
            .field("api_key", &"[REDACTED]")
            .field("client_id", &self.client_id.as_ref().map(|_| "[set]"))
            .finish_non_exhaustive()
    }
}

/// Authentication response from broker
#[derive(Clone)]
pub struct AuthResponse {
    pub auth_token: String,
    pub feed_token: Option<String>,
    pub user_id: String,
    pub user_name: Option<String>,
}

impl std::fmt::Debug for AuthResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthResponse")
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

/// Broker registry for managing multiple brokers
pub struct BrokerRegistry {
    brokers: HashMap<String, Arc<dyn Broker>>,
}

impl BrokerRegistry {
    /// Create new broker registry with all supported brokers
    pub fn new() -> Self {
        let mut brokers: HashMap<String, Arc<dyn Broker>> = HashMap::new();

        // Register brokers
        brokers.insert("angel".to_string(), Arc::new(angel::AngelBroker::new()));
        brokers.insert(
            "zerodha".to_string(),
            Arc::new(zerodha::ZerodhaBroker::new()),
        );
        brokers.insert("fyers".to_string(), Arc::new(fyers::FyersBroker::new()));

        Self { brokers }
    }

    /// Registry with exactly these adapters (tests use a mock broker).
    pub fn with(brokers: Vec<Arc<dyn Broker>>) -> Self {
        Self {
            brokers: brokers
                .into_iter()
                .map(|b| (b.id().to_string(), b))
                .collect(),
        }
    }

    /// IDs of the adapters compiled into this build.
    pub fn ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.brokers.keys().cloned().collect();
        v.sort();
        v
    }

    /// Get broker by ID
    pub fn get(&self, id: &str) -> Option<Arc<dyn Broker>> {
        self.brokers.get(id).cloned()
    }

    /// List all available brokers
    pub fn list(&self) -> Vec<Arc<dyn Broker>> {
        self.brokers.values().cloned().collect()
    }
}

impl Default for BrokerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
