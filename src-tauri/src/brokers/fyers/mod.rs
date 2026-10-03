//! Fyers API v3 adapter (web `broker/fyers/**`).
//!
//! The session token is `app_id:access_token`, sent verbatim in
//! `Authorization`.
//!
//! next wave: history (`/data/history` with resolution map and chunking),
//! margin (`multiorder/margin`), exit-all via `DELETE /positions`, the HSM
//! streaming feed (hsm_key from the JWT, `sf|`/`if|`/`dp|` topics), quote
//! batching beyond 50 symbols, OI on quotes and BSE index renames are not
//! ported yet; those calls report `Unsupported` rather than answering with
//! partial data.

#![allow(non_snake_case)]

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::http;
use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::master_contract::format_expiry;
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::brokers::{AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const API_URL: &str = "https://api-t1.fyers.in/api/v3";
const DATA_URL: &str = "https://api-t1.fyers.in/data";

const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Cds,
    Exchange::Mcx,
    Exchange::NseIndex,
    Exchange::BseIndex,
];

/// web `BrokerData.timeframe_map` (history itself is next wave).
const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("5s", "5S"),
    ("10s", "10S"),
    ("15s", "15S"),
    ("30s", "30S"),
    ("45s", "45S"),
    ("1m", "1"),
    ("2m", "2"),
    ("3m", "3"),
    ("5m", "5"),
    ("10m", "10"),
    ("15m", "15"),
    ("20m", "20"),
    ("30m", "30"),
    ("1h", "60"),
    ("2h", "120"),
    ("4h", "240"),
    ("D", "1D"),
];

pub struct FyersBroker {
    http: reqwest::Client,
    symbols: SymbolResolver,
}

fn session_expired() -> AppError {
    AppError::Auth("Your Fyers session has expired. Log in to Fyers again.".into())
}

/// `sha256("app_id:app_secret")` hex.
fn app_id_hash(api_key: &str, api_secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("{}:{}", api_key, api_secret).as_bytes());
    hex::encode(h.finalize())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Envelope {
    #[serde(deserialize_with = "string_lenient")]
    s: String,
    #[serde(deserialize_with = "i64_lenient")]
    code: i64,
    #[serde(deserialize_with = "string_lenient")]
    message: String,
    #[serde(deserialize_with = "string_lenient")]
    id: String,
    #[serde(flatten)]
    rest: HashMap<String, Value>,
}

impl FyersBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self {
            http: http::client(),
            symbols,
        }
    }

    async fn call(
        &self,
        method: Method,
        url: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<Envelope> {
        if auth.pair().is_none() {
            return Err(session_expired());
        }
        let mut req = self
            .http
            .request(method, url)
            .header("Authorization", auth.raw())
            .header("Content-Type", "application/json");
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await?;
        let (status, env): (_, Envelope) = http::read_json("fyers", resp).await?;
        if env.s != "ok" {
            tracing::warn!(
                status = status.as_u16(),
                code = env.code,
                "Fyers refused the request: {}",
                env.message
            );
            if env.code == -8 || env.code == -15 || env.code == -16 || status.as_u16() == 401 {
                return Err(session_expired());
            }
            return Err(AppError::Broker(if env.message.is_empty() {
                "Fyers refused the request.".into()
            } else {
                env.message
            }));
        }
        Ok(env)
    }

    fn rows<T: serde::de::DeserializeOwned + Default>(env: &mut Envelope, key: &str) -> Vec<T> {
        env.rest
            .remove(key)
            .and_then(|v| serde_json::from_value::<Option<Vec<T>>>(v).ok().flatten())
            .unwrap_or_default()
    }

    fn lookup(&self, key: &QuoteKey) -> Result<SymToken> {
        self.symbols.by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })
    }

    /// Fyers `NSE:SBIN-EQ` -> OpenAlgo symbol on the OpenAlgo exchange.
    fn oa_symbol(&self, fyers_symbol: &str, exchange: &str) -> String {
        self.symbols
            .oa_symbol(fyers_symbol, exchange)
            .unwrap_or_else(|| {
                tracing::debug!("No OpenAlgo symbol for {}:{}", exchange, fyers_symbol);
                fyers_symbol
                    .split_once(':')
                    .map(|(_, s)| s.to_string())
                    .unwrap_or_else(|| fyers_symbol.to_string())
            })
    }

    async fn depth_raw(&self, auth: &AuthToken, key: &QuoteKey) -> Result<(SymToken, FyersDepth)> {
        let row = self.lookup(key)?;
        let url = format!(
            "{}/depth?symbol={}&ohlcv_flag=1",
            DATA_URL,
            urlencoding::encode(row.br_symbol())
        );
        let mut env = self.call(Method::GET, &url, auth, None).await?;
        let d: HashMap<String, FyersDepth> = env
            .rest
            .remove("d")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let depth = d.get(row.br_symbol()).cloned().ok_or_else(|| {
            AppError::Broker(format!(
                "Fyers returned no market data for {} {}.",
                key.exchange, key.symbol
            ))
        })?;
        Ok((row, depth))
    }
}

