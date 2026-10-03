//! Services Layer
//!
//! Business logic shared between Tauri IPC commands and REST API handlers.
//! This architecture matches the Flask OpenAlgo pattern where services
//! contain the core logic and are called by both internal routes and external APIs.
//!
//! # Architecture
//!
//! ```text
//! Frontend UI  --> Tauri Commands ──┐
//!                                   ├──> Services --> Broker/DB
//! External SDK --> REST API ────────┘
//! ```
//!
//! # Services
//!
//! - `OrderService` - Place, modify, cancel orders
//! - `PositionService` - Get positions, close positions
//! - `HoldingsService` - Get holdings
//! - `FundsService` - Get funds/margin
//! - `QuotesService` - Get quotes, market depth
//! - `OrderbookService` - Get order book, trade book
//! - `SmartOrderService` - Smart orders, split orders, basket orders
//! - `SymbolService` - Symbol search, lookup, master contract
//! - `AnalyzerService` - Analyze mode (sandbox) management
//! - `OptionsService` - Option chain, Greeks, option orders
//! - `HistoryService` - Historical data

pub mod analyzer_service;
pub mod apikey_service;
pub mod auth_service;
pub mod broker_auth_service;
pub mod error_log;
pub mod funds_service;
pub mod health_service;
pub mod history_service;
pub mod holdings_service;
pub mod market_calendar_service;
pub mod monitor;
pub mod options_service;
pub mod order_service;
pub mod orderbook_service;
pub mod position_service;
pub mod quotes_service;
pub mod security_service;
pub mod smart_order_service;
pub mod symbol_service;
pub mod system_info;

// Re-export commonly used types and services
pub use analyzer_service::{AnalyzerService, AnalyzerStatus};
pub use funds_service::{FundsResult, FundsService};
pub use history_service::{CandleData, HistoryResult, HistoryService, IntervalsResult};
pub use holdings_service::{HoldingsResult, HoldingsService};
pub use options_service::{
    OptionChainResult, OptionGreeks, OptionSymbolResult, OptionsService, SyntheticFutureResult,
};
pub use order_service::{CancelOrderResult, ModifyOrderResult, OrderService, PlaceOrderResult};
pub use orderbook_service::{
    OrderStatusResult, OrderbookResult, OrderbookService, TradebookResult,
};
pub use position_service::{ClosePositionResult, PositionResult, PositionService};
pub use quotes_service::{DepthResult, QuoteResult, QuotesService};
pub use smart_order_service::{SmartOrderResult, SmartOrderService, SplitOrderResult};
pub use symbol_service::{ExpiryResult, SymbolSearchResult, SymbolService};
