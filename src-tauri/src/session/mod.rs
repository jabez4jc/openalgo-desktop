//! Sessions: the daily boundary, browser sessions, and the task that ends
//! everything at the boundary.

pub mod boundary;
pub mod web;

use crate::events::SessionEndReason;
use crate::services::broker_auth_service::BrokerAuthService;
use crate::state::AppState;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::Duration;

/// Poll interval of the expiry task. Short enough that a machine waking from
/// sleep past 03:00 expires the session within a minute, and long enough to
/// cost nothing.
pub const EXPIRY_POLL: Duration = Duration::from_secs(30);

/// One step of the expiry task: if a boundary passed after `last_check`,
/// revoke the broker session and drop every browser session. Returns
/// whether it fired.
pub async fn expire_if_crossed(ctx: &AppState, last_check: DateTime<Utc>) -> bool {
    let now = ctx.now();
    let cfg = ctx.server_config();
    let boundary = boundary::last_boundary(now, cfg.session_expiry_hour, cfg.session_expiry_minute);
    if last_check < boundary && now >= boundary {
        tracing::info!("Daily session boundary reached; ending broker and browser sessions");
        if let Err(e) = BrokerAuthService::revoke(ctx, SessionEndReason::DailyExpiry).await {
            tracing::error!("Could not revoke the broker session at the boundary: {}", e);
        }
        ctx.sessions.clear();
        true
    } else {
        false
    }
}

/// Start the owned background task that enforces the daily boundary.
pub fn spawn_expiry_task(ctx: Arc<AppState>) {
    let token = ctx.shutdown.clone();
    // Weak: the context owns this task, so a strong reference would be a cycle.
    let weak = Arc::downgrade(&ctx);
    let mut last = ctx.now();
    ctx.spawn(async move {
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(EXPIRY_POLL) => {
                    let Some(c) = weak.upgrade() else { break };
                    expire_if_crossed(&c, last).await;
                    last = c.now();
                }
            }
        }
    });
}