// web mapping: exchange/segment codes, statuses, types, products
fn exchange_name(exchange: i64, segment: i64) -> &'static str {
    match (exchange, segment) {
        (10, 10) => "NSE",
        (10, 11) => "NFO",
        (10, 12) => "CDS",
        (12, 10) => "BSE",
        (12, 11) => "BFO",
        (11, 20) => "MCX",
        _ => "NSE",
    }
}

fn order_status(code: i64) -> String {
    match code {
        1 => "cancelled",
        2 => "complete",
        4 => "trigger pending",
        5 => "rejected",
        6 => "open",
        _ => "unknown",
    }
    .to_string()
}

fn side(code: i64) -> String {
    if code == -1 { "SELL" } else { "BUY" }.to_string()
}

fn pricetype(code: i64) -> String {
    match code {
        1 => "LIMIT",
        3 => "SL-M",
        4 => "SL",
        _ => "MARKET",
    }
    .to_string()
}

fn fyers_type(pt: &str) -> i64 {
    match pt {
        "LIMIT" => 1,
        "SL-M" => 3,
        "SL" => 4,
        _ => 2,
    }
}

fn oa_product(p: &str) -> String {
    match p {
        "INTRADAY" => "MIS".into(),
        "MARGIN" => "NRML".into(),
        other => other.to_string(),
    }
}

fn fyers_product(p: &str) -> &'static str {
    match p {
        "CNC" => "CNC",
        "NRML" => "MARGIN",
        _ => "INTRADAY",
    }
}

