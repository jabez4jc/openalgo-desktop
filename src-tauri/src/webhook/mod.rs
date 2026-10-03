//! Webhook and REST API server module
//!
//! Provides:
//! - Dynamic strategy-based webhooks (/webhook/{webhook_id})
//! - OpenAlgo SDK compatible REST API (/api/v1/*)
//!
//! Supports webhooks from:
//! - TradingView
//! - GoCharting
//! - Chartink
//!
//! Usage:
//! 1. Enable webhook server in settings
//! 2. Run ngrok: `ngrok http <port>`
//! 3. Configure ngrok URL in settings
//! 4. Create strategies with webhook_id
//! 5. Use the webhook URL: `<ngrok_url>/webhook/<webhook_id>`

pub mod handlers;
pub mod rate_limiter;
mod server;
mod types;

pub use server::WebhookServer;
pub use types::{
    ApiKeyRequest,
    // REST API types
    ApiResponse,
    CancelAllOrdersRequest,
    CancelOrderRequest,
    ClosePositionRequest,
    Empty,
    FundsData,
    HoldingData,
    ModifyOrderRequest,
    OrderData,
    PlaceOrderRequest,
    PlaceSmartOrderRequest,
    PositionData,
    ProcessedAlert,
    QuoteData,
    QuoteRequest,
    TradeData,
    // Webhook types
    WebhookPayload,
    // Legacy (for backward compatibility)
    WebhookResponse,
    WebhookResult,
};
