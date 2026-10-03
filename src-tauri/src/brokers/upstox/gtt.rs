//! GTT orders, v3 (web `api/gtt_api.py`, `mapping/gtt_data.py`).
//!
//! Upstox GTT rules carry no limit price: the child order fires at the
//! rule's trigger price, widened by `market_protection` for a MARKET
//! request (Upstox refuses MARKET children, UDAPI1158). Every GTT needs an
//! ENTRY rule (UDAPI1141), so an OpenAlgo OCO becomes a MULTIPLE bracket
//! whose ENTRY fires immediately at the last price and opens the position
//! before the target / stop-loss pair is armed. That is logged at place
//! time, as on the web.

use super::mapping::{self, product_code};
use super::{Category, UpstoxBroker};
use crate::brokers::common::mapping::{Action, PriceType};
use crate::brokers::common::mpp::{instrument_type_from_symbol, mpp_percentage, py_round};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};

/// Rule statuses that can still fire (INACTIVE: a MULTIPLE's exit legs
/// before the ENTRY triggers).
const LIVE_RULE_STATUSES: &[&str] = &["SCHEDULED", "PENDING", "OPEN", "INACTIVE"];

fn lookup(b: &UpstoxBroker, key: &QuoteKey) -> Result<SymToken> {
    b.resolver().by_symbol(&key.exchange, &key.symbol).ok_or_else(|| {
        AppError::Validation(format!(
            "Symbol {} was not found on {}. Check the symbol, or download the master contract again from the broker page.",
            key.symbol, key.exchange
        ))
    })
}

/// `market_protection` for a MARKET request (web `_market_protection`): the
/// shared MPP slab percentage for the price, rounded and clamped to 1..=25.
pub fn market_protection(req: &GttRequest, row: &SymToken, base_price: f64) -> Option<i64> {
    if req.pricetype != PriceType::Market {
        return None;
    }
    let it = if row.instrument_type.is_empty() {
        instrument_type_from_symbol(&row.symbol).to_string()
    } else {
        row.instrument_type.clone()
    };
    let pct = mpp_percentage(base_price, &it);
    let pct = if pct > 0.0 { pct } else { 1.0 };
    Some((py_round(pct, 0) as i64).clamp(1, 25))
}

fn rule(strategy: &str, trigger_type: &str, trigger: f64, mp: Option<i64>) -> Value {
    let mut r = json!({
        "strategy": strategy,
        "trigger_type": trigger_type,
        "trigger_price": trigger,
    });
    if let Some(m) = mp {
        r["market_protection"] = json!(m);
    }
    r
}

/// SINGLE trigger: the legacy `trigger_price` when set, else the stop-loss
/// trigger, else the target trigger.
pub fn single_trigger(req: &GttRequest) -> f64 {
    if req.trigger_price != 0.0 {
        req.trigger_price
    } else if req.triggerprice_sl > 0.0 {
        req.triggerprice_sl
    } else {
        req.triggerprice_tg
    }
}

/// `BELOW` / `ABOVE` / `IMMEDIATE` against the last price; without one,
/// from which OpenAlgo field was set (web `_single_trigger_direction`).
pub fn single_direction(req: &GttRequest, trigger: f64, last_price: f64) -> &'static str {
    if last_price > 0.0 {
        if trigger < last_price {
            "BELOW"
        } else if trigger > last_price {
            "ABOVE"
        } else {
            "IMMEDIATE"
        }
    } else if req.triggerprice_tg > 0.0 {
        "ABOVE"
    } else {
        "BELOW"
    }
}

/// `(type, transaction_type, rules)` (web `_build_rules`).
pub fn build_rules(
    req: &GttRequest,
    row: &SymToken,
    last_price: f64,
) -> (&'static str, Action, Vec<Value>) {
    match req.trigger_type {
        GttTriggerType::Oco => {
            // OpenAlgo's action is the exit side; Upstox's is the entry side.
            let entry = req.action.opposite();
            let (target, stop) = match entry {
                Action::Buy => (req.triggerprice_tg, req.triggerprice_sl),
                Action::Sell => (req.triggerprice_sl, req.triggerprice_tg),
            };
            if req.stoploss != 0.0 || req.target != 0.0 {
                tracing::info!(
                    "Upstox GTT: stop-loss and target limit prices are not sent; each rule executes at its trigger price"
                );
            }
            let rules = vec![
                rule(
                    "ENTRY",
                    "IMMEDIATE",
                    last_price,
                    market_protection(req, row, last_price),
                ),
                rule(
                    "TARGET",
                    "IMMEDIATE",
                    target,
                    market_protection(req, row, target),
                ),
                rule(
                    "STOPLOSS",
                    "IMMEDIATE",
                    stop,
                    market_protection(req, row, stop),
                ),
            ];
            ("MULTIPLE", entry, rules)
        }
        GttTriggerType::Single => {
            let trigger = single_trigger(req);
            let dir = single_direction(req, trigger, last_price);
            if req.price > 0.0 && req.price != trigger {
                tracing::info!(
                    "Upstox GTT: limit price not sent; the rule executes at its trigger price"
                );
            }
            (
                "SINGLE",
                req.action,
                vec![rule(
                    "ENTRY",
                    dir,
                    trigger,
                    market_protection(req, row, trigger),
                )],
            )
        }
    }
}

