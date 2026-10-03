//! GTT rules (web `api/gtt_api.py`, `mapping/gtt_data.py`).
//!
//! * Angel has no OCO rule: an OpenAlgo OCO is two independent rules, the
//!   stop-loss leg then the target leg, returned as one composite trigger id
//!   `"<sl_rule_id>-<tg_rule_id>"`. A failed second leg rolls the first back.
//! * A rule always fires a LIMIT child order, so a MARKET request becomes a
//!   Market-Price-Protected LIMIT (SINGLE around LTP, OCO around each leg's
//!   trigger).
//! * `cancelRule` needs the token and exchange, so each rule is read back
//!   through `ruleDetails` first.
//! * The book asks only for statuses that can still fire.

use super::mapping::{map_product_type, num, reverse_map_product_type};
use super::{angel_error, AngelBroker, Category};
use crate::brokers::common::de::string_lenient;
use crate::brokers::common::mapping::PriceType;
use crate::brokers::common::mpp::{instrument_type_from_symbol, protected_price};
use crate::brokers::common::symbols::{SymToken, SymbolResolver};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{json, Value};

pub const GTT_BASE: &str = "/rest/secure/angelbroking/gtt/v1";
/// Rule validity in days sent with every create/modify (web
/// `DEFAULT_TIME_PERIOD_DAYS`).
pub const TIME_PERIOD_DAYS: i64 = 365;
/// Statuses that can still fire (web `ACTIVE_GTT_STATUSES`).
pub const ACTIVE_STATUSES: &[&str] = &["NEW", "ACTIVE", "SENTTOEXCHANGE"];
const PAGE_SIZE: usize = 10;
const MAX_PAGES: u32 = 25;
const DELIMITER: char = '-';

/// web `map_gtt_product_type`: GTT accepts only DELIVERY and MARGIN.
pub fn gtt_product(product: &str) -> &'static str {
    match map_product_type(product) {
        "CARRYFORWARD" | "INTRADAY" => "MARGIN",
        other => other,
    }
}

/// web `reverse_map_gtt_product_type`.
pub fn reverse_gtt_product(producttype: &str) -> String {
    let p = producttype.to_ascii_uppercase();
    if p == "MARGIN" {
        return "NRML".into();
    }
    reverse_map_product_type(&p).unwrap_or("CNC").to_string()
}

/// web `_encode_trigger_id`.
pub fn encode_trigger_id(ids: &[String]) -> String {
    ids.join("-")
}

