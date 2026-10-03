//! Upstox instrument master -> OpenAlgo symbol master (web
//! `database/master_contract_db.py`).
//!
//! The master is one gzip'd JSON array. Per row: `NSE_COM` is dropped;
//! `segment` maps to the OpenAlgo exchange and is kept as `brexchange`;
//! `instrument_key` is the token; `expiry` (epoch ms) becomes `DD-MMM-YY`;
//! `tick_size` arrives in paise and is divided by 100; the OpenAlgo symbol
//! is rebuilt positionally from the space-separated `trading_symbol`
//! (`NAME FUT DD MMM YY` -> `NAMEDDMMMYYFUT`, `NAME STRIKE CE DD MMM YY` ->
//! `NAMEDDMMMYYSTRIKECE`), then the index renames apply (NSE names on every
//! row, BSE short names on `BSE_INDEX` rows only, world names on
//! `GLOBAL_INDEX` rows only).

use super::mapping::exchange_from_segment;
use super::UpstoxBroker;
use crate::brokers::common::de::{f64_lenient, i64_lenient, string_lenient};
use crate::brokers::common::http::DOWNLOAD_TIMEOUT;
use crate::brokers::common::master_contract::{
    format_expiry, rename, BSE_INDEX_RENAMES, NSE_INDEX_RENAMES,
};
use crate::brokers::common::symbols::SymToken;
use crate::brokers::types::AuthToken;
use crate::error::{AppError, Result};
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;

/// World indices and indicators -> OpenAlgo `GLOBAL_INDEX` symbols.
pub const GLOBAL_INDEX_RENAMES: &[(&str, &str)] = &[
    ("^HSI", "HANGSENG"),
    ("^DJI", "DOWJONES"),
    ("^FTSE", "UK100"),
    ("^GSPC", "US500"),
    ("^GDAXI", "GERMANY40"),
    ("^FCHI", "FRANCE40"),
    ("^N225", "JAPAN225"),
    ("IXIX", "US100"),
    ("GIFT NIFTY", "GIFTNIFTY"),
    ("DOW FUTURES", "US30"),
    ("BZUSD", "BRENTOIL"),
    ("CLUSD", "WTIOIL"),
];

/// The columns the web reads from each instrument.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RawInstrument {
    #[serde(deserialize_with = "string_lenient")]
    pub segment: String,
    #[serde(deserialize_with = "string_lenient")]
    pub instrument_key: String,
    #[serde(deserialize_with = "string_lenient")]
    pub trading_symbol: String,
    #[serde(deserialize_with = "string_lenient")]
    pub name: String,
    pub expiry: Option<Value>,
    #[serde(deserialize_with = "f64_lenient")]
    pub strike_price: f64,
    #[serde(deserialize_with = "i64_lenient")]
    pub lot_size: i64,
    #[serde(deserialize_with = "string_lenient")]
    pub instrument_type: String,
    #[serde(deserialize_with = "f64_lenient")]
    pub tick_size: f64,
}

/// web `reformat_symbol`: purely positional on the space-split symbol.
pub fn reformat_symbol(trading_symbol: &str, instrument_type: &str) -> String {
    let parts: Vec<&str> = trading_symbol.split(' ').collect();
    match instrument_type {
        "FUT" if parts.len() == 5 => format!(
            "{}{}{}{}{}",
            parts[0], parts[2], parts[3], parts[4], parts[1]
        ),
        "CE" | "PE" if parts.len() == 6 => format!(
            "{}{}{}{}{}{}",
            parts[0], parts[3], parts[4], parts[5], parts[1], parts[2]
        ),
        _ => trading_symbol.to_string(),
    }
}

/// Epoch milliseconds (number or numeric string) -> `DD-MMM-YY` of the UTC
/// date, as pandas `to_datetime(unit="ms")` reads it.
pub fn expiry_from_ms(v: Option<&Value>) -> String {
    let ms = match v {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    ms.filter(|m| m.is_finite() && *m > 0.0)
        .and_then(|m| chrono::DateTime::from_timestamp_millis(m as i64))
        .map(|d| format_expiry(d.date_naive()))
        .unwrap_or_default()
}

/// One master row, or `None` for dropped segments.
pub fn to_row(r: RawInstrument) -> Option<SymToken> {
    if r.segment == "NSE_COM" || r.instrument_key.is_empty() {
        return None;
    }
    let exchange = exchange_from_segment(&r.segment)?;
    let mut symbol = reformat_symbol(&r.trading_symbol, &r.instrument_type);
    if let Some(s) = rename(NSE_INDEX_RENAMES, &symbol) {
        symbol = s.to_string();
    }
    if exchange == "BSE_INDEX" {
        if let Some(s) = rename(BSE_INDEX_RENAMES, &symbol) {
            symbol = s.to_string();
        }
    }
    if exchange == "GLOBAL_INDEX" {
        if let Some(s) = rename(GLOBAL_INDEX_RENAMES, &symbol) {
            symbol = s.to_string();
        }
    }
    Some(SymToken {
        symbol,
        brsymbol: r.trading_symbol,
        name: r.name,
        exchange: exchange.to_string(),
        brexchange: r.segment,
        token: r.instrument_key,
        expiry: expiry_from_ms(r.expiry.as_ref()),
        strike: r.strike_price,
        lot_size: i32::try_from(r.lot_size).unwrap_or(1),
        instrument_type: r.instrument_type,
        // Paise to rupees; indices send null, which reads as 0.
        tick_size: r.tick_size / 100.0,
    })
}

/// Parse the decompressed JSON array.
pub fn parse_json<R: Read>(reader: R) -> Result<Vec<SymToken>> {
    let raw: Vec<RawInstrument> = serde_json::from_reader(reader).map_err(|e| {
        tracing::error!("Upstox instrument master is not the expected JSON: {}", e);
        AppError::Broker(
            "Upstox's instrument list has an unexpected format. Try downloading the master contract again later."
                .into(),
        )
    })?;
    let total = raw.len();
    let rows: Vec<SymToken> = raw.into_iter().filter_map(to_row).collect();
    if rows.len() < total {
        tracing::info!(
            "Upstox instrument rows skipped (NSE_COM or unmapped segment): {}",
            total - rows.len()
        );
    }
    Ok(rows)
}

/// Decompress and parse the gzip master.
pub fn parse_gz(bytes: &[u8]) -> Result<Vec<SymToken>> {
    parse_json(std::io::BufReader::new(flate2::read::GzDecoder::new(bytes)))
}

pub async fn download(b: &UpstoxBroker, _auth: &AuthToken) -> Result<Vec<SymToken>> {
    // The master is a public static file on Upstox's asset host (no auth,
    // not paced, as on the web).
    let resp = b
        .http
        .get(&b.urls.master)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(status = status.as_u16(), "Upstox master download refused");
        return Err(AppError::Broker(
            "Upstox did not send the instrument list. Try downloading the master contract again."
                .into(),
        ));
    }
    let bytes = resp.bytes().await?;
    // Parsing ~100k rows is CPU work; keep it off the async workers. The
    // handle is awaited, so the task is owned.
    let rows = tokio::task::spawn_blocking(move || parse_gz(&bytes))
        .await
        .map_err(|e| AppError::Internal(format!("Master contract parse task failed: {}", e)))??;
    tracing::info!("Upstox master contract parsed: {} instruments", rows.len());
    Ok(rows)
}