fn clamp(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FyersOrder {
    #[serde(deserialize_with = "string_lenient")]
    id: String,
    #[serde(deserialize_with = "string_lenient")]
    exchOrdId: String,
    #[serde(deserialize_with = "string_lenient")]
    symbol: String,
    #[serde(deserialize_with = "i64_lenient")]
    exchange: i64,
    #[serde(deserialize_with = "i64_lenient")]
    segment: i64,
    #[serde(deserialize_with = "i64_lenient")]
    side: i64,
    #[serde(rename = "type", deserialize_with = "i64_lenient")]
    kind: i64,
    #[serde(deserialize_with = "i64_lenient")]
    status: i64,
    #[serde(deserialize_with = "i64_lenient")]
    qty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    filledQty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    remainingQuantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    limitPrice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    stopPrice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    tradedPrice: f64,
    #[serde(deserialize_with = "string_lenient")]
    productType: String,
    #[serde(deserialize_with = "string_lenient")]
    orderValidity: String,
    #[serde(deserialize_with = "string_lenient")]
    orderDateTime: String,
    #[serde(deserialize_with = "string_lenient")]
    message: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FyersTrade {
    #[serde(deserialize_with = "string_lenient")]
    symbol: String,
    #[serde(deserialize_with = "i64_lenient")]
    exchange: i64,
    #[serde(deserialize_with = "i64_lenient")]
    segment: i64,
    #[serde(deserialize_with = "i64_lenient")]
    side: i64,
    #[serde(deserialize_with = "string_lenient")]
    productType: String,
    #[serde(deserialize_with = "i64_lenient")]
    tradedQty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    tradePrice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    tradeValue: f64,
    #[serde(deserialize_with = "string_lenient")]
    orderNumber: String,
    #[serde(deserialize_with = "string_lenient")]
    tradeNumber: String,
    #[serde(deserialize_with = "string_lenient")]
    orderDateTime: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FyersPosition {
    #[serde(deserialize_with = "string_lenient")]
    symbol: String,
    #[serde(deserialize_with = "i64_lenient")]
    exchange: i64,
    #[serde(deserialize_with = "i64_lenient")]
    segment: i64,
    #[serde(deserialize_with = "i64_lenient")]
    netQty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    netAvg: f64,
    #[serde(deserialize_with = "f64_lenient")]
    ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pl: f64,
    #[serde(deserialize_with = "f64_lenient")]
    realized_profit: f64,
    #[serde(deserialize_with = "f64_lenient")]
    unrealized_profit: f64,
    #[serde(deserialize_with = "i64_lenient")]
    buyQty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    buyVal: f64,
    #[serde(deserialize_with = "i64_lenient")]
    sellQty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    sellVal: f64,
    #[serde(deserialize_with = "string_lenient")]
    productType: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FyersHolding {
    #[serde(deserialize_with = "string_lenient")]
    symbol: String,
    #[serde(deserialize_with = "i64_lenient")]
    exchange: i64,
    #[serde(deserialize_with = "i64_lenient")]
    segment: i64,
    #[serde(deserialize_with = "i64_lenient")]
    quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    costPrice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pl: f64,
    #[serde(deserialize_with = "string_lenient")]
    isin: String,
    #[serde(deserialize_with = "string_lenient")]
    holdingType: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FundLimit {
    #[serde(deserialize_with = "string_lenient")]
    title: String,
    #[serde(deserialize_with = "f64_lenient")]
    equityAmount: f64,
    #[serde(deserialize_with = "f64_lenient")]
    commodityAmount: f64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct FyersDepthLevel {
    #[serde(deserialize_with = "f64_lenient")]
    price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    volume: i64,
    #[serde(deserialize_with = "i64_lenient")]
    ord: i64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct FyersDepth {
    bids: Vec<FyersDepthLevel>,
    ask: Vec<FyersDepthLevel>,
    #[serde(deserialize_with = "f64_lenient")]
    o: f64,
    #[serde(deserialize_with = "f64_lenient")]
    h: f64,
    #[serde(deserialize_with = "f64_lenient")]
    l: f64,
    #[serde(deserialize_with = "f64_lenient")]
    c: f64,
    #[serde(deserialize_with = "f64_lenient")]
    ltp: f64,
    #[serde(deserialize_with = "i64_lenient")]
    ltq: i64,
    #[serde(deserialize_with = "i64_lenient")]
    v: i64,
    #[serde(deserialize_with = "i64_lenient")]
    oi: i64,
    #[serde(deserialize_with = "i64_lenient")]
    totalbuyqty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    totalsellqty: i64,
}

/// One quote from `/data/depth` (web `get_quotes`, which uses depth for OI).
fn depth_quote(key: &QuoteKey, d: &FyersDepth) -> Quote {
    let bid = d.bids.first().cloned().unwrap_or_default();
    let ask = d.ask.first().cloned().unwrap_or_default();
    let (change, change_percent) = if d.c > 0.0 {
        (d.ltp - d.c, (d.ltp - d.c) / d.c * 100.0)
    } else {
        (0.0, 0.0)
    };
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: d.ltp,
        open: d.o,
        high: d.h,
        low: d.l,
        close: d.c,
        volume: d.v,
        bid: bid.price,
        ask: ask.price,
        bid_qty: bid.volume,
        ask_qty: ask.volume,
        oi: d.oi,
        change,
        change_percent,
        timestamp: String::new(),
    }
}

fn depth_book(key: &QuoteKey, d: &FyersDepth) -> MarketDepth {
    let pad = |side: &[FyersDepthLevel]| -> Vec<DepthLevel> {
        (0..5)
            .map(|i| {
                side.get(i)
                    .map(|l| DepthLevel {
                        price: l.price,
                        quantity: l.volume,
                        orders: l.ord,
                    })
                    .unwrap_or_default()
            })
            .collect()
    };
    MarketDepth {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        bids: pad(&d.bids),
        asks: pad(&d.ask),
        ltp: d.ltp,
        ltq: d.ltq,
        open: d.o,
        high: d.h,
        low: d.l,
        prev_close: d.c,
        volume: d.v,
        oi: d.oi,
        total_buy_qty: d.totalbuyqty,
        total_sell_qty: d.totalsellqty,
    }
}

fn place_body(o: &ResolvedOrder) -> Value {
    json!({
        "symbol": o.brsymbol(),
        "qty": o.quantity,
        "type": fyers_type(o.pricetype.as_str()),
        "side": if o.action.as_str() == "BUY" { 1 } else { -1 },
        "productType": fyers_product(o.product.as_str()),
        "limitPrice": o.price,
        "stopPrice": o.trigger_price,
        "validity": "DAY",
        "disclosedQty": o.disclosed_quantity,
        "offlineOrder": false,
        "stopLoss": 0,
        "takeProfit": 0,
        "orderTag": "openalgo",
    })
}

fn modify_body(m: &ResolvedModify) -> Value {
    json!({
        "id": m.order_id,
        "qty": m.quantity,
        "type": fyers_type(m.pricetype.as_str()),
        "limitPrice": m.price,
        "stopPrice": m.trigger_price,
    })
}

#[async_trait]
impl Broker for FyersBroker {
    fn id(&self) -> &'static str {
        "fyers"
    }

    fn name(&self) -> &'static str {
        "Fyers"
    }

    fn logo(&self) -> &'static str {
        "/logos/fyers.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::Redirect { param: "auth_code" }
    }

    fn supported_exchanges(&self) -> &'static [Exchange] {
        SUPPORTED_EXCHANGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)] {
        TIMEFRAME_MAP
    }

    fn symbols(&self) -> Option<&SymbolResolver> {
        Some(&self.symbols)
    }

    async fn authenticate(&self, credentials: BrokerCredentials) -> Result<AuthResponse> {
        let code = credentials
            .auth_code
            .or(credentials.request_token)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::Validation(
                    "Fyers did not return a login code. Start the Fyers login again.".into(),
                )
            })?;
        let secret = credentials
            .api_secret
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::Validation(
                    "Your Fyers app secret is missing. Add it on the broker settings page.".into(),
                )
            })?;
        #[derive(Serialize)]
        struct Validate<'a> {
            grant_type: &'a str,
            appIdHash: String,
            code: &'a str,
        }
        let resp = self
            .http
            .post(format!("{}/validate-authcode", API_URL))
            .header("Accept", "application/json")
            .json(&Validate {
                grant_type: "authorization_code",
                appIdHash: app_id_hash(&credentials.api_key, &secret),
                code: &code,
            })
            .send()
            .await?;
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Validated {
            s: String,
            message: String,
            access_token: String,
        }
        let (_, v): (_, Validated) = http::read_json("fyers", resp).await?;
        if v.s != "ok" || v.access_token.is_empty() {
            tracing::warn!("Fyers login refused");
            return Err(AppError::Auth(if v.message.is_empty() {
                "Fyers did not accept the login. Start the Fyers login again.".into()
            } else {
                v.message
            }));
        }
        let user_id = credentials.client_id.unwrap_or_else(|| {
            credentials
                .api_key
                .split('-')
                .next()
                .unwrap_or("")
                .to_string()
        });
        Ok(AuthResponse {
            auth_token: format!("{}:{}", credentials.api_key, v.access_token),
            feed_token: None,
            user_id,
            user_name: None,
        })
    }

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        let env = self
            .call(
                Method::POST,
                &format!("{}/orders/sync", API_URL),
                auth,
                Some(&place_body(order)),
            )
            .await?;
        if env.id.is_empty() {
            return Err(AppError::Broker("Fyers did not return an order id.".into()));
        }
        Ok(OrderResponse {
            order_id: env.id,
            message: None,
        })
    }

    async fn modify_order(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
    ) -> Result<OrderResponse> {
        let env = self
            .call(
                Method::PATCH,
                &format!("{}/orders/sync", API_URL),
                auth,
                Some(&modify_body(order)),
            )
            .await?;
        Ok(OrderResponse {
            order_id: if env.id.is_empty() {
                order.order_id.clone()
            } else {
                env.id
            },
            message: None,
        })
    }

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        let env = self
            .call(
                Method::DELETE,
                &format!("{}/orders/sync", API_URL),
                auth,
                Some(&json!({"id": order_id})),
            )
            .await?;
        Ok(OrderResponse {
            order_id: if env.id.is_empty() {
                order_id.to_string()
            } else {
                env.id
            },
            message: None,
        })
    }

    async fn get_order_book(&self, auth: &AuthToken) -> Result<Vec<Order>> {
        let mut env = self
            .call(Method::GET, &format!("{}/orders", API_URL), auth, None)
            .await?;
        let rows: Vec<FyersOrder> = Self::rows(&mut env, "orderBook");
        Ok(rows
            .into_iter()
            .map(|o| {
                let ex = exchange_name(o.exchange, o.segment);
                let status = order_status(o.status);
                Order {
                    symbol: self.oa_symbol(&o.symbol, ex),
                    exchange: ex.to_string(),
                    exchange_order_id: (!o.exchOrdId.is_empty()).then_some(o.exchOrdId),
                    side: side(o.side),
                    quantity: clamp(o.qty),
                    filled_quantity: clamp(o.filledQty),
                    pending_quantity: clamp(o.remainingQuantity),
                    price: o.limitPrice,
                    trigger_price: o.stopPrice,
                    average_price: o.tradedPrice,
                    order_type: pricetype(o.kind),
                    product: oa_product(&o.productType),
                    rejection_reason: (status == "rejected" && !o.message.is_empty())
                        .then_some(o.message),
                    status,
                    validity: if o.orderValidity.is_empty() {
                        "DAY".into()
                    } else {
                        o.orderValidity
                    },
                    order_timestamp: o.orderDateTime,
                    exchange_timestamp: None,
                    order_id: o.id,
                }
            })
            .collect())
    }

    async fn get_trade_book(&self, auth: &AuthToken) -> Result<Vec<Trade>> {
        let mut env = self
            .call(Method::GET, &format!("{}/tradebook", API_URL), auth, None)
            .await?;
        let rows: Vec<FyersTrade> = Self::rows(&mut env, "tradeBook");
        Ok(rows
            .into_iter()
            .map(|t| {
                let ex = exchange_name(t.exchange, t.segment);
                Trade {
                    symbol: self.oa_symbol(&t.symbol, ex),
                    exchange: ex.to_string(),
                    product: oa_product(&t.productType),
                    side: side(t.side),
                    quantity: clamp(t.tradedQty),
                    average_price: t.tradePrice,
                    trade_value: t.tradeValue,
                    order_id: t.orderNumber,
                    trade_id: t.tradeNumber,
                    timestamp: t.orderDateTime,
                }
            })
            .collect())
    }

    async fn get_positions(&self, auth: &AuthToken) -> Result<Vec<Position>> {
        let mut env = self
            .call(Method::GET, &format!("{}/positions", API_URL), auth, None)
            .await?;
        let rows: Vec<FyersPosition> = Self::rows(&mut env, "netPositions");
        Ok(rows
            .into_iter()
            .map(|p| {
                let ex = exchange_name(p.exchange, p.segment);
                Position {
                    symbol: self.oa_symbol(&p.symbol, ex),
                    exchange: ex.to_string(),
                    product: oa_product(&p.productType),
                    quantity: clamp(p.netQty),
                    overnight_quantity: 0,
                    average_price: p.netAvg,
                    ltp: p.ltp,
                    pnl: p.pl,
                    realized_pnl: p.realized_profit,
                    unrealized_pnl: p.unrealized_profit,
                    buy_quantity: clamp(p.buyQty),
                    buy_value: p.buyVal,
                    sell_quantity: clamp(p.sellQty),
                    sell_value: p.sellVal,
                }
            })
            .collect())
    }

    async fn get_holdings(&self, auth: &AuthToken) -> Result<Vec<Holding>> {
        let mut env = self
            .call(Method::GET, &format!("{}/holdings", API_URL), auth, None)
            .await?;
        let rows: Vec<FyersHolding> = Self::rows(&mut env, "holdings");
        Ok(rows
            .into_iter()
            .map(|h| {
                let ex = exchange_name(h.exchange, h.segment);
                let pnl_percentage = if h.costPrice != 0.0 {
                    (h.ltp - h.costPrice) / h.costPrice * 100.0
                } else {
                    0.0
                };
                Holding {
                    symbol: self.oa_symbol(&h.symbol, ex),
                    exchange: ex.to_string(),
                    product: "CNC".into(),
                    isin: (!h.isin.is_empty()).then_some(h.isin),
                    t1_quantity: if h.holdingType == "T1" {
                        clamp(h.quantity)
                    } else {
                        0
                    },
                    quantity: clamp(h.quantity),
                    average_price: h.costPrice,
                    ltp: h.ltp,
                    close_price: 0.0,
                    pnl: h.pl,
                    pnl_percentage,
                    current_value: h.quantity as f64 * h.ltp,
                }
            })
            .collect())
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        let mut env = self
            .call(Method::GET, &format!("{}/funds", API_URL), auth, None)
            .await?;
        if env.code != 200 {
            return Err(AppError::Broker(
                "Fyers did not return your funds. Try again.".into(),
            ));
        }
        let limits: Vec<FundLimit> = Self::rows(&mut env, "fund_limit");
        let get = |k: &str| -> f64 {
            limits
                .iter()
                .find(|f| f.title.to_lowercase().replace(' ', "_") == k)
                .map(|f| f.equityAmount + f.commodityAmount)
                .unwrap_or(0.0)
        };
        let mut funds = Funds {
            // web uses Clear Balance: Available Balance already folds in collateral.
            available_cash: get("clear_balance"),
            used_margin: get("utilized_amount"),
            total_margin: get("total_balance"),
            opening_balance: get("total_balance"),
            payin: get("receivables"),
            payout: 0.0,
            span: 0.0,
            exposure: 0.0,
            collateral: get("collaterals"),
            m2m_unrealized: 0.0,
            m2m_realized: 0.0,
            utilised_debits: get("utilized_amount"),
        };
        match self.get_positions(auth).await {
            Ok(ps) => {
                funds.m2m_realized = ps.iter().map(|p| p.realized_pnl).sum();
                funds.m2m_unrealized = ps.iter().map(|p| p.unrealized_pnl).sum();
            }
            Err(e) => tracing::warn!("Fyers position P&L for funds failed: {}", e.code()),
        }
        Ok(funds)
    }

    async fn get_quote(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
        let (_, d) = self.depth_raw(auth, key).await?;
        Ok(depth_quote(key, &d))
    }

    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        let (_, d) = self.depth_raw(auth, key).await?;
        Ok(depth_book(key, &d))
    }

    async fn get_history(&self, _auth: &AuthToken, _req: &HistoryRequest) -> Result<Vec<Candle>> {
        // next wave: /data/history with resolution map and 300/60/25-day chunks.
        Err(AppError::Unsupported("history"))
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        let mut all = Vec::new();
        for (key, url) in [
            ("NSE_CM", "https://public.fyers.in/sym_details/NSE_CM.csv"),
            ("NSE_FO", "https://public.fyers.in/sym_details/NSE_FO.csv"),
            ("BSE_CM", "https://public.fyers.in/sym_details/BSE_CM.csv"),
            ("BSE_FO", "https://public.fyers.in/sym_details/BSE_FO.csv"),
        ] {
            let text = self
                .http
                .get(url)
                .timeout(http::DOWNLOAD_TIMEOUT)
                .send()
                .await?
                .text()
                .await?;
            all.extend(master::process_csv(&text, key));
        }
        // CDS and MCX come from the JSON masters: their `qtyMultiplier` is the
        // real lot (the CSV's "Minimum lot size" is 1 on every row).
        for (exchange, url) in [
            (
                "CDS",
                "https://public.fyers.in/sym_details/NSE_CD_sym_master.json",
            ),
            (
                "MCX",
                "https://public.fyers.in/sym_details/MCX_COM_sym_master.json",
            ),
        ] {
            let resp = self
                .http
                .get(url)
                .timeout(http::DOWNLOAD_TIMEOUT)
                .send()
                .await?;
            let (_, map): (_, HashMap<String, master::JsonRow>) =
                http::read_json("fyers", resp).await?;
            all.extend(master::process_json(map.into_values(), exchange));
        }
        tracing::info!("Fyers master contract parsed: {} instruments", all.len());
        Ok(all)
    }
}

