//! The service layer: every `/api/v1` endpoint's business logic, shared by
//! the HTTP handlers (and later MCP tools and Tauri commands).
//!
//! Handlers validate with [`schema`] (the web's Marshmallow rules), call one
//! function here and send its [`core::Reply`] as is. Services decide the
//! destination once (sandbox engine in analyzer mode, otherwise the broker)
//! and publish the web's event on the bus; side effects are subscribers.
//!
//! | Module | Web services |
//! |---|---|
//! | [`order_service`] | place, smart, modify, cancel, cancel all, close position |
//! | [`batch_order_service`] | basket, split |
//! | [`options_order_service`] | optionsorder, optionsmultiorder |
//! | [`options_service`] | optionsymbol, optionchain, syntheticfuture, optiongreeks, multioptiongreeks |
//! | [`greeks_service`] | Black-76 pricing, IV and Greeks |
//! | [`account_service`] | orderbook, tradebook, positionbook, holdings, funds, orderstatus, openposition, pnl/symbols |
//! | [`market_data_service`] | quotes, multiquotes, depth, history, intervals, ticker, margin |
//! | [`symbol_service`] | symbol, search, expiry, instruments, freeze quantities |
//! | [`gtt_service`] | place/modify/cancel GTT, GTT book |
//! | [`analyzer_service`] | analyzer status and toggle (engine lifecycle) |
//! | [`sandbox_feed`] | the sandbox engine's quote, tick and symbol adapters |

pub mod account_service;
pub mod analyzer_service;
pub mod apikey_service;
pub mod auth_service;
pub mod batch_order_service;
pub mod broker_auth_service;
pub mod core;
pub mod error_log;
pub mod greeks_service;
pub mod gtt_service;
pub mod health_service;
pub mod market_calendar_service;
pub mod market_data_service;
pub mod monitor;
pub mod options_order_service;
pub mod options_service;
pub mod order_service;
pub mod sandbox_feed;
pub mod schema;
pub mod schemas;
pub mod security_service;
pub mod symbol_service;
pub mod system_info;

pub use analyzer_service::{AnalyzerService, AnalyzerStatus};
pub use core::Reply;
pub use order_service::Route;
