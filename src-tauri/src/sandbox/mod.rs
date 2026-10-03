//! Sandbox (analyzer mode) engine. Placeholder for the next wave.
//!
//! The engine will mirror the web's `sandbox/` package (see
//! `docs/audit/2026-10-03/07-sandbox.md`): fund manager with 1 crore default
//! capital, order manager, tick-driven execution task, position netting,
//! T+1 holdings, exchange-aligned MIS square-off, GTT and catch-up, in its
//! own `sandbox.db`, with exact decimal money and the injected clock
//! (`crate::clock`). Services short-circuit here before any broker call when
//! analyzer mode is on, and publish `sandbox.*` events on the bus.
//!
//! Today the older sandbox tables in the main database still back the
//! analyze-mode shapes returned by `/api/v1/funds` and friends.
