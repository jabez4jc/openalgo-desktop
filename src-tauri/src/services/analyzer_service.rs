//! Analyzer (sandbox) mode: status, toggle, and the engine lifecycle.
//!
//! Turning analyzer mode on starts the sandbox engine (after its catch-up)
//! with live ticks from the market-data feed; turning it off stops it
//! (web `services/analyzer_service.py`).

use super::core::{Reply, UNEXPECTED};
use super::sandbox_feed::FeedTicks;
use crate::error::Result;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

/// Analyzer status data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzerStatus {
    pub analyze_mode: bool,
    pub mode: String,
    pub total_logs: i64,
}

pub struct AnalyzerService;

impl AnalyzerService {
    /// Current analyzer status.
    pub fn get_status(state: &AppState) -> Result<AnalyzerStatus> {
        let analyze_mode = state.sqlite.get_analyze_mode()?;
        let total_logs = state.logs.count_analyzer_logs().unwrap_or(0);
        Ok(AnalyzerStatus {
            analyze_mode,
            mode: if analyze_mode { "analyze" } else { "live" }.to_string(),
            total_logs,
        })
    }

    /// Persist the mode and start or stop the engine in the background
    /// (callers that cannot await: the session route).
    pub fn toggle_mode(state: &AppState, enable: bool) -> Result<AnalyzerStatus> {
        state.sqlite.set_analyze_mode(enable)?;
        Self::spawn_engine_transition(state, enable);
        Self::get_status(state)
    }

    /// Start or stop the sandbox engine on a task owned by the context.
    pub fn spawn_engine_transition(state: &AppState, on: bool) {
        let sandbox = state.sandbox.clone();
        let ticks: Arc<dyn crate::sandbox::TickSource> = FeedTicks::start(state);
        state.spawn(async move {
            if let Err(e) = sandbox.set_analyzer_mode(on, ticks).await {
                tracing::error!("Sandbox engine transition failed: {}", e.message);
            }
        });
    }

    /// Persist the mode and wait for the engine to start or stop.
    pub async fn set_mode(state: &AppState, enable: bool) -> Result<AnalyzerStatus> {
        state.sqlite.set_analyze_mode(enable)?;
        let ticks: Arc<dyn crate::sandbox::TickSource> = FeedTicks::start(state);
        if let Err(e) = state.sandbox.set_analyzer_mode(enable, ticks).await {
            tracing::error!("Sandbox engine transition failed: {}", e.message);
        }
        Self::get_status(state)
    }

    /// `POST /api/v1/analyzer`.
    pub fn status_reply(state: &AppState) -> Reply {
        match Self::get_status(state) {
            Ok(s) => Reply::ok(json!({"status": "success", "data": {
                "analyze_mode": s.analyze_mode, "mode": s.mode, "total_logs": s.total_logs,
            }})),
            Err(e) => {
                tracing::error!("Analyzer status failed: {}", e);
                Reply::error(500, UNEXPECTED)
            }
        }
    }

    /// `POST /api/v1/analyzer/toggle`.
    pub async fn toggle_reply(state: &AppState, mode: bool) -> Reply {
        match Self::set_mode(state, mode).await {
            Ok(s) => Reply::ok(json!({"status": "success", "data": {
                "analyze_mode": s.analyze_mode,
                "message": format!("Analyzer mode switched to {}", s.mode),
                "mode": s.mode,
                "total_logs": s.total_logs,
            }})),
            Err(e) => {
                tracing::error!("Analyzer toggle failed: {}", e);
                Reply::error(500, UNEXPECTED)
            }
        }
    }
}
