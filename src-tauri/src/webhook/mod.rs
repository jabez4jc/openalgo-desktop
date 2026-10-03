//! Legacy REST handlers (`/api/v1/*`) and strategy webhook handlers.
//!
//! The HTTP server lives in `crate::server`; these handlers are mounted there
//! behind the JSON error envelope, the per-IP rate limiter and the body limit.

pub mod handlers;
pub mod types;

pub use types::{
    ApiKeyRequest, ApiResponse, CancelAllOrdersRequest, CancelOrderRequest, ClosePositionRequest,
    Empty, FundsData, HoldingData, ModifyOrderRequest, OrderData, PlaceOrderRequest,
    PlaceSmartOrderRequest, PositionData, ProcessedAlert, QuoteData, QuoteRequest, TradeData,
    WebhookPayload, WebhookResponse, WebhookResult,
};
