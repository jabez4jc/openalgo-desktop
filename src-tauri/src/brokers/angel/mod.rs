//! Angel One SmartAPI adapter (web `broker/angel/**`).
//!
//! The stored session token is `api_key:jwt` so every secure call can send
//! the API key in `X-PrivateKey` (audit A.X1: it used to go out empty).
//!
//! next wave: history (`getCandleData` + `getOIData`), margin
//! (`margin/v1/batch`), the SmartAPI streaming feed and quote batching beyond
//! 50 tokens are not ported yet; those calls report `Unsupported` rather
//! than answering with partial data.

#![allow(non_snake_case)]

use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::http;
use crate::brokers::common::mapping::Exchange;
use crate::brokers::common::master_contract::{
    expiry_compact, format_expiry, format_strike, parse_broker_expiry,
};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::brokers::{lower_status, AuthResponse, Broker, BrokerCredentials};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

const BASE_URL: &str = "https://apiconnect.angelone.in";
const MASTER_CONTRACT_URL: &str =
    "https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json";

/// web `plugin.json`.
const SUPPORTED_EXCHANGES: &[Exchange] = &[
    Exchange::Nse,
    Exchange::Bse,
    Exchange::Nfo,
    Exchange::Bfo,
    Exchange::Cds,
    Exchange::Mcx,
    Exchange::NseIndex,
    Exchange::BseIndex,
    Exchange::McxIndex,
];

/// web `BrokerData.timeframe_map` (history itself is next wave).
const TIMEFRAME_MAP: &[(&str, &str)] = &[
    ("1m", "ONE_MINUTE"),
    ("3m", "THREE_MINUTE"),
    ("5m", "FIVE_MINUTE"),
    ("10m", "TEN_MINUTE"),
    ("15m", "FIFTEEN_MINUTE"),
    ("30m", "THIRTY_MINUTE"),
    ("1h", "ONE_HOUR"),
    ("D", "ONE_DAY"),
];

pub struct AngelBroker {
    http: reqwest::Client,
    base_url: String,
    symbols: SymbolResolver,
}

fn session_expired() -> AppError {
    AppError::Auth("Your Angel One session has expired. Log in to Angel One again.".into())
}

impl AngelBroker {
    pub fn new(symbols: SymbolResolver) -> Self {
        Self {
            http: http::client(),
            base_url: BASE_URL.to_string(),
            symbols,
        }
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        api_key: &str,
        jwt: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut r = self
            .http
            .request(method, format!("{}{}", self.base_url, path))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("X-UserType", "USER")
            .header("X-SourceID", "WEB")
            .header("X-ClientLocalIP", "CLIENT_LOCAL_IP")
            .header("X-ClientPublicIP", "CLIENT_PUBLIC_IP")
            .header("X-MACAddress", "MAC_ADDRESS")
            .header("X-PrivateKey", api_key);
        if let Some(t) = jwt {
            r = r.header("Authorization", format!("Bearer {}", t));
        }
        r
    }

    /// One secure call; `status: false` becomes a broker error.
    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        auth: &AuthToken,
        body: Option<&Value>,
    ) -> Result<Option<T>> {
        let (api_key, jwt) = auth.pair().ok_or_else(session_expired)?;
        let mut req = self.request(method, path, api_key, Some(jwt));
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await?;
        let (status, env): (_, AngelResponse<T>) = http::read_json("angel", resp).await?;
        if !env.status {
            tracing::warn!(status = status.as_u16(), code = %env.errorcode, "Angel One refused {}: {}", path, env.message);
            if matches!(
                env.errorcode.as_str(),
                "AG8001" | "AG8002" | "AG8003" | "AB1010"
            ) {
                return Err(session_expired());
            }
            return Err(AppError::Broker(if env.message.is_empty() {
                "Angel One refused the request.".into()
            } else {
                env.message
            }));
        }
        Ok(env.data)
    }

    fn lookup(&self, key: &QuoteKey) -> Result<SymToken> {
        self.symbols.by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
            AppError::Validation(format!(
                "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
                key.symbol, key.exchange
            ))
        })
    }

    /// OpenAlgo symbol for a book row: by token first (web `get_symbol`),
    /// then by broker symbol, else the raw tradingsymbol.
    fn oa_symbol(&self, token: &str, brsymbol: &str, exchange: &str) -> String {
        if !token.is_empty() {
            if let Some(r) = self.symbols.by_token(exchange, token) {
                return r.symbol;
            }
        }
        self.symbols.oa_symbol_or_raw(brsymbol, exchange)
    }

    async fn fetch_quotes(
        &self,
        auth: &AuthToken,
        rows: &[(QuoteKey, SymToken)],
    ) -> Result<Vec<AngelQuoteData>> {
        let mut out = Vec::new();
        // SmartAPI accepts at most 50 tokens per quote call.
        for chunk in rows.chunks(50) {
            let mut tokens: HashMap<String, Vec<String>> = HashMap::new();
            for (_, r) in chunk {
                tokens
                    .entry(quote_exchange(&r.exchange).to_string())
                    .or_default()
                    .push(r.token.clone());
            }
            let body = json!({"mode": "FULL", "exchangeTokens": tokens});
            let data: Option<AngelQuoteResponse> = self
                .call(
                    Method::POST,
                    "/rest/secure/angelbroking/market/v1/quote/",
                    auth,
                    Some(&body),
                )
                .await?;
            out.extend(data.and_then(|d| d.fetched).unwrap_or_default());
        }
        Ok(out)
    }
}

