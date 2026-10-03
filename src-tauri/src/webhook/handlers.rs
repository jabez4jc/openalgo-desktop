//! Strategy webhook models (Chartink / TradingView strategies stored in the
//! main database). The legacy `/api/v1` handlers that lived here were
//! replaced by `crate::server::api_v1` and `crate::services`; the strategy
//! webhook endpoint itself arrives with the strategy wave.

/// Kept for routes that still import it from here.
pub use crate::services::core::INVALID_API_KEY;

/// A webhook strategy row.
#[derive(Debug, Clone)]
pub struct Strategy {
    pub id: i64,
    pub name: String,
    pub webhook_id: String,
    pub is_active: bool,
    pub is_intraday: bool,
    pub trading_mode: String,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub squareoff_time: Option<String>,
}

/// A symbol mapped to a webhook strategy.
#[derive(Debug, Clone)]
pub struct SymbolMapping {
    pub symbol: String,
    pub exchange: String,
    pub quantity: i32,
    pub product_type: String,
}
