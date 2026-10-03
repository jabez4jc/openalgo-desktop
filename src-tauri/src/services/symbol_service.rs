//! Symbol master service: download, persist, load, search, expiries.
//!
//! The master lives in one place at runtime, the shared `SymbolResolver`
//! (`state.symbols`, also held by every broker adapter). SQLite is only the
//! cache that survives a restart.

use crate::brokers::common::symbols::{ContractQuery, SymToken};
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Symbol search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolSearchResult {
    pub symbol: String,
    pub brsymbol: String,
    pub token: String,
    pub exchange: String,
    pub brexchange: String,
    pub name: String,
    pub instrument_type: String,
    pub lot_size: i32,
    pub tick_size: f64,
    pub strike: Option<f64>,
    pub expiry: Option<String>,
}

impl From<SymToken> for SymbolSearchResult {
    fn from(s: SymToken) -> Self {
        Self {
            strike: Some(s.strike),
            expiry: Some(s.expiry),
            symbol: s.symbol,
            brsymbol: s.brsymbol,
            token: s.token,
            exchange: s.exchange,
            brexchange: s.brexchange,
            name: s.name,
            instrument_type: s.instrument_type,
            lot_size: s.lot_size,
            tick_size: s.tick_size,
        }
    }
}

/// Expiry dates result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpiryResult {
    pub success: bool,
    pub expiry_dates: Vec<String>,
}

pub struct SymbolService;

impl SymbolService {
    /// Search: symbols starting with the query first (sorted), then symbols
    /// or names containing it, up to `limit`.
    pub fn search_symbols(
        state: &AppState,
        query: &str,
        exchange: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<SymbolSearchResult>> {
        let limit = limit.unwrap_or(50);
        let snap = state.symbols.snapshot();
        let mut out: Vec<&SymToken> = snap.search_prefix(query, exchange, limit);
        if out.len() < limit {
            let q = query.to_lowercase();
            for s in snap.rows() {
                if out.len() >= limit {
                    break;
                }
                if exchange.is_some_and(|e| !s.exchange.eq_ignore_ascii_case(e)) {
                    continue;
                }
                let hit =
                    s.symbol.to_lowercase().contains(&q) || s.name.to_lowercase().contains(&q);
                if hit && !out.iter().any(|o| std::ptr::eq(*o, s)) {
                    out.push(s);
                }
            }
        }
        Ok(out.into_iter().cloned().map(Into::into).collect())
    }

    pub fn get_symbol_info(
        state: &AppState,
        exchange: &str,
        symbol: &str,
    ) -> Result<SymbolSearchResult> {
        state
            .symbols
            .by_symbol(exchange, symbol)
            .map(Into::into)
            .ok_or_else(|| AppError::NotFound(format!("Symbol not found: {} {}", exchange, symbol)))
    }

    pub fn get_symbol_by_token(
        state: &AppState,
        exchange: &str,
        token: &str,
    ) -> Result<SymbolSearchResult> {
        state
            .symbols
            .by_token(exchange, token)
            .map(Into::into)
            .ok_or_else(|| AppError::NotFound(format!("Token not found: {} {}", exchange, token)))
    }

    pub fn get_symbol_count(state: &AppState) -> usize {
        state.symbols.len()
    }

    /// Every instrument (optionally one exchange).
    pub fn get_instruments(state: &AppState, exchange: Option<&str>) -> Vec<SymbolSearchResult> {
        state
            .symbols
            .snapshot()
            .rows()
            .iter()
            .filter(|s| exchange.is_none_or(|e| s.exchange.eq_ignore_ascii_case(e)))
            .cloned()
            .map(Into::into)
            .collect()
    }

    /// Expiries of an underlying from the `expiry` column (`DD-MMM-YY`),
    /// earliest first. `instrument_type` is the web's `futures` / `options`
    /// or an exact `FUT` / `CE` / `PE`.
    pub fn get_expiry_dates(
        state: &AppState,
        symbol: &str,
        exchange: &str,
        instrument_type: &str,
    ) -> Result<ExpiryResult> {
        let types: &[&str] = match instrument_type.to_ascii_lowercase().as_str() {
            "futures" | "fut" => &["FUT"],
            "options" => &["CE", "PE"],
            "ce" => &["CE"],
            "pe" => &["PE"],
            _ => &["FUT", "CE", "PE"],
        };
        let snap = state.symbols.snapshot();
        let mut dated: Vec<(chrono::NaiveDate, String)> = types
            .iter()
            .flat_map(|t| snap.expiries(exchange, symbol, Some(t)))
            .filter_map(|e| {
                crate::brokers::common::master_contract::parse_oa_expiry(&e).map(|d| (d, e))
            })
            .collect();
        dated.sort();
        dated.dedup();
        Ok(ExpiryResult {
            success: true,
            expiry_dates: dated.into_iter().map(|(_, e)| e).collect(),
        })
    }

    /// Contracts of an underlying (option-chain building block).
    pub fn contracts(state: &AppState, q: &ContractQuery<'_>) -> Vec<SymToken> {
        state.symbols.contracts(q)
    }

    /// Load the persisted master into memory (start-up / broker resume).
    pub fn load_from_db(state: &AppState) -> Result<usize> {
        let rows = state.sqlite.load_symbols()?;
        Ok(state.symbols.load(rows))
    }

    /// Download the master from the connected broker, persist it, and swap
    /// it in as the new in-memory generation.
    pub async fn refresh_symbol_master(state: &AppState) -> Result<usize> {
        info!("Downloading the master contract");
        let session = state.get_broker_session().ok_or_else(|| {
            AppError::Auth("Log in to your broker to download the master contract.".into())
        })?;
        let broker = state.brokers.get(&session.broker_id).ok_or_else(|| {
            AppError::Broker("This broker is not available in this version.".into())
        })?;
        let auth = AuthToken::new(session.auth_token.expose())
            .with_feed(session.feed_token.as_ref().map(|t| t.expose().to_string()));
        // No database connection is held across the download.
        let rows = broker.download_master_contract(&auth).await?;
        if rows.is_empty() {
            return Err(AppError::Broker(
                "The broker returned an empty instrument list. Try again later.".into(),
            ));
        }
        state.sqlite.store_symbols(&rows)?;
        let n = state.symbols.load(rows);
        info!("Master contract loaded: {} instruments", n);
        Ok(n)
    }
}