/// Master-contract parsing (web `database/master_contract_db.py`).
pub mod master {
    use super::*;

    /// `"BANKNIFTY 27 Oct 26 FUT"` -> `BANKNIFTY27OCT26FUT` (DDMMMYY, the
    /// order every other broker uses); options keep the strike and take the
    /// option type appended by the caller.
    pub fn reformat_symbol_detail(details: &str) -> Option<String> {
        let p: Vec<&str> = details.split_whitespace().collect();
        if p.len() < 5 {
            return None;
        }
        Some(format!(
            "{}{}{}{}{}",
            p[0],
            p[1],
            p[2].to_uppercase(),
            p[3],
            p[4]
        ))
    }

    fn expiry_from_epoch(secs: i64) -> String {
        if secs <= 0 {
            return String::new();
        }
        chrono::DateTime::from_timestamp(secs, 0)
            .map(|d| format_expiry(d.date_naive()))
            .unwrap_or_default()
    }

    /// One derivative row (CSV or JSON master).
    #[allow(clippy::too_many_arguments)]
    fn derivative(
        token: &str,
        details: &str,
        ticker: &str,
        option_type: &str,
        expiry_epoch: i64,
        strike: f64,
        lot: i64,
        tick: f64,
        exchange: &str,
    ) -> Option<SymToken> {
        let instrument_type = match option_type {
            "CE" | "PE" => option_type.to_string(),
            _ => "FUT".to_string(),
        };
        let base = reformat_symbol_detail(details)?;
        let symbol = match option_type {
            "CE" | "PE" => format!("{}{}", base, option_type),
            _ => base,
        };
        Some(SymToken {
            symbol,
            brsymbol: ticker.to_string(),
            name: details.to_string(),
            exchange: exchange.to_string(),
            brexchange: exchange.to_string(),
            token: token.to_string(),
            expiry: expiry_from_epoch(expiry_epoch),
            strike,
            lot_size: i32::try_from(lot).unwrap_or(1),
            instrument_type,
            tick_size: tick,
        })
    }