/// `POST /v3/order/gtt/place` body (web `transform_place_gtt`).
pub fn place_body(req: &GttRequest, row: &SymToken, last_price: f64) -> Value {
    let (kind, side, rules) = build_rules(req, row, last_price);
    json!({
        "type": kind,
        "quantity": req.quantity,
        "product": product_code(req.product),
        "instrument_token": row.token,
        "transaction_type": side.as_str(),
        "rules": rules,
    })
}

/// `PUT /v3/order/gtt/modify` body (web `transform_modify_gtt`): no
/// instrument, product, side or `market_protection`.
pub fn modify_body(req: &GttRequest, row: &SymToken, last_price: f64, trigger_id: &str) -> Value {
    let (kind, _side, mut rules) = build_rules(req, row, last_price);
    for r in rules.iter_mut() {
        if let Some(o) = r.as_object_mut() {
            o.remove("market_protection");
        }
    }
    json!({
        "type": kind,
        "quantity": req.quantity,
        "gtt_order_id": trigger_id,
        "rules": rules,
    })
}

/// `gtt_order_ids[0]` or `gtt_order_id` of a GTT answer.
pub fn gtt_id(data: &Value) -> Option<String> {
    let text = |v: &Value| match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    data.get("gtt_order_ids")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(text)
        .or_else(|| data.get("gtt_order_id").and_then(text))
}

/// The last price, from the request or a quote. OCO needs one (it is the
/// ENTRY trigger); SINGLE falls back to field semantics without it.
async fn last_price(b: &UpstoxBroker, auth: &AuthToken, req: &GttRequest) -> Result<f64> {
    if let Some(lp) = req.last_price.filter(|p| *p > 0.0) {
        return Ok(lp);
    }
    match super::data::get_quote(b, auth, &req.key).await {
        Ok(q) if q.ltp > 0.0 => Ok(q.ltp),
        other => {
            if let Err(e) = other {
                tracing::warn!("Upstox GTT last price lookup failed: {}", e.code());
            }
            if req.trigger_type == GttTriggerType::Oco {
                Err(AppError::Broker(
                    "Could not fetch the last price from Upstox to place the GTT. Try again."
                        .into(),
                ))
            } else {
                Ok(0.0)
            }
        }
    }
}

