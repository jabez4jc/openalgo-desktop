//! Funds and margin (web `api/funds.py`, `api/margin_api.py`,
//! `mapping/margin_data.py`).
//!
//! Funds are cached for 60 s per session and a 429 starts a 30 s -> 60 s ->
//! 120 s backoff during which the cached figures are served (web
//! `CACHE_TTL`, `INITIAL_BACKOFF`, `MAX_BACKOFF`). The cache holds one entry
//! (the signed-in session) and is replaced, never grown.

use super::mapping::{self, FyersPosition};
use super::orders::raw_positions;
use super::{code_message, fyers_error, FyersBroker};
use crate::brokers::common::de::{f64_lenient, string_lenient};
use crate::brokers::types::*;
use crate::error::{AppError, Result};
use parking_lot::Mutex;
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;

pub const CACHE_TTL: Duration = Duration::from_secs(60);
pub const INITIAL_BACKOFF: Duration = Duration::from_secs(30);
pub const MAX_BACKOFF: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
struct Entry {
    session: [u8; 32],
    data: Option<(Instant, Funds)>,
    backoff_until: Option<Instant>,
    backoff: Duration,
}

/// One-entry funds cache keyed by a hash of the session token.
#[derive(Debug, Default)]
pub struct FundsCache {
    entry: Mutex<Option<Entry>>,
}

fn session_key(auth: &AuthToken) -> [u8; 32] {
    Sha256::digest(auth.raw().as_bytes()).into()
}

enum Cached {
    Fresh(Funds),
    /// Inside a 429 backoff: serve this (or fail when nothing is cached).
    Backoff(Option<Funds>),
    Miss,
}

impl FundsCache {
    fn lookup(&self, auth: &AuthToken, now: Instant) -> Cached {
        let key = session_key(auth);
        let guard = self.entry.lock();
        let Some(e) = guard.as_ref().filter(|e| e.session == key) else {
            return Cached::Miss;
        };
        if let Some((at, f)) = &e.data {
            if now.duration_since(*at) < CACHE_TTL {
                return Cached::Fresh(f.clone());
            }
        }
        if e.backoff_until.is_some_and(|t| now < t) {
            return Cached::Backoff(e.data.as_ref().map(|(_, f)| f.clone()));
        }
        Cached::Miss
    }

    fn store(&self, auth: &AuthToken, now: Instant, funds: &Funds) {
        *self.entry.lock() = Some(Entry {
            session: session_key(auth),
            data: Some((now, funds.clone())),
            backoff_until: None,
            backoff: Duration::ZERO,
        });
    }