/// `NSE_INDEX` -> `NSE` etc. for market-data calls.
fn quote_exchange(oa: &str) -> &str {
    match oa {
        "NSE_INDEX" => "NSE",
        "BSE_INDEX" => "BSE",
        "MCX_INDEX" => "MCX",
        other => other,
    }
}

#[derive(Deserialize)]
struct AngelResponse<T> {
    #[serde(default)]
    status: bool,
    #[serde(default, deserialize_with = "string_lenient")]
    message: String,
    #[serde(default, deserialize_with = "string_lenient")]
    errorcode: String,
    data: Option<T>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AngelOrderData {
    #[serde(deserialize_with = "string_lenient")]
    orderid: String,
    #[serde(deserialize_with = "string_lenient")]
    exchangeorderid: String,
    #[serde(deserialize_with = "string_lenient")]
    tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    symboltoken: String,
    #[serde(deserialize_with = "string_lenient")]
    exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    transactiontype: String,
    #[serde(deserialize_with = "i64_lenient")]
    quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    filledshares: i64,
    #[serde(deserialize_with = "i64_lenient")]
    unfilledshares: i64,
    #[serde(deserialize_with = "f64_lenient")]
    price: f64,
    #[serde(deserialize_with = "f64_lenient")]
    triggerprice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    averageprice: f64,
    #[serde(deserialize_with = "string_lenient")]
    ordertype: String,
    #[serde(deserialize_with = "string_lenient")]
    producttype: String,
    #[serde(deserialize_with = "string_lenient")]
    status: String,
    #[serde(deserialize_with = "string_lenient")]
    duration: String,
    #[serde(deserialize_with = "string_lenient")]
    updatetime: String,
    #[serde(deserialize_with = "string_lenient")]
    exchtime: String,
    #[serde(deserialize_with = "string_lenient")]
    text: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AngelTradeData {
    #[serde(deserialize_with = "string_lenient")]
    orderid: String,
    #[serde(deserialize_with = "string_lenient")]
    fillid: String,
    #[serde(deserialize_with = "string_lenient")]
    tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    symboltoken: String,
    #[serde(deserialize_with = "string_lenient")]
    exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    producttype: String,
    #[serde(deserialize_with = "string_lenient")]
    transactiontype: String,
    #[serde(deserialize_with = "i64_lenient")]
    fillsize: i64,
    #[serde(deserialize_with = "i64_lenient")]
    quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    fillprice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    tradevalue: f64,
    #[serde(deserialize_with = "string_lenient")]
    filltime: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AngelPositionData {
    #[serde(deserialize_with = "string_lenient")]
    tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    symboltoken: String,
    #[serde(deserialize_with = "string_lenient")]
    exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    producttype: String,
    #[serde(deserialize_with = "i64_lenient")]
    netqty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    cfbuyqty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    avgnetprice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pnl: f64,
    #[serde(deserialize_with = "f64_lenient")]
    realised: f64,
    #[serde(deserialize_with = "f64_lenient")]
    unrealised: f64,
    #[serde(deserialize_with = "i64_lenient")]
    buyqty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    buyamount: f64,
    #[serde(deserialize_with = "i64_lenient")]
    sellqty: i64,
    #[serde(deserialize_with = "f64_lenient")]
    sellamount: f64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AngelHoldingsResponse {
    holdings: Option<Vec<AngelHoldingData>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AngelHoldingData {
    #[serde(deserialize_with = "string_lenient")]
    tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    isin: String,
    #[serde(deserialize_with = "i64_lenient")]
    quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    t1quantity: i64,
    #[serde(deserialize_with = "f64_lenient")]
    averageprice: f64,
    #[serde(deserialize_with = "f64_lenient")]
    ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    close: f64,
    #[serde(deserialize_with = "f64_lenient")]
    profitandloss: f64,
    #[serde(deserialize_with = "f64_lenient")]
    pnlpercentage: f64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AngelFundsData {
    #[serde(deserialize_with = "f64_lenient")]
    availablecash: f64,
    #[serde(deserialize_with = "f64_lenient")]
    utilisedmargin: f64,
    #[serde(deserialize_with = "f64_lenient")]
    net: f64,
    #[serde(deserialize_with = "f64_lenient")]
    availableintradaypayin: f64,
    #[serde(deserialize_with = "f64_lenient")]
    utilisedpayout: f64,
    #[serde(deserialize_with = "f64_lenient")]
    utiliseddebits: f64,
    #[serde(deserialize_with = "f64_lenient")]
    utilisedspan: f64,
    #[serde(deserialize_with = "f64_lenient")]
    utilisedexposure: f64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AngelQuoteResponse {
    fetched: Option<Vec<AngelQuoteData>>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct AngelQuoteData {
    #[serde(deserialize_with = "string_lenient")]
    exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    symbolToken: String,
    #[serde(deserialize_with = "f64_lenient")]
    ltp: f64,
    #[serde(deserialize_with = "f64_lenient")]
    open: f64,
    #[serde(deserialize_with = "f64_lenient")]
    high: f64,
    #[serde(deserialize_with = "f64_lenient")]
    low: f64,
    #[serde(deserialize_with = "f64_lenient")]
    close: f64,
    #[serde(deserialize_with = "i64_lenient")]
    lastTradeQty: i64,
    #[serde(deserialize_with = "i64_lenient")]
    tradeVolume: i64,
    #[serde(deserialize_with = "i64_lenient")]
    opnInterest: i64,
    #[serde(deserialize_with = "i64_lenient")]
    totBuyQuan: i64,
    #[serde(deserialize_with = "i64_lenient")]
    totSellQuan: i64,
    depth: Option<AngelDepthData>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct AngelDepthData {
    buy: Vec<AngelDepthLevel>,
    sell: Vec<AngelDepthLevel>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct AngelDepthLevel {
    #[serde(deserialize_with = "f64_lenient")]
    price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    quantity: i64,
    #[serde(deserialize_with = "i64_lenient")]
    orders: i64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AngelSymbolData {
    #[serde(deserialize_with = "string_lenient")]
    token: String,
    #[serde(deserialize_with = "string_lenient")]
    symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    name: String,
    #[serde(deserialize_with = "string_lenient")]
    exch_seg: String,
    #[serde(deserialize_with = "string_lenient")]
    instrumenttype: String,
    #[serde(deserialize_with = "i64_lenient")]
    lotsize: i64,
    #[serde(deserialize_with = "f64_lenient")]
    tick_size: f64,
    #[serde(deserialize_with = "string_lenient")]
    expiry: String,
    #[serde(deserialize_with = "f64_lenient")]
    strike: f64,
}

// web mapping/transform_data.py
fn variety(pt: &str) -> &'static str {
    match pt {
        "SL" | "SL-M" => "STOPLOSS",
        _ => "NORMAL",
    }
}

fn order_type(pt: &str) -> &'static str {
    match pt {
        "LIMIT" => "LIMIT",
        "SL" => "STOPLOSS_LIMIT",
        "SL-M" => "STOPLOSS_MARKET",
        _ => "MARKET",
    }
}

fn product_type(p: &str) -> &'static str {
    match p {
        "CNC" => "DELIVERY",
        "NRML" => "CARRYFORWARD",
        _ => "INTRADAY",
    }
}

// web mapping/order_data.py
fn oa_product(exchange: &str, producttype: &str) -> String {
    match (exchange, producttype) {
        ("NSE" | "BSE", "DELIVERY") => "CNC".into(),
        (_, "INTRADAY") => "MIS".into(),
        ("NFO" | "MCX" | "BFO" | "CDS", "CARRYFORWARD") => "NRML".into(),
        _ => producttype.to_string(),
    }
}

fn oa_pricetype(ordertype: &str) -> String {
    match ordertype {
        "STOPLOSS_LIMIT" => "SL".into(),
        "STOPLOSS_MARKET" => "SL-M".into(),
        other => other.to_string(),
    }
}

fn num(v: f64) -> String {
    format_strike(v)
}

fn place_body(o: &ResolvedOrder) -> Value {
    let pt = o.pricetype.as_str();
    json!({
        "variety": if o.amo { "AMO" } else { variety(pt) },
        "tradingsymbol": o.brsymbol(),
        "symboltoken": o.token(),
        "transactiontype": o.action.as_str(),
        "exchange": o.brexchange(),
        "ordertype": order_type(pt),
        "producttype": product_type(o.product.as_str()),
        "duration": "DAY",
        "price": num(o.price),
        "triggerprice": num(o.trigger_price),
        "squareoff": "0",
        "stoploss": num(o.trigger_price),
        "quantity": o.quantity.to_string(),
    })
}

fn modify_body(m: &ResolvedModify) -> Value {
    let pt = m.pricetype.as_str();
    json!({
        "variety": variety(pt),
        "orderid": m.order_id,
        "ordertype": order_type(pt),
        "producttype": product_type(m.product.as_str()),
        "duration": "DAY",
        "price": num(m.price),
        "quantity": m.quantity.to_string(),
        "tradingsymbol": m.brsymbol(),
        "symboltoken": m.token(),
        "exchange": m.instrument.br_exchange(),
        "disclosedquantity": m.disclosed_quantity.to_string(),
        "stoploss": num(m.trigger_price),
        "triggerprice": num(m.trigger_price),
    })
}

fn clamp(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[derive(Deserialize)]
struct OrderIdData {
    #[serde(default, deserialize_with = "string_lenient")]
    orderid: String,
}

#[async_trait]
impl Broker for AngelBroker {
    fn id(&self) -> &'static str {
        "angel"
    }

    fn name(&self) -> &'static str {
        "Angel One"
    }

    fn logo(&self) -> &'static str {
        "/logos/angel.svg"
    }

    fn login_kind(&self) -> LoginKind {
        LoginKind::DirectTotp {
            fields: &["client_id", "password", "totp"],
        }
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
        let missing =
            |what: &str| AppError::Validation(format!("Enter your Angel One {} to log in.", what));
        let totp = credentials
            .totp
            .filter(|s| !s.is_empty())
            .ok_or_else(|| missing("TOTP"))?;
        let client_id = credentials
            .client_id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| missing("client ID"))?;
        let password = credentials
            .password
            .filter(|s| !s.is_empty())
            .ok_or_else(|| missing("PIN"))?;
        if credentials.api_key.is_empty() {
            return Err(AppError::Validation(
                "Your Angel One API key is missing. Add it on the broker settings page.".into(),
            ));
        }
        #[derive(Serialize)]
        struct Login<'a> {
            clientcode: &'a str,
            password: &'a str,
            totp: &'a str,
        }
        #[derive(Deserialize)]
        struct LoginData {
            jwtToken: String,
            #[serde(default)]
            feedToken: Option<String>,
        }
        let resp = self
            .request(
                Method::POST,
                "/rest/auth/angelbroking/user/v1/loginByPassword",
                &credentials.api_key,
                None,
            )
            .json(&Login {
                clientcode: &client_id,
                password: &password,
                totp: &totp,
            })
            .send()
            .await?;
        let (_, env): (_, AngelResponse<LoginData>) = http::read_json("angel", resp).await?;
        let data = match (env.status, env.data) {
            (true, Some(d)) => d,
            _ => {
                tracing::warn!(code = %env.errorcode, "Angel One login refused");
                return Err(AppError::Auth(if env.message.is_empty() {
                    "Angel One did not accept the login. Check your client ID, PIN and TOTP.".into()
                } else {
                    env.message
                }));
            }
        };
        Ok(AuthResponse {
            auth_token: format!("{}:{}", credentials.api_key, data.jwtToken),
            feed_token: data.feedToken,
            user_id: client_id,
            user_name: None,
        })
    }

    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse> {
        let body = place_body(order);
        let data: Option<OrderIdData> = self
            .call(
                Method::POST,
                "/rest/secure/angelbroking/order/v1/placeOrder",
                auth,
                Some(&body),
            )
            .await?;
        let id = data
            .map(|d| d.orderid)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::Broker("Angel One did not return an order id.".into()))?;
        Ok(OrderResponse {
            order_id: id,
            message: None,
        })
    }

    async fn modify_order(
        &self,
        auth: &AuthToken,
        order: &ResolvedModify,
    ) -> Result<OrderResponse> {
        let body = modify_body(order);
        let data: Option<OrderIdData> = self
            .call(
                Method::POST,
                "/rest/secure/angelbroking/order/v1/modifyOrder",
                auth,
                Some(&body),
            )
            .await?;
        Ok(OrderResponse {
            order_id: data
                .map(|d| d.orderid)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| order.order_id.clone()),
            message: None,
        })
    }

    async fn cancel_order(&self, auth: &AuthToken, order_id: &str) -> Result<OrderResponse> {
        let body = json!({"variety": "NORMAL", "orderid": order_id});
        let data: Option<OrderIdData> = self
            .call(
                Method::POST,
                "/rest/secure/angelbroking/order/v1/cancelOrder",
                auth,
                Some(&body),
            )
            .await?;
        Ok(OrderResponse {
            order_id: data
                .map(|d| d.orderid)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| order_id.to_string()),
            message: None,
        })
    }

    async fn get_order_book(&self, auth: &AuthToken) -> Result<Vec<Order>> {
        let rows: Vec<AngelOrderData> = self
            .call(
                Method::GET,
                "/rest/secure/angelbroking/order/v1/getOrderBook",
                auth,
                None,
            )
            .await?
            .unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|o| Order {
                symbol: self.oa_symbol(&o.symboltoken, &o.tradingsymbol, &o.exchange),
                order_type: oa_pricetype(&o.ordertype),
                product: oa_product(&o.exchange, &o.producttype),
                status: lower_status(&o.status),
                exchange_order_id: (!o.exchangeorderid.is_empty()).then_some(o.exchangeorderid),
                rejection_reason: (!o.text.is_empty()).then_some(o.text),
                exchange_timestamp: (!o.exchtime.is_empty()).then_some(o.exchtime),
                order_id: o.orderid,
                exchange: o.exchange,
                side: o.transactiontype,
                quantity: clamp(o.quantity),
                filled_quantity: clamp(o.filledshares),
                pending_quantity: clamp(o.unfilledshares),
                price: o.price,
                trigger_price: o.triggerprice,
                average_price: o.averageprice,
                validity: o.duration,
                order_timestamp: o.updatetime,
            })
            .collect())
    }

    async fn get_trade_book(&self, auth: &AuthToken) -> Result<Vec<Trade>> {
        let rows: Vec<AngelTradeData> = self
            .call(
                Method::GET,
                "/rest/secure/angelbroking/order/v1/getTradeBook",
                auth,
                None,
            )
            .await?
            .unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|t| Trade {
                symbol: self.oa_symbol(&t.symboltoken, &t.tradingsymbol, &t.exchange),
                product: oa_product(&t.exchange, &t.producttype),
                quantity: clamp(if t.fillsize != 0 {
                    t.fillsize
                } else {
                    t.quantity
                }),
                order_id: t.orderid,
                trade_id: t.fillid,
                exchange: t.exchange,
                side: t.transactiontype,
                average_price: t.fillprice,
                trade_value: t.tradevalue,
                timestamp: t.filltime,
            })
            .collect())
    }

    async fn get_positions(&self, auth: &AuthToken) -> Result<Vec<Position>> {
        let rows: Vec<AngelPositionData> = self
            .call(
                Method::GET,
                "/rest/secure/angelbroking/order/v1/getPosition",
                auth,
                None,
            )
            .await?
            .unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|p| Position {
                symbol: self.oa_symbol(&p.symboltoken, &p.tradingsymbol, &p.exchange),
                product: oa_product(&p.exchange, &p.producttype),
                exchange: p.exchange,
                quantity: clamp(p.netqty),
                overnight_quantity: clamp(p.cfbuyqty),
                average_price: p.avgnetprice,
                ltp: p.ltp,
                pnl: p.pnl,
                realized_pnl: p.realised,
                unrealized_pnl: p.unrealised,
                buy_quantity: clamp(p.buyqty),
                buy_value: p.buyamount,
                sell_quantity: clamp(p.sellqty),
                sell_value: p.sellamount,
            })
            .collect())
    }

    async fn get_holdings(&self, auth: &AuthToken) -> Result<Vec<Holding>> {
        let data: AngelHoldingsResponse = self
            .call(
                Method::GET,
                "/rest/secure/angelbroking/portfolio/v1/getAllHolding",
                auth,
                None,
            )
            .await?
            .unwrap_or_default();
        Ok(data
            .holdings
            .unwrap_or_default()
            .into_iter()
            .map(|h| Holding {
                symbol: self.symbols.oa_symbol_or_raw(&h.tradingsymbol, &h.exchange),
                exchange: h.exchange,
                product: "CNC".into(),
                isin: (!h.isin.is_empty()).then_some(h.isin),
                quantity: clamp(h.quantity),
                t1_quantity: clamp(h.t1quantity),
                average_price: h.averageprice,
                ltp: h.ltp,
                close_price: h.close,
                pnl: h.profitandloss,
                pnl_percentage: h.pnlpercentage,
                current_value: h.quantity as f64 * h.ltp,
            })
            .collect())
    }

    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds> {
        let d: AngelFundsData = self
            .call(
                Method::GET,
                "/rest/secure/angelbroking/user/v1/getRMS",
                auth,
                None,
            )
            .await?
            .unwrap_or_default();
        // web api/funds.py: raw availablecash is net margin; collateral is
        // availablecash - utilisedpayout; free cash = net + debits - collateral.
        let collateral = d.availablecash - d.utilisedpayout;
        let mut funds = Funds {
            available_cash: d.availablecash + d.utiliseddebits - collateral,
            used_margin: d.utilisedmargin,
            total_margin: d.net,
            opening_balance: d.availableintradaypayin,
            payin: d.availableintradaypayin,
            payout: d.utilisedpayout,
            span: d.utilisedspan,
            exposure: d.utilisedexposure,
            collateral,
            m2m_unrealized: 0.0,
            m2m_realized: 0.0,
            utilised_debits: d.utiliseddebits,
        };
        // P&L from the position book, as on the web (best effort).
        match self.get_positions(auth).await {
            Ok(ps) => {
                for p in ps {
                    if p.quantity == 0 {
                        funds.m2m_realized += p.pnl;
                    } else {
                        funds.m2m_unrealized += p.pnl;
                    }
                }
            }
            Err(e) => tracing::warn!("Angel One position P&L for funds failed: {}", e.code()),
        }
        Ok(funds)
    }

    async fn get_quote(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Quote> {
        let row = self.lookup(key)?;
        let rows = vec![(key.clone(), row.clone())];
        let fetched = self.fetch_quotes(auth, &rows).await?;
        let q = fetched
            .into_iter()
            .find(|q| q.symbolToken == row.token)
            .ok_or_else(|| {
                AppError::Broker(format!(
                    "Angel One returned no quote for {} {}.",
                    key.exchange, key.symbol
                ))
            })?;
        Ok(to_quote(key, &q))
    }

    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth> {
        let row = self.lookup(key)?;
        let fetched = self
            .fetch_quotes(auth, &[(key.clone(), row.clone())])
            .await?;
        let q = fetched
            .into_iter()
            .find(|q| q.symbolToken == row.token)
            .ok_or_else(|| {
                AppError::Broker(format!(
                    "Angel One returned no market depth for {} {}.",
                    key.exchange, key.symbol
                ))
            })?;
        let d = q.depth.clone().unwrap_or_default();
        let pad = |side: &[AngelDepthLevel]| -> Vec<DepthLevel> {
            (0..5)
                .map(|i| {
                    side.get(i)
                        .map(|l| DepthLevel {
                            price: l.price,
                            quantity: l.quantity,
                            orders: l.orders,
                        })
                        .unwrap_or_default()
                })
                .collect()
        };
        Ok(MarketDepth {
            symbol: key.symbol.clone(),
            exchange: key.exchange.clone(),
            bids: pad(&d.buy),
            asks: pad(&d.sell),
            ltp: q.ltp,
            ltq: q.lastTradeQty,
            open: q.open,
            high: q.high,
            low: q.low,
            prev_close: q.close,
            volume: q.tradeVolume,
            oi: q.opnInterest,
            total_buy_qty: q.totBuyQuan,
            total_sell_qty: q.totSellQuan,
        })
    }

    async fn get_history(&self, _auth: &AuthToken, _req: &HistoryRequest) -> Result<Vec<Candle>> {
        // next wave: getCandleData + getOIData with per-interval chunking.
        Err(AppError::Unsupported("history"))
    }

    async fn download_master_contract(&self, _auth: &AuthToken) -> Result<Vec<SymbolData>> {
        let resp = self
            .http
            .get(MASTER_CONTRACT_URL)
            .timeout(http::DOWNLOAD_TIMEOUT)
            .send()
            .await?;
        let (_, rows): (_, Vec<AngelSymbolData>) = http::read_json("angel", resp).await?;
        Ok(rows.into_iter().map(process_angel_symbol).collect())
    }
}