    /// The 21-column Fyers CSV (no header, no quoted fields).
    pub fn process_csv(text: &str, key: &str) -> Vec<SymToken> {
        let mut out = Vec::new();
        for line in text.lines() {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            if f.len() < 17 || f[0].is_empty() || f[9].is_empty() {
                continue;
            }
            let itype: i64 = f[2].parse().unwrap_or(-1);
            let lot: i64 = f[3].parse::<f64>().map(|v| v as i64).unwrap_or(1);
            let tick: f64 = f[4].parse().unwrap_or(0.05);
            let expiry: i64 = f[8].parse::<f64>().map(|v| v as i64).unwrap_or(0);
            let strike: f64 = f[15].parse().unwrap_or(0.0);
            let row = match key {
                "NSE_CM" | "BSE_CM" => {
                    let (eq_types, ex, idx_ex): (&[i64], &str, &str) = if key == "NSE_CM" {
                        (&[0, 9], "NSE", "NSE_INDEX")
                    } else {
                        (&[0, 4, 50], "BSE", "BSE_INDEX")
                    };
                    let exchange = if eq_types.contains(&itype)
                        || (key == "NSE_CM" && itype == 2 && f[9].ends_with("-GB"))
                    {
                        ex
                    } else if itype == 10 {
                        idx_ex
                    } else {
                        continue;
                    };
                    let mut symbol = f[13].to_string();
                    if exchange == "NSE_INDEX" {
                        symbol = symbol.replace([' ', '-'], "");
                        if symbol == "NIFTYMID50" {
                            symbol = "NIFTYMIDCAP50".into();
                        }
                    }
                    // next wave: BSE index rename table (100 -> BSE100, ...).
                    Some(SymToken {
                        symbol,
                        brsymbol: f[9].to_string(),
                        name: f[1].to_string(),
                        exchange: exchange.to_string(),
                        brexchange: ex.to_string(),
                        token: f[0].to_string(),
                        expiry: String::new(),
                        strike,
                        lot_size: i32::try_from(lot).unwrap_or(1),
                        instrument_type: "EQ".into(),
                        tick_size: tick,
                    })
                }
                "NSE_FO" => derivative(f[0], f[1], f[9], f[16], expiry, strike, lot, tick, "NFO"),
                "BSE_FO" => derivative(f[0], f[1], f[9], f[16], expiry, strike, lot, tick, "BFO"),
                _ => None,
            };
            if let Some(r) = row {
                out.push(r);
            }
        }
        out
    }