    /// Start or lengthen the 429 backoff; returns cached funds if any.
    fn rate_limited(&self, auth: &AuthToken, now: Instant) -> Option<Funds> {
        let key = session_key(auth);
        let mut guard = self.entry.lock();
        if guard.as_ref().is_none_or(|e| e.session != key) {
            *guard = Some(Entry {
                session: key,
                data: None,
                backoff_until: None,
                backoff: Duration::ZERO,
            });
        }
        let e = guard.as_mut()?;
        e.backoff = if e.backoff.is_zero() {
            INITIAL_BACKOFF
        } else {
            (e.backoff * 2).min(MAX_BACKOFF)
        };
        e.backoff_until = Some(now + e.backoff);
        tracing::warn!("Fyers funds rate limited; backing off {:?}", e.backoff);
        e.data.as_ref().map(|(_, f)| f.clone())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FundLimit {
    #[serde(deserialize_with = "string_lenient")]
    pub title: String,
    #[serde(rename = "equityAmount", deserialize_with = "f64_lenient")]
    pub equity_amount: f64,
    #[serde(rename = "commodityAmount", deserialize_with = "f64_lenient")]
    pub commodity_amount: f64,
}

/// web `get_margin_data` from `fund_limit` rows (keyed by
/// `title.lower().replace(" ", "_")`): available cash is the settled
/// `clear_balance` (not `available_balance`, which folds collateral back in,
/// web issue #1582), collateral is `collaterals`, debits `utilized_amount`.
pub fn funds_from_limits(limits: &[FundLimit], positions: &[FyersPosition]) -> Funds {
    let mut by_key: HashMap<String, (f64, f64)> = HashMap::new();
    for f in limits {
        by_key.insert(
            f.title.to_lowercase().replace(' ', "_"),
            (f.equity_amount, f.commodity_amount),
        );
    }
    let total = |k: &str| by_key.get(k).map(|(e, c)| e + c).unwrap_or(0.0);
    let (realised, unrealised) = m2m(positions);
    Funds {
        available_cash: total("clear_balance"),
        used_margin: total("utilized_amount"),
        total_margin: total("total_balance"),
        opening_balance: total("limit_at_start_of_the_day"),
        payin: total("fund_transfer"),
        payout: 0.0,
        span: 0.0,
        exposure: 0.0,
        collateral: total("collaterals"),
        m2m_unrealized: unrealised,
        m2m_realized: realised,
        utilised_debits: total("utilized_amount"),
    }
}

/// Sum of `realized_profit` / `unrealized_profit` over the net positions.
pub fn m2m(positions: &[FyersPosition]) -> (f64, f64) {
    positions.iter().fold((0.0, 0.0), |(r, u), p| {
        (r + p.realized_profit, u + p.unrealized_profit)
    })
}

pub async fn get_funds(b: &FyersBroker, auth: &AuthToken) -> Result<Funds> {
    let now = Instant::now();
    match b.funds_cache.lookup(auth, now) {
        Cached::Fresh(f) => return Ok(f),
        Cached::Backoff(Some(f)) => return Ok(f),
        Cached::Backoff(None) => {
            return Err(AppError::Broker(
                "Fyers is limiting requests right now. Wait a moment and try again.".into(),
            ))
        }
        Cached::Miss => {}
    }
    let (status, v) = b.raw(Method::GET, "/api/v3/funds", auth, None).await?;
    if status == StatusCode::TOO_MANY_REQUESTS {
        return b.funds_cache.rate_limited(auth, now).ok_or_else(|| {
            AppError::Broker(
                "Fyers is limiting requests right now. Wait a moment and try again.".into(),
            )
        });
    }
    let (code, message) = code_message(&v);
    // web: anything but code 200 means no real funds (an expired session
    // must never read as a zero-balance account).
    if code != 200 {
        tracing::warn!(
            status = status.as_u16(),
            code,
            "Fyers funds refused: {}",
            message
        );
        return Err(fyers_error(status.as_u16(), code, &message));
    }
    let limits: Vec<FundLimit> = mapping::rows(&v, "fund_limit");
    let positions = raw_positions(b, auth).await?;
    let funds = funds_from_limits(&limits, &positions);
    b.funds_cache.store(auth, now, &funds);
    Ok(funds)
}

pub async fn calculate_margin(
    b: &FyersBroker,
    auth: &AuthToken,
    legs: &[MarginLeg],
) -> Result<MarginResult> {
    let mut data = Vec::with_capacity(legs.len());
    for leg in legs {
        match mapping::margin_leg(leg, b.resolver()) {
            Some(v) => data.push(v),
            None => tracing::warn!(
                "Margin leg skipped, symbol not found: {} ({})",
                leg.key.symbol,
                leg.key.exchange
            ),
        }
    }
    if data.is_empty() {
        return Err(AppError::Validation(
            "No valid positions to calculate margin. Check if symbols are valid.".into(),
        ));
    }
    let body = json!({ "data": Value::Array(data) });
    let v = b
        .call(Method::POST, "/api/v3/multiorder/margin", auth, Some(&body))
        .await?;
    Ok(mapping::parse_margin(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn cache_serves_fresh_then_expires_and_backs_off() {
        let c = FundsCache::default();
        let a = AuthToken::new("APP:tok");
        let other = AuthToken::new("APP:tok2");
        let t0 = Instant::now();
        assert!(matches!(c.lookup(&a, t0), Cached::Miss));
        let f = Funds {
            available_cash: 10.0,
            ..Default::default()
        };
        c.store(&a, t0, &f);
        assert!(matches!(
            c.lookup(&a, t0 + Duration::from_secs(59)),
            Cached::Fresh(_)
        ));
        assert!(matches!(c.lookup(&other, t0), Cached::Miss));
        let t1 = t0 + Duration::from_secs(61);
        assert!(matches!(c.lookup(&a, t1), Cached::Miss));
        assert_eq!(c.rate_limited(&a, t1).unwrap().available_cash, 10.0);
        assert!(matches!(
            c.lookup(&a, t1 + Duration::from_secs(29)),
            Cached::Backoff(Some(_))
        ));
        // 30 -> 60 -> 120 -> 120
        c.rate_limited(&a, t1);
        c.rate_limited(&a, t1);
        c.rate_limited(&a, t1);
        let e = c.entry.lock().clone().unwrap();
        assert_eq!(e.backoff, MAX_BACKOFF);
        // A new session replaces the one entry.
        c.store(&other, t1, &f);
        assert!(matches!(c.lookup(&a, t1), Cached::Miss));
    }
}