fn to_quote(key: &QuoteKey, q: &AngelQuoteData) -> Quote {
    let d = q.depth.clone().unwrap_or_default();
    let bid = d.buy.first().cloned().unwrap_or_default();
    let ask = d.sell.first().cloned().unwrap_or_default();
    let (change, change_percent) = if q.close > 0.0 {
        (q.ltp - q.close, (q.ltp - q.close) / q.close * 100.0)
    } else {
        (0.0, 0.0)
    };
    Quote {
        symbol: key.symbol.clone(),
        exchange: key.exchange.clone(),
        ltp: q.ltp,
        open: q.open,
        high: q.high,
        low: q.low,
        close: q.close,
        volume: q.tradeVolume,
        bid: bid.price,
        ask: ask.price,
        bid_qty: bid.quantity,
        ask_qty: ask.quantity,
        oi: q.opnInterest,
        change,
        change_percent,
        timestamp: String::new(),
    }
}

/// Index symbol from the scrip `name` (web: upper, strip spaces/hyphens,
/// BSE also strips `S&P `), then the override table.
fn index_symbol(name: &str, exchange: &str) -> String {
    let mut s = name.to_uppercase();
    if exchange == "BSE_INDEX" {
        s = s.replace("S&P ", "");
    }
    let s = s.replace([' ', '-'], "");
    match s.as_str() {
        "NIFTY50" => "NIFTY".into(),
        "NIFTYBANK" => "BANKNIFTY".into(),
        "NIFTYFINSERVICE" => "FINNIFTY".into(),
        "NIFTYNEXT50" => "NIFTYNXT50".into(),
        "NIFTYMIDSELECT" | "NIFTYMIDCAPSELECT" => "MIDCPNIFTY".into(),
        "SNSX50" => "SENSEX50".into(),
        _ => s,
    }
}