    /// A row of `NSE_CD_sym_master.json` / `MCX_COM_sym_master.json`.
    #[derive(Debug, Deserialize, Default)]
    #[serde(default)]
    pub struct JsonRow {
        #[serde(deserialize_with = "string_lenient")]
        pub fyToken: String,
        #[serde(deserialize_with = "string_lenient")]
        pub symDetails: String,
        #[serde(deserialize_with = "string_lenient")]
        pub symTicker: String,
        #[serde(deserialize_with = "string_lenient")]
        pub optType: String,
        #[serde(deserialize_with = "i64_lenient")]
        pub expiryDate: i64,
        #[serde(deserialize_with = "f64_lenient")]
        pub strikePrice: f64,
        #[serde(deserialize_with = "i64_lenient")]
        pub qtyMultiplier: i64,
        #[serde(deserialize_with = "f64_lenient")]
        pub tickSize: f64,
    }

    pub fn process_json(rows: impl IntoIterator<Item = JsonRow>, exchange: &str) -> Vec<SymToken> {
        rows.into_iter()
            .filter_map(|r| {
                derivative(
                    &r.fyToken,
                    &r.symDetails,
                    &r.symTicker,
                    &r.optType,
                    r.expiryDate,
                    r.strikePrice,
                    r.qtyMultiplier.max(1),
                    r.tickSize,
                    exchange,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::master::*;
    use super::*;

    #[test]
    fn derivative_symbols_are_ddmmmyy() {
        assert_eq!(
            reformat_symbol_detail("BANKNIFTY 27 Oct 26 FUT").as_deref(),
            Some("BANKNIFTY27OCT26FUT")
        );
        let csv = "101125102771850,BANKNIFTY 27 Oct 26 71900 CE,14,30,0.05,,0915-1530|1815-1915:,2026-10-03,1793098800,NSE:BANKNIFTY26OCT71900CE,10,11,71850,BANKNIFTY,26009,71900.0,CE,101000000026009,None,None,None\n\
101125102712345,BANKNIFTY 27 Oct 26 FUT,11,30,0.2,,0915-1530|1815-1915:,2026-10-03,1793098800,NSE:BANKNIFTY26OCTFUT,10,11,62345,BANKNIFTY,26009,-1.0,XX,101000000026009,None,None,None";
        let rows = process_csv(csv, "NSE_FO");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].symbol, "BANKNIFTY27OCT2671900CE");
        assert_eq!(rows[0].instrument_type, "CE");
        assert_eq!(rows[0].expiry, "27-OCT-26");
        assert_eq!(rows[0].brsymbol, "NSE:BANKNIFTY26OCT71900CE");
        assert_eq!(rows[0].lot_size, 30);
        assert_eq!(rows[1].symbol, "BANKNIFTY27OCT26FUT");
        assert_eq!(rows[1].instrument_type, "FUT");
    }

    #[test]
    fn cds_and_mcx_lots_come_from_qty_multiplier() {
        let json = r#"{
          "MCX:CRUDEOIL26OCTFUT": {"fyToken": "1120261019123", "symDetails": "CRUDEOIL 19 Oct 26 FUT", "symTicker": "MCX:CRUDEOIL26OCTFUT", "optType": "XX", "expiryDate": "1792432800", "strikePrice": -1.0, "qtyMultiplier": 100, "tickSize": 1.0},
          "NSE:USDINR26OCT83.25CE": {"fyToken": "1012261029123", "symDetails": "USDINR 29 Oct 26 83.25 CE", "symTicker": "NSE:USDINR26OCT83.25CE", "optType": "CE", "expiryDate": 1793269800, "strikePrice": 83.25, "qtyMultiplier": 1000.0, "tickSize": 0.0025}
        }"#;
        let map: HashMap<String, JsonRow> = serde_json::from_str(json).unwrap();
        let mut mcx = process_json(
            map.into_iter()
                .filter(|(k, _)| k.starts_with("MCX"))
                .map(|(_, v)| v),
            "MCX",
        );
        let crude = mcx.remove(0);
        assert_eq!(crude.symbol, "CRUDEOIL19OCT26FUT");
        assert_eq!(crude.lot_size, 100);
        let map: HashMap<String, JsonRow> = serde_json::from_str(json).unwrap();
        let cds = process_json(
            map.into_iter()
                .filter(|(k, _)| k.contains("USDINR"))
                .map(|(_, v)| v),
            "CDS",
        );
        assert_eq!(cds[0].symbol, "USDINR29OCT2683.25CE");
        assert_eq!(cds[0].lot_size, 1000);
    }

    #[test]
    fn nse_index_symbols_are_normalised() {
        let csv = "101000000026000,NIFTY 50,10,1,0.05,,0915-1530|1815-1915:,2026-10-03,,NSE:NIFTY50-INDEX,10,10,26000,NIFTY 50,26000,-1.0,XX,101000000026000,None,None,None\n\
101000000026013,NIFTYMID50,10,1,0.05,,0915-1530|1815-1915:,2026-10-03,,NSE:NIFTYMIDCAP50-INDEX,10,10,26013,NIFTYMID50,26013,-1.0,XX,101000000026013,None,None,None\n\
10100000003045,STATE BANK OF INDIA,0,1,0.05,INE062A01020,0915-1530|1815-1915:,2026-10-03,,NSE:SBIN-EQ,10,10,3045,SBIN,3045,-1.0,XX,10100000003045,None,None,None";
        let rows = process_csv(csv, "NSE_CM");
        assert_eq!(rows[0].symbol, "NIFTY50");
        assert_eq!(rows[0].exchange, "NSE_INDEX");
        assert_eq!(rows[0].instrument_type, "EQ");
        assert_eq!(rows[1].symbol, "NIFTYMIDCAP50");
        assert_eq!(rows[2].symbol, "SBIN");
        assert_eq!(rows[2].brsymbol, "NSE:SBIN-EQ");
    }

    #[test]
    fn books_map_back_to_openalgo_symbols() {
        let r = SymbolResolver::new();
        r.load(vec![SymToken {
            symbol: "SBIN".into(),
            brsymbol: "NSE:SBIN-EQ".into(),
            name: "SBIN".into(),
            exchange: "NSE".into(),
            brexchange: "NSE".into(),
            token: "10100000003045".into(),
            expiry: String::new(),
            strike: -1.0,
            lot_size: 1,
            instrument_type: "EQ".into(),
            tick_size: 0.05,
        }]);
        let b = FyersBroker::new(r.clone());
        assert_eq!(b.oa_symbol("NSE:SBIN-EQ", "NSE"), "SBIN");
        assert_eq!(b.oa_symbol("NSE:XYZ-EQ", "NSE"), "XYZ-EQ");
        assert_eq!(order_status(4), "trigger pending");
        assert_eq!(exchange_name(11, 20), "MCX");
        let req = OrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            side: "SELL".into(),
            quantity: 2,
            price: 0.0,
            order_type: "SL-M".into(),
            product: "NRML".into(),
            validity: "IOC".into(),
            trigger_price: Some(900.0),
            disclosed_quantity: None,
            amo: true,
        };
        let body = place_body(&ResolvedOrder::resolve(&req, &r).unwrap());
        assert_eq!(body["symbol"], "NSE:SBIN-EQ");
        assert_eq!(body["type"], 3);
        assert_eq!(body["side"], -1);
        assert_eq!(body["productType"], "MARGIN");
        assert_eq!(body["validity"], "DAY");
        assert_eq!(body["offlineOrder"], false);
        assert_eq!(body["orderTag"], "openalgo");
    }

    #[test]
    fn quotes_come_from_depth() {
        let d: FyersDepth = serde_json::from_str(
            r#"{"totalbuyqty": 1000, "totalsellqty": 900, "bids": [{"price": 954.0, "volume": 10, "ord": 2}], "ask": [{"price": 954.1, "volume": 5, "ord": 1}], "o": 951, "h": 957.4, "l": 948.2, "c": 950.65, "ltp": 954.1, "ltq": 3, "v": 4823170, "oi": 0}"#,
        )
        .unwrap();
        let k = QuoteKey::new("NSE", "SBIN");
        let q = depth_quote(&k, &d);
        assert_eq!((q.bid, q.ask, q.bid_qty, q.ask_qty), (954.0, 954.1, 10, 5));
        let book = depth_book(&k, &d);
        assert_eq!(book.bids.len(), 5);
        assert_eq!(book.bids[0].orders, 2);
        assert_eq!(book.total_buy_qty, 1000);
        assert_eq!(
            app_id_hash("a", "b"),
            "6783a31eabf68ccc0660f935c0826282bdd2241f3a80a9f2d10d59aea9ebb5d8"
        );
    }
}