/// web `_decode_trigger_id`.
pub fn decode_trigger_id(trigger_id: &str) -> Vec<String> {
    trigger_id
        .split(DELIMITER)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// web `_apply_mpp_if_market`.
pub fn apply_mpp(req: &GttRequest, row: &SymToken, last_price: f64) -> GttRequest {
    let mut r = req.clone();
    if r.pricetype != PriceType::Market {
        return r;
    }
    let it = if row.instrument_type.is_empty() {
        instrument_type_from_symbol(&row.symbol).to_string()
    } else {
        row.instrument_type.clone()
    };
    let tick = (row.tick_size > 0.0).then_some(row.tick_size);
    match r.trigger_type {
        GttTriggerType::Oco => {
            if r.triggerprice_sl > 0.0 {
                r.stoploss = protected_price(r.triggerprice_sl, r.action, &it, tick);
            }
            if r.triggerprice_tg > 0.0 {
                r.target = protected_price(r.triggerprice_tg, r.action, &it, tick);
            }
        }
        GttTriggerType::Single => {
            if last_price > 0.0 {
                r.price = protected_price(last_price, r.action, &it, tick);
            }
        }
    }
    r.pricetype = PriceType::Limit;
    r
}

/// web `_legs`: `(label, trigger, limit)` per Angel rule.
pub fn legs(req: &GttRequest) -> Vec<(&'static str, f64, f64)> {
    match req.trigger_type {
        GttTriggerType::Oco => vec![
            ("SL", req.triggerprice_sl, req.stoploss),
            ("TG", req.triggerprice_tg, req.target),
        ],
        GttTriggerType::Single => {
            let trigger = if req.trigger_price != 0.0 {
                req.trigger_price
            } else if req.triggerprice_sl > 0.0 {
                req.triggerprice_sl
            } else {
                req.triggerprice_tg
            };
            vec![("SINGLE", trigger, req.price)]
        }
    }
}

/// web `transform_place_gtt`: one `createRule` body per leg.
pub fn create_bodies(req: &GttRequest, row: &SymToken) -> Vec<(&'static str, Value)> {
    legs(req)
        .into_iter()
        .map(|(label, trigger, price)| {
            (
                label,
                json!({
                    "tradingsymbol": row.br_symbol(),
                    "symboltoken": row.token,
                    "exchange": req.key.exchange,
                    "transactiontype": req.action.as_str(),
                    "producttype": gtt_product(req.product.as_str()),
                    "price": num(price),
                    "qty": req.quantity.to_string(),
                    "triggerprice": num(trigger),
                    "disclosedqty": "0",
                    "timeperiod": TIME_PERIOD_DAYS,
                }),
            )
        })
        .collect()
}

/// web `transform_modify_gtt`: one `modifyRule` body per existing rule id.
pub fn modify_bodies(
    req: &GttRequest,
    token: &str,
    rule_ids: &[String],
) -> Vec<(&'static str, Value)> {
    legs(req)
        .into_iter()
        .zip(rule_ids)
        .map(|((label, trigger, price), id)| {
            (
                label,
                json!({
                    "id": id,
                    "symboltoken": token,
                    "exchange": req.key.exchange,
                    "price": num(price),
                    "qty": req.quantity.to_string(),
                    "triggerprice": num(trigger),
                    "disclosedqty": "0",
                    "timeperiod": TIME_PERIOD_DAYS,
                }),
            )
        })
        .collect()
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AngelRule {
    #[serde(deserialize_with = "string_lenient")]
    pub id: String,
    #[serde(deserialize_with = "string_lenient")]
    pub gttid: String,
    #[serde(deserialize_with = "string_lenient")]
    pub ruleid: String,
    #[serde(deserialize_with = "string_lenient")]
    pub status: String,
    #[serde(deserialize_with = "string_lenient")]
    pub tradingsymbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub symboltoken: String,
    #[serde(deserialize_with = "string_lenient")]
    pub exchange: String,
    #[serde(deserialize_with = "string_lenient")]
    pub transactiontype: String,
    #[serde(deserialize_with = "string_lenient")]
    pub producttype: String,
    #[serde(deserialize_with = "crate::brokers::common::de::f64_lenient")]
    pub price: f64,
    #[serde(deserialize_with = "crate::brokers::common::de::f64_lenient")]
    pub qty: f64,
    #[serde(deserialize_with = "crate::brokers::common::de::f64_lenient")]
    pub triggerprice: f64,
    #[serde(deserialize_with = "string_lenient")]
    pub createddate: String,
    #[serde(deserialize_with = "string_lenient")]
    pub updateddate: String,
    #[serde(deserialize_with = "string_lenient")]
    pub expirydate: String,
}

/// web `map_gtt_book`: active rules only, one single-trigger row each.
pub fn map_gtt_book(rules: Vec<AngelRule>, symbols: &SymbolResolver) -> Vec<GttOrder> {
    rules
        .into_iter()
        .filter_map(|r| {
            let status = r.status.to_ascii_uppercase();
            if !ACTIVE_STATUSES.contains(&status.as_str()) {
                return None;
            }
            let symbol = if r.tradingsymbol.is_empty() || r.exchange.is_empty() {
                r.tradingsymbol.clone()
            } else {
                symbols.oa_symbol_or_raw(&r.tradingsymbol, &r.exchange)
            };
            let trigger_id = [r.id, r.gttid, r.ruleid]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or_default();
            Some(GttOrder {
                trigger_id,
                trigger_type: "single".into(),
                status: "active".into(),
                symbol,
                exchange: r.exchange,
                trigger_prices: vec![r.triggerprice],
                last_price: 0.0,
                legs: vec![GttLeg {
                    action: r.transactiontype.to_ascii_uppercase(),
                    quantity: r.qty as i64,
                    price: r.price,
                    pricetype: "LIMIT".into(),
                    product: reverse_gtt_product(&r.producttype),
                }],
                created_at: r.createddate,
                updated_at: r.updateddate,
                expires_at: r.expirydate,
            })
        })
        .collect()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RuleId {
    #[serde(deserialize_with = "string_lenient")]
    id: String,
}

async fn post<T: serde::de::DeserializeOwned>(
    b: &AngelBroker,
    auth: &AuthToken,
    path: &str,
    body: &Value,
    cat: Category,
) -> Result<Option<T>> {
    b.call(
        Method::POST,
        &format!("{}/{}", GTT_BASE, path),
        auth,
        Some(body),
        cat,
    )
    .await
}

async fn rule_details(b: &AngelBroker, auth: &AuthToken, id: &str) -> Result<AngelRule> {
    post::<AngelRule>(b, auth, "ruleDetails", &json!({"id": id}), Category::Other)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Could not fetch Angel One GTT rule {}", id)))
}

async fn cancel_rule(
    b: &AngelBroker,
    auth: &AuthToken,
    id: &str,
    token: &str,
    exchange: &str,
) -> Result<String> {
    let body = json!({"id": id, "symboltoken": token, "exchange": exchange});
    let r: Option<RuleId> = post(b, auth, "cancelRule", &body, Category::Order).await?;
    Ok(r.map(|r| r.id)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| id.to_string()))
}

/// web `_prepare_market`: LTP for a MARKET SINGLE, then MPP.
async fn prepare(b: &AngelBroker, auth: &AuthToken, req: &GttRequest) -> Result<GttRequest> {
    if req.pricetype != PriceType::Market {
        return Ok(req.clone());
    }
    let row = b.lookup(&req.key)?;
    let mut lp = req.last_price.unwrap_or(0.0);
    if req.trigger_type == GttTriggerType::Single && lp <= 0.0 {
        lp = super::data::get_quote(b, auth, &req.key)
            .await
            .map(|q| q.ltp)
            .unwrap_or(0.0);
        if lp <= 0.0 {
            return Err(AppError::Broker(
                "Could not fetch the last price from Angel One to place the GTT. Try again.".into(),
            ));
        }
    }
    Ok(apply_mpp(req, &row, lp))
}

pub async fn place_gtt(b: &AngelBroker, auth: &AuthToken, req: &GttRequest) -> Result<GttResponse> {
    let req = prepare(b, auth, req).await?;
    let row = b.lookup(&req.key)?;
    let mut created: Vec<String> = Vec::new();
    for (label, body) in create_bodies(&req, &row) {
        let path = format!("{}/createRule", GTT_BASE);
        let outcome = b
            .call_env::<RuleId>(Method::POST, &path, auth, Some(&body), Category::Order)
            .await;
        let failure = match outcome {
            Ok(env) if env.status => match env.data.map(|d| d.id).filter(|s| !s.is_empty()) {
                Some(id) => {
                    created.push(id);
                    continue;
                }
                None => AppError::Broker(format!(
                    "Angel One returned no rule id for the {} leg.",
                    label
                )),
            },
            Ok(env) => angel_error(&env.errorcode, &env.message),
            Err(e) => e,
        };
        // Partial OCO: roll the created leg back so no orphan trigger stays.
        for id in &created {
            tracing::warn!(
                "Angel One GTT: rolling back rule {} after {} leg failed",
                id,
                label
            );
            if let Err(e) = cancel_rule(b, auth, id, &row.token, &req.key.exchange).await {
                tracing::error!("Angel One GTT rollback of rule {} failed: {}", id, e.code());
            }
        }
        return Err(failure);
    }
    Ok(GttResponse {
        trigger_id: encode_trigger_id(&created),
    })
}

pub async fn modify_gtt(
    b: &AngelBroker,
    auth: &AuthToken,
    trigger_id: &str,
    req: &GttRequest,
) -> Result<GttResponse> {
    let ids = decode_trigger_id(trigger_id);
    if ids.is_empty() {
        return Err(AppError::Validation("trigger_id is required".into()));
    }
    let expected = if req.trigger_type == GttTriggerType::Oco {
        2
    } else {
        1
    };
    if ids.len() != expected {
        return Err(AppError::Validation(format!(
            "This GTT has {} Angel One rule(s) but a {} GTT needs {}. Angel One cannot convert between single and OCO; cancel it and place a new one.",
            ids.len(),
            if expected == 2 { "OCO" } else { "single" },
            expected
        )));
    }
    let req = prepare(b, auth, req).await?;
    let token = match b.resolver().token(&req.key.symbol, &req.key.exchange) {
        Some(t) if !t.is_empty() => t,
        _ => rule_details(b, auth, &ids[0]).await?.symboltoken,
    };
    let mut modified = Vec::new();
    for (label, body) in modify_bodies(&req, &token, &ids) {
        let r: Option<RuleId> = post(b, auth, "modifyRule", &body, Category::Order)
            .await
            .map_err(|e| match e {
                AppError::Broker(m) => AppError::Broker(format!("{} leg: {}", label, m)),
                other => other,
            })?;
        let fallback = body["id"].as_str().unwrap_or_default().to_string();
        modified.push(
            r.map(|r| r.id)
                .filter(|s| !s.is_empty())
                .unwrap_or(fallback),
        );
    }
    Ok(GttResponse {
        trigger_id: encode_trigger_id(&modified),
    })
}

pub async fn cancel_gtt(
    b: &AngelBroker,
    auth: &AuthToken,
    trigger_id: &str,
) -> Result<GttResponse> {
    let ids = decode_trigger_id(trigger_id);
    if ids.is_empty() {
        return Err(AppError::Validation("trigger_id is required".into()));
    }
    let mut cancelled: Vec<String> = Vec::new();
    for id in &ids {
        let done = async {
            let rule = rule_details(b, auth, id).await?;
            cancel_rule(b, auth, id, &rule.symboltoken, &rule.exchange).await
        }
        .await;
        match done {
            Ok(c) => cancelled.push(c),
            Err(e) if cancelled.is_empty() => return Err(e),
            Err(e) => {
                return Err(AppError::Broker(format!(
                    "{} (already cancelled: {})",
                    e.client_message(),
                    cancelled.join(", ")
                )))
            }
        }
    }
    Ok(GttResponse {
        trigger_id: encode_trigger_id(&cancelled),
    })
}

/// web `get_gtt_book`: `ruleList` paged by 10 up to 25 pages, active
/// statuses only. `include_history` is accepted for parity and ignored, as
/// on the web.
pub async fn get_gtt_book(
    b: &AngelBroker,
    auth: &AuthToken,
    _include_history: bool,
) -> Result<Vec<GttOrder>> {
    let mut rules: Vec<AngelRule> = Vec::new();
    for page in 1..=MAX_PAGES {
        let body = json!({"status": ACTIVE_STATUSES, "page": page, "count": PAGE_SIZE});
        let data: Option<Value> = match post(b, auth, "ruleList", &body, Category::Other).await {
            Ok(d) => d,
            Err(e) if page > 1 && !rules.is_empty() => {
                tracing::warn!("Angel One GTT book page {} failed: {}", page, e.code());
                break;
            }
            Err(e) => return Err(e),
        };
        let page_rules: Vec<AngelRule> = match data {
            Some(Value::Array(a)) => a
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect(),
            Some(v @ Value::Object(_)) => serde_json::from_value(v).into_iter().collect(),
            _ => Vec::new(),
        };
        let n = page_rules.len();
        rules.extend(page_rules);
        if n < PAGE_SIZE {
            break;
        }
        if page == MAX_PAGES {
            tracing::warn!("Angel One GTT book stopped at the page ceiling; it may be truncated");
        }
    }
    Ok(map_gtt_book(rules, b.resolver()))
}