/// web `process_angel_json` for one row.
fn process_angel_symbol(s: AngelSymbolData) -> SymToken {
    let brexchange = s.exch_seg.clone();
    let brsymbol = s.symbol.clone();
    let mut exchange = s.exch_seg.clone();
    let it = s.instrumenttype.as_str();
    if it == "AMXIDX" {
        exchange = match exchange.as_str() {
            "NSE" => "NSE_INDEX".into(),
            "BSE" => "BSE_INDEX".into(),
            "MCX" => "MCX_INDEX".into(),
            other => other.into(),
        };
    }
    let mut strike = s.strike / 100.0;
    if exchange == "CDS" && matches!(it, "OPTCUR" | "OPTIRC") {
        strike /= 100_000.0;
    }
    let expiry = if s.expiry.is_empty() {
        String::new()
    } else {
        NaiveDateExt::angel_expiry(&s.expiry)
    };
    let mut symbol = ["-EQ", "-BE", "-MF", "-SG"]
        .iter()
        .fold(brsymbol.clone(), |acc, suf| acc.replace(suf, ""));
    let ex = exchange.as_str();
    let opt = if brsymbol.ends_with("CE") { "CE" } else { "PE" };
    if matches!(
        (it, ex),
        ("FUTCUR" | "FUTIRC", "CDS") | ("FUTCOM", "MCX") | ("FUTIDX" | "FUTSTK", "BFO")
    ) {
        symbol = format!("{}{}FUT", s.name, expiry_compact(&expiry));
    } else if matches!(
        (it, ex),
        ("OPTCUR" | "OPTIRC", "CDS") | ("OPTFUT", "MCX") | ("OPTIDX" | "OPTSTK", "BFO")
    ) {
        symbol = format!(
            "{}{}{}{}",
            s.name,
            expiry_compact(&expiry),
            format_strike(strike),
            opt
        );
    }
    if exchange == "NSE_INDEX" || exchange == "BSE_INDEX" {
        symbol = index_symbol(&s.name, &exchange);
    }
    let instrument_type = match it {
        "OPTIDX" | "OPTSTK" | "OPTFUT" | "OPTCUR" | "OPTIRC" if brsymbol.ends_with("CE") => {
            "CE".into()
        }
        "OPTIDX" | "OPTSTK" | "OPTFUT" | "OPTCUR" | "OPTIRC" if brsymbol.ends_with("PE") => {
            "PE".into()
        }
        "FUTIDX" | "FUTSTK" | "FUTCOM" | "FUTCUR" | "FUTIRC" | "FUTIRT" => "FUT".into(),
        other => other.to_string(),
    };
    SymToken {
        symbol,
        brsymbol,
        name: s.name,
        exchange,
        brexchange,
        token: s.token,
        expiry,
        strike,
        lot_size: i32::try_from(s.lotsize).unwrap_or(1),
        instrument_type,
        tick_size: s.tick_size / 100.0,
    }
}

