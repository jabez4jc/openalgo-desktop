//! Shared vocabulary of the sandbox engine: order enums with the web's exact
//! strings, the error type every public call returns, decimal helpers and the
//! symbol-master view the engine needs.

use chrono::NaiveDate;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::HashMap;
use std::str::FromStr;

/// `BUY` / `SELL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Buy,
    Sell,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Buy => "BUY",
            Action::Sell => "SELL",
        }
    }

    /// Case-insensitive, like the web's `.upper()` before comparing.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "BUY" => Some(Action::Buy),
            "SELL" => Some(Action::Sell),
            _ => None,
        }
    }

    pub fn opposite(&self) -> Self {
        match self {
            Action::Buy => Action::Sell,
            Action::Sell => Action::Buy,
        }
    }

    /// +1 for BUY, -1 for SELL.
    pub fn sign(&self) -> i64 {
        match self {
            Action::Buy => 1,
            Action::Sell => -1,
        }
    }
}

/// `MARKET` / `LIMIT` / `SL` / `SL-M`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PriceType {
    Market,
    Limit,
    Sl,
    SlM,
}

impl PriceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            PriceType::Market => "MARKET",
            PriceType::Limit => "LIMIT",
            PriceType::Sl => "SL",
            PriceType::SlM => "SL-M",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "MARKET" => Some(PriceType::Market),
            "LIMIT" => Some(PriceType::Limit),
            "SL" => Some(PriceType::Sl),
            "SL-M" => Some(PriceType::SlM),
            _ => None,
        }
    }

    /// Whether the type carries a limit price (LIMIT, SL).
    pub fn has_price(&self) -> bool {
        matches!(self, PriceType::Limit | PriceType::Sl)
    }

    /// Whether the type carries a trigger price (SL, SL-M).
    pub fn has_trigger(&self) -> bool {
        matches!(self, PriceType::Sl | PriceType::SlM)
    }
}

/// `CNC` / `NRML` / `MIS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Product {
    Cnc,
    Nrml,
    Mis,
}

impl Product {
    pub fn as_str(&self) -> &'static str {
        match self {
            Product::Cnc => "CNC",
            Product::Nrml => "NRML",
            Product::Mis => "MIS",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "CNC" => Some(Product::Cnc),
            "NRML" => Some(Product::Nrml),
            "MIS" => Some(Product::Mis),
            _ => None,
        }
    }
}

/// Order statuses, byte-identical to the web (note the space in
/// `"trigger pending"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderStatus {
    Open,
    TriggerPending,
    Complete,
    Cancelled,
    Rejected,
}

impl OrderStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            OrderStatus::Open => "open",
            OrderStatus::TriggerPending => "trigger pending",
            OrderStatus::Complete => "complete",
            OrderStatus::Cancelled => "cancelled",
            OrderStatus::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(OrderStatus::Open),
            "trigger pending" => Some(OrderStatus::TriggerPending),
            "complete" => Some(OrderStatus::Complete),
            "cancelled" => Some(OrderStatus::Cancelled),
            "rejected" => Some(OrderStatus::Rejected),
            _ => None,
        }
    }

    /// Still resting: can be filled, modified or cancelled.
    pub fn is_pending(&self) -> bool {
        matches!(self, OrderStatus::Open | OrderStatus::TriggerPending)
    }
}

/// Exchanges the web accepts (`utils/constants.py` VALID_EXCHANGES).
pub const VALID_EXCHANGES: &[&str] = &[
    "NSE",
    "NFO",
    "CDS",
    "BSE",
    "BFO",
    "BCD",
    "MCX",
    "NCDEX",
    "NCO",
    "NSE_INDEX",
    "BSE_INDEX",
    "MCX_INDEX",
    "GLOBAL_INDEX",
    "CRYPTO",
];

/// Exchanges whose quantity must be a multiple of the lot size.
pub const LOT_SIZE_EXCHANGES: &[&str] = &["NFO", "BFO", "CDS", "BCD", "MCX", "NCDEX", "CRYPTO"];

/// Derivative exchanges (web `FNO_EXCHANGES`, CRYPTO included).
pub const FNO_EXCHANGES: &[&str] = &["NFO", "BFO", "MCX", "CDS", "BCD", "NCDEX", "NCO", "CRYPTO"];

/// Option by exchange and canonical suffix (web `is_option`).
pub fn is_option(symbol: &str, exchange: &str) -> bool {
    FNO_EXCHANGES.contains(&exchange) && (symbol.ends_with("CE") || symbol.ends_with("PE"))
}

/// Future or perpetual (web `is_future`).
pub fn is_future(symbol: &str, exchange: &str) -> bool {
    if exchange == "CRYPTO" {
        return !(symbol.ends_with("CE") || symbol.ends_with("PE"));
    }
    FNO_EXCHANGES.contains(&exchange) && symbol.ends_with("FUT")
}

/// `(symbol, exchange)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SymbolKey {
    pub symbol: String,
    pub exchange: String,
}

impl SymbolKey {
    pub fn new(symbol: impl Into<String>, exchange: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            exchange: exchange.into(),
        }
    }
}

/// What the engine needs from the symbol master for one instrument.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolMeta {
    pub lotsize: i64,
    /// Multiplier for P&L (1 for everything except a few crypto contracts).
    pub contract_value: Decimal,
    /// Contract expiry from the master (`DD-MMM-YY`), used when the symbol
    /// itself does not carry a `DDMMMYY` date.
    pub expiry: Option<NaiveDate>,
}