pub async fn place_gtt(
    b: &UpstoxBroker,
    auth: &AuthToken,
    req: &GttRequest,
) -> Result<GttResponse> {
    let row = lookup(b, &req.key)?;
    let lp = last_price(b, auth, req).await?;
    if req.trigger_type == GttTriggerType::Oco {
        tracing::warn!(
            "Upstox GTT OCO is placed as a bracket: the entry leg opens the position at market before the target and stop-loss are set"
        );
    }
    let body = place_body(req, &row, lp);
    let data = b
        .call(
            Method::POST,
            &b.api("/v3/order/gtt/place"),
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    let trigger_id = gtt_id(&data).ok_or_else(|| {
        AppError::Broker(
            "Upstox accepted the GTT but did not return its id. Check the GTT book.".into(),
        )
    })?;
    Ok(GttResponse { trigger_id })
}

pub async fn modify_gtt(
    b: &UpstoxBroker,
    auth: &AuthToken,
    trigger_id: &str,
    req: &GttRequest,
) -> Result<GttResponse> {
    if trigger_id.trim().is_empty() {
        return Err(AppError::Validation("trigger_id is required".into()));
    }
    let row = lookup(b, &req.key)?;
    let lp = last_price(b, auth, req).await?;
    let body = modify_body(req, &row, lp, trigger_id);
    let data = b
        .call(
            Method::PUT,
            &b.api("/v3/order/gtt/modify"),
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    Ok(GttResponse {
        trigger_id: gtt_id(&data).unwrap_or_else(|| trigger_id.to_string()),
    })
}

/// Cancel is a DELETE carrying `{"gtt_order_id": ..}` in its body.
pub async fn cancel_gtt(
    b: &UpstoxBroker,
    auth: &AuthToken,
    trigger_id: &str,
) -> Result<GttResponse> {
    if trigger_id.trim().is_empty() {
        return Err(AppError::Validation("trigger_id is required".into()));
    }
    let body = json!({ "gtt_order_id": trigger_id });
    let data = b
        .call(
            Method::DELETE,
            &b.api("/v3/order/gtt/cancel"),
            auth,
            Some(&body),
            Category::Order,
        )
        .await?;
    Ok(GttResponse {
        trigger_id: gtt_id(&data).unwrap_or_else(|| trigger_id.to_string()),
    })
}

/// Epoch in any unit (s / ms / us / ns, inferred by magnitude; Upstox sends
/// microseconds) or an ISO string -> `YYYY-MM-DDTHH:MM:SSZ`.
pub fn iso_timestamp(v: &Value) -> String {
    let epoch = match v {
        Value::Null => return String::new(),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return String::new();
            }
            if !t
                .trim_start_matches('-')
                .chars()
                .all(|c| c.is_ascii_digit())
            {
                return t.to_string();
            }
            match t.parse::<f64>() {
                Ok(f) => f,
                Err(_) => return t.to_string(),
            }
        }
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        _ => return String::new(),
    };
    if epoch <= 0.0 {
        return String::new();
    }
    let secs = [(1e18, 1e9), (1e15, 1e6), (1e12, 1e3), (1e9, 1.0)]
        .iter()
        .find(|(threshold, _)| epoch >= *threshold)
        .map(|(_, div)| epoch / div);
    let Some(secs) = secs else {
        return String::new();
    };
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

/// GTT book (web `map_gtt_book`): live GTTs only, status `active`; a
/// MULTIPLE hides its mandatory ENTRY rule so it reads as an OCO pair
/// ordered low to high.
pub fn map_gtt_book(data: &Value, b: &UpstoxBroker) -> Vec<GttOrder> {
    let Some(entries) = data.as_array() else {
        return Vec::new();
    };
    let f = |v: &Value, k: &str| match v.get(k) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    };
    let s = |v: &Value, k: &str| match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let mut out = Vec::new();
    for g in entries {
        let rules: Vec<&Value> = g
            .get("rules")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter(|x| x.is_object()).collect())
            .unwrap_or_default();
        let live = rules
            .iter()
            .any(|r| LIVE_RULE_STATUSES.contains(&s(r, "status").to_ascii_uppercase().as_str()));
        if !live {
            continue;
        }
        let kind = {
            let k = s(g, "type").to_ascii_uppercase();
            if k.is_empty() {
                "SINGLE".to_string()
            } else {
                k
            }
        };
        let br_exchange = s(g, "exchange");
        let exchange = mapping::openalgo_exchange(&br_exchange).to_string();
        let br_symbol = s(g, "trading_symbol");
        let symbol = mapping::oa_symbol(
            b.resolver(),
            &s(g, "instrument_token"),
            &exchange,
            &br_symbol,
        );
        let quantity = f(g, "quantity") as i64;
        let product = mapping::reverse_product(&exchange, &s(g, "product"))
            .unwrap_or("")
            .to_string();
        let mut shown: Vec<&Value> = if kind == "MULTIPLE" {
            rules
                .iter()
                .copied()
                .filter(|r| !s(r, "strategy").eq_ignore_ascii_case("ENTRY"))
                .collect()
        } else {
            rules.clone()
        };
        if shown.is_empty() {
            shown = rules.clone();
        }
        shown.sort_by(|a, b| f(a, "trigger_price").total_cmp(&f(b, "trigger_price")));
        out.push(GttOrder {
            trigger_id: s(g, "gtt_order_id"),
            trigger_type: if kind == "MULTIPLE" {
                "two-leg".into()
            } else {
                "single".into()
            },
            status: "active".into(),
            symbol,
            exchange,
            trigger_prices: shown.iter().map(|r| f(r, "trigger_price")).collect(),
            last_price: 0.0,
            legs: shown
                .iter()
                .map(|r| GttLeg {
                    action: s(r, "transaction_type").to_ascii_uppercase(),
                    quantity,
                    price: f(r, "trigger_price"),
                    pricetype: "LIMIT".into(),
                    product: product.clone(),
                })
                .collect(),
            created_at: iso_timestamp(g.get("created_at").unwrap_or(&Value::Null)),
            updated_at: String::new(),
            expires_at: iso_timestamp(g.get("expires_at").unwrap_or(&Value::Null)),
        });
    }
    out
}

pub async fn get_gtt_book(
    b: &UpstoxBroker,
    auth: &AuthToken,
    _include_history: bool,
) -> Result<Vec<GttOrder>> {
    // The web accepts include_history for interface parity and still
    // returns live GTTs only.
    let data = b
        .call(
            Method::GET,
            &b.api("/v3/order/gtt"),
            auth,
            None,
            Category::Order,
        )
        .await?;
    Ok(map_gtt_book(&data, b))
}