/// Angel expiry `19MAR2024` -> `19-MAR-24`; anything else uppercased.
struct NaiveDateExt;

impl NaiveDateExt {
    fn angel_expiry(s: &str) -> String {
        match chrono::NaiveDate::parse_from_str(&s.to_ascii_uppercase(), "%d%b%Y") {
            Ok(d) => format_expiry(d),
            Err(_) => parse_broker_expiry(s)
                .map(format_expiry)
                .unwrap_or_else(|| s.to_ascii_uppercase()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::common::symbols::tests::row;

    fn sym(json: &str) -> SymToken {
        process_angel_symbol(serde_json::from_str(json).unwrap())
    }

    #[test]
    fn bfo_and_mcx_derivatives_are_rebuilt_nfo_kept() {
        let f = sym(
            r#"{"token":"1","symbol":"SENSEX25OCTFUT","name":"SENSEX","exch_seg":"BFO","instrumenttype":"FUTIDX","lotsize":"20","tick_size":"5","expiry":"30OCT2025","strike":"-1"}"#,
        );
        assert_eq!(f.symbol, "SENSEX30OCT25FUT");
        assert_eq!(f.instrument_type, "FUT");
        assert_eq!(f.expiry, "30-OCT-25");
        let o = sym(
            r#"{"token":"2","symbol":"CRUDEOIL25NOV5000CE","name":"CRUDEOIL","exch_seg":"MCX","instrumenttype":"OPTFUT","lotsize":"100","tick_size":"10","expiry":"17NOV2025","strike":"500000"}"#,
        );
        assert_eq!(o.symbol, "CRUDEOIL17NOV255000CE");
        assert_eq!(o.instrument_type, "CE");
        assert_eq!(o.tick_size, 0.1);
        let n = sym(
            r#"{"token":"3","symbol":"NIFTY30OCT2525000CE","name":"NIFTY","exch_seg":"NFO","instrumenttype":"OPTIDX","lotsize":"75","tick_size":"5","expiry":"30OCT2025","strike":"2500000"}"#,
        );
        assert_eq!(n.symbol, "NIFTY30OCT2525000CE");
        assert_eq!(n.strike, 25000.0);
        let eq = sym(
            r#"{"token":"3045","symbol":"SBIN-EQ","name":"SBIN","exch_seg":"NSE","instrumenttype":"","lotsize":"1","tick_size":"5","expiry":"","strike":"-1"}"#,
        );
        assert_eq!(
            (eq.symbol.as_str(), eq.brsymbol.as_str()),
            ("SBIN", "SBIN-EQ")
        );
        assert_eq!(eq.expiry, "");
    }

    #[test]
    fn index_symbols_derive_from_name() {
        let i = sym(
            r#"{"token":"99926000","symbol":"Nifty 50","name":"NIFTY","exch_seg":"NSE","instrumenttype":"AMXIDX","lotsize":"1","tick_size":"0","expiry":"","strike":"0"}"#,
        );
        assert_eq!(
            (i.exchange.as_str(), i.symbol.as_str()),
            ("NSE_INDEX", "NIFTY")
        );
        assert_eq!(index_symbol("Nifty IT", "NSE_INDEX"), "NIFTYIT");
        assert_eq!(index_symbol("S&P BSE SENSEX", "BSE_INDEX"), "BSESENSEX");
        assert_eq!(index_symbol("NIFTY MID SELECT", "NSE_INDEX"), "MIDCPNIFTY");
    }

    #[test]
    fn cds_option_strike_matches_web() {
        let c = sym(
            r#"{"token":"5","symbol":"USDINR25OCT8325CE","name":"USDINR","exch_seg":"CDS","instrumenttype":"OPTCUR","lotsize":"1","tick_size":"0.25","expiry":"29OCT2025","strike":"832500000.000000"}"#,
        );
        assert_eq!(c.strike, 83.25);
        assert_eq!(c.symbol, "USDINR29OCT2583.25CE");
    }

    #[test]
    fn order_bodies_send_broker_symbol_token_and_mapped_types() {
        let r = SymbolResolver::new();
        let mut sbin = row("SBIN", "SBIN-EQ", "NSE", "3045");
        sbin.brexchange = "NSE".into();
        r.load(vec![sbin]);
        let req = OrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            side: "BUY".into(),
            quantity: 5,
            price: 0.0,
            order_type: "SL-M".into(),
            product: "CNC".into(),
            validity: "DAY".into(),
            trigger_price: Some(900.5),
            disclosed_quantity: None,
            amo: false,
        };
        let o = ResolvedOrder::resolve(&req, &r).unwrap();
        let b = place_body(&o);
        assert_eq!(b["tradingsymbol"], "SBIN-EQ");
        assert_eq!(b["symboltoken"], "3045");
        assert_eq!(b["variety"], "STOPLOSS");
        assert_eq!(b["ordertype"], "STOPLOSS_MARKET");
        assert_eq!(b["producttype"], "DELIVERY");
        assert_eq!(b["triggerprice"], "900.5");
        assert_eq!(b["stoploss"], "900.5");
        assert_eq!(b["price"], "0");
        let m = ModifyOrderRequest {
            symbol: "SBIN".into(),
            exchange: "NSE".into(),
            action: "BUY".into(),
            product: "MIS".into(),
            pricetype: "SL".into(),
            quantity: 5,
            price: 905.0,
            trigger_price: 900.0,
            disclosed_quantity: 0,
        };
        let mb = modify_body(&ResolvedModify::resolve("42", &m, &r).unwrap());
        assert_eq!(mb["tradingsymbol"], "SBIN-EQ");
        assert_eq!(mb["symboltoken"], "3045");
        assert_eq!(mb["exchange"], "NSE");
        assert_eq!(mb["producttype"], "INTRADAY");
        assert_eq!(mb["variety"], "STOPLOSS");
        assert_eq!(mb["ordertype"], "STOPLOSS_LIMIT");
    }

    #[test]
    fn book_symbols_map_back_by_token() {
        let r = SymbolResolver::new();
        r.load(vec![row("SBIN", "SBIN-EQ", "NSE", "3045")]);
        let b = AngelBroker::new(r);
        assert_eq!(b.oa_symbol("3045", "SBIN-EQ", "NSE"), "SBIN");
        assert_eq!(b.oa_symbol("", "SBIN-EQ", "NSE"), "SBIN");
        assert_eq!(b.oa_symbol("1", "XYZ-EQ", "NSE"), "XYZ-EQ");
        assert_eq!(oa_product("NFO", "CARRYFORWARD"), "NRML");
        assert_eq!(oa_pricetype("STOPLOSS_LIMIT"), "SL");
        assert_eq!(quote_exchange("BSE_INDEX"), "BSE");
    }

    #[test]
    fn x_private_key_is_sent_on_secure_calls() {
        let b = AngelBroker::new(SymbolResolver::new());
        let req = b
            .request(Method::GET, "/x", "myapikey", Some("jwt"))
            .build()
            .unwrap();
        assert_eq!(req.headers()["X-PrivateKey"], "myapikey");
        assert_eq!(req.headers()["Authorization"], "Bearer jwt");
        let tok = AuthToken::new("myapikey:eyJhbGciOi.jwt.sig");
        assert_eq!(tok.pair(), Some(("myapikey", "eyJhbGciOi.jwt.sig")));
    }
}
