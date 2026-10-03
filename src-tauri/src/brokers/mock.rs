//! Scriptable broker for service and HTTP tests.

use super::types::*;
use super::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use parking_lot::Mutex;

pub struct MockBroker {
    pub id: &'static str,
    pub funds_ok: Mutex<bool>,
    /// Credentials passed to the last `authenticate` call.
    pub last_auth: Mutex<Option<BrokerCredentials>>,
    pub funds_calls: Mutex<u32>,
}

impl MockBroker {
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            funds_ok: Mutex::new(true),
            last_auth: Mutex::new(None),
            funds_calls: Mutex::new(0),
        }
    }

    fn unsupported<T>() -> Result<T> {
        Err(AppError::Broker("not supported by the mock".into()))
    }
}

#[async_trait]
impl Broker for MockBroker {
    fn id(&self) -> &'static str {
        self.id
    }
    fn name(&self) -> &'static str {
        "Mock"
    }
    fn logo(&self) -> &'static str {
        ""
    }
    fn requires_totp(&self) -> bool {
        false
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        let ok = credentials.request_token.is_some() || credentials.totp.is_some();
        *self.last_auth.lock() = Some(credentials);
        if !ok {
            return Err(AppError::Auth("Mock rejected the sign-in".into()));
        }
        Ok(AuthResponse {
            auth_token: "mock-access-token".into(),
            feed_token: Some("mock-feed-token".into()),
            user_id: "AB1234".into(),
            user_name: Some("Mock Trader".into()),
        })
    }

    async fn place_order(&self, _: &str, _: OrderRequest) -> Result<OrderResponse> {
        Self::unsupported()
    }
    async fn modify_order(&self, _: &str, _: &str, _: ModifyOrderRequest) -> Result<OrderResponse> {
        Self::unsupported()
    }
    async fn cancel_order(&self, _: &str, _: &str, _: Option<&str>) -> Result<()> {
        Self::unsupported()
    }
    async fn get_order_book(&self, _: &str) -> Result<Vec<Order>> {
        Ok(vec![])
    }
    async fn get_trade_book(&self, _: &str) -> Result<Vec<Order>> {
        Ok(vec![])
    }
    async fn get_positions(&self, _: &str) -> Result<Vec<Position>> {
        Ok(vec![])
    }
    async fn get_holdings(&self, _: &str) -> Result<Vec<Holding>> {
        Ok(vec![])
    }
    async fn get_funds(&self, auth_token: &str) -> Result<Funds> {
        *self.funds_calls.lock() += 1;
        if !*self.funds_ok.lock() || auth_token != "mock-access-token" {
            return Err(AppError::Broker(
                "Incorrect `api_key` or `access_token`.".into(),
            ));
        }
        Ok(Funds {
            available_cash: 125000.5,
            used_margin: 2500.25,
            total_margin: 127500.75,
            opening_balance: 127500.75,
            payin: 0.0,
            payout: 0.0,
            span: 0.0,
            exposure: 0.0,
            collateral: 1000.0,
        })
    }
    async fn get_quote(&self, _: &str, _: Vec<(String, String)>) -> Result<Vec<Quote>> {
        Self::unsupported()
    }
    async fn get_market_depth(&self, _: &str, _: &str, _: &str) -> Result<MarketDepth> {
        Self::unsupported()
    }
    async fn download_master_contract(&self, _: &str) -> Result<Vec<SymbolData>> {
        Ok(vec![])
    }
}