impl Default for SymbolMeta {
    fn default() -> Self {
        Self {
            lotsize: 1,
            contract_value: Decimal::ONE,
            expiry: None,
        }
    }
}

/// The symbol master, as the engine sees it. Production wraps the app's
/// symbol cache; tests use [`StaticSymbols`].
pub trait SymbolSource: Send + Sync {
    fn lookup(&self, symbol: &str, exchange: &str) -> Option<SymbolMeta>;
}

/// In-memory symbol master (tests and fixtures).
#[derive(Debug, Default, Clone)]
pub struct StaticSymbols {
    map: HashMap<SymbolKey, SymbolMeta>,
}

impl StaticSymbols {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, symbol: &str, exchange: &str, lotsize: i64) -> Self {
        self.insert(
            symbol,
            exchange,
            SymbolMeta {
                lotsize,
                ..SymbolMeta::default()
            },
        );
        self
    }

    pub fn insert(&mut self, symbol: &str, exchange: &str, meta: SymbolMeta) {
        self.map.insert(SymbolKey::new(symbol, exchange), meta);
    }
}

impl SymbolSource for StaticSymbols {
    fn lookup(&self, symbol: &str, exchange: &str) -> Option<SymbolMeta> {
        self.map.get(&SymbolKey::new(symbol, exchange)).cloned()
    }
}

/// Error returned by every public sandbox call: the HTTP status and the
/// trader-facing message the web returns for the same case. Serialise it with
/// [`SandboxError::body`].
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxError {
    pub http_status: u16,
    pub message: String,
    /// Set for a rejected order that was still recorded (CNC sell check).
    pub orderid: Option<String>,
    /// Set for GTT errors that name the trigger.
    pub trigger_id: Option<String>,
}

impl SandboxError {
    pub fn new(http_status: u16, message: impl Into<String>) -> Self {
        Self {
            http_status,
            message: message.into(),
            orderid: None,
            trigger_id: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(409, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, message)
    }

    pub fn with_orderid(mut self, orderid: impl Into<String>) -> Self {
        self.orderid = Some(orderid.into());
        self
    }

    pub fn with_trigger_id(mut self, trigger_id: impl Into<String>) -> Self {
        self.trigger_id = Some(trigger_id.into());
        self
    }

    /// The web's error body: `{"status":"error","message",...,"mode":"analyze"}`.
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            status: "error",
            message: self.message.clone(),
            mode: "analyze",
            orderid: self.orderid.clone(),
            trigger_id: self.trigger_id.clone(),
        }
    }
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.http_status)
    }
}

impl std::error::Error for SandboxError {}

impl From<rusqlite::Error> for SandboxError {
    fn from(e: rusqlite::Error) -> Self {
        tracing::error!("Sandbox database error: {}", e);
        SandboxError::internal(
            "The sandbox could not save this change. Try again; if it keeps failing, restart OpenAlgo.",
        )
    }
}

impl From<crate::error::AppError> for SandboxError {
    fn from(e: crate::error::AppError) -> Self {
        tracing::error!("Sandbox error: {}", e);
        SandboxError::internal(
            "The sandbox could not complete this request. Try again; if it keeps failing, restart OpenAlgo.",
        )
    }
}

pub type SbResult<T> = std::result::Result<T, SandboxError>;

/// Serialised error body.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ErrorBody {
    pub status: &'static str,
    pub message: String,
    pub mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orderid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Decimal helpers
// ---------------------------------------------------------------------------

/// Decimal places kept for margin amounts (paise).
pub const MARGIN_DP: u32 = 2;
/// Decimal places kept for average prices.
pub const AVG_DP: u32 = 8;
/// Decimal places kept for P&L percentages (web DECIMAL(10,4)).
pub const PCT_DP: u32 = 4;

/// Text form stored in `sandbox.db` (exact, no exponent).
pub fn dec_to_db(d: Decimal) -> String {
    d.normalize().to_string()
}

/// Parse a stored decimal. Garbage reads as zero and is logged, so one bad
/// row cannot take down a book.
pub fn dec_from_db(s: &str) -> Decimal {
    match Decimal::from_str(s.trim()) {
        Ok(d) => d,
        Err(_) => match Decimal::from_scientific(s.trim()) {
            Ok(d) => d,
            Err(_) => {
                if !s.trim().is_empty() {
                    tracing::warn!("Unreadable sandbox amount '{}' read as 0", s);
                }
                Decimal::ZERO
            }
        },
    }
}

/// A float from a quote or request, as an exact decimal (via its shortest
/// text form, like Python's `Decimal(str(x))`).
pub fn dec_from_f64(x: f64) -> Decimal {
    if !x.is_finite() {
        return Decimal::ZERO;
    }
    Decimal::from_str(&format!("{}", x))
        .or_else(|_| Decimal::from_scientific(&format!("{:e}", x)))
        .unwrap_or(Decimal::ZERO)
}

/// Money as the web serialises it: the DECIMAL(…,2) column read back as a
/// float.
pub fn money(d: Decimal) -> f64 {
    d.round_dp(2).to_f64().unwrap_or(0.0)
}

/// A percentage as the web serialises it (DECIMAL(10,4)).
pub fn pct(d: Decimal) -> f64 {
    d.round_dp(PCT_DP).to_f64().unwrap_or(0.0)
}

/// An unrounded decimal as a float (totals the web computes in Python).
pub fn float(d: Decimal) -> f64 {
    d.to_f64().unwrap_or(0.0)
}

/// Money in a trader-facing message: rupees with two decimals.
pub fn rupees(d: Decimal) -> String {
    format!("{:.2}", d.round_dp(2))
}
