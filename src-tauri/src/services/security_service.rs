//! Security dashboard (web `blueprints/security.py`): blocked addresses,
//! 404 and invalid API key tracking, thresholds, login activity and active
//! sessions. Backed by `logs.db` (see `db::sqlite::monitor`), the settings
//! row, the browser session store and the monitor's ban cache.

use crate::db::sqlite::monitor::{self as store, BanRow, LoginAttempt, TrackerRow};
use crate::db::sqlite::webui;
use crate::error::Result;
use crate::state::AppState;
use chrono::{DateTime, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::IpAddr;

/// `dd-mm-YYYY hh:mm:ss AM` in IST (web `strftime("%d-%m-%Y %I:%M:%S %p")`).
pub fn web_time(ts: &str) -> String {
    store::parse_ts(ts)
        .map(|t| {
            t.with_timezone(&Kolkata)
                .format("%d-%m-%Y %I:%M:%S %p")
                .to_string()
        })
        .unwrap_or_else(|| "Unknown".into())
}

fn iso_ist(t: DateTime<Utc>) -> String {
    t.with_timezone(&Kolkata)
        .format("%Y-%m-%dT%H:%M:%S%.6f%:z")
        .to_string()
}

/// Record a sign-in attempt for the login activity list. Failures to write
/// are logged and never block the sign-in.
pub fn record_login(
    ctx: &AppState,
    username: &str,
    ip: IpAddr,
    status: &str,
    login_type: &str,
    failure_reason: Option<&str>,
) {
    let a = LoginAttempt {
        username: username.to_string(),
        ip_address: Some(ip.to_string()),
        device_info: None,
        status: status.into(),
        login_type: Some(login_type.into()),
        broker: ctx.get_broker_session().map(|b| b.broker_id),
        failure_reason: failure_reason.map(String::from),
    };
    let now = ctx.now();
    if let Err(e) = ctx
        .logs
        .conn()
        .and_then(|c| store::insert_login_attempt(&c, &a, now))
    {
        tracing::warn!("Could not record a sign-in attempt: {}", e);
    }
}

fn ban_json(b: &BanRow) -> Value {
    json!({
        "ip_address": b.ip_address,
        "ban_reason": b.ban_reason,
        "banned_at": web_time(&b.banned_at),
        "expires_at": b.expires_at.as_deref().map(web_time).unwrap_or_else(|| "Permanent".into()),
        "is_permanent": b.is_permanent,
        "ban_count": b.ban_count,
        "created_by": b.created_by,
    })
}

fn tracker_404_json(t: &TrackerRow) -> Value {
    json!({
        "ip_address": t.ip_address,
        "error_count": t.count,
        "first_error_at": web_time(&t.first_at),
        "last_error_at": web_time(&t.last_at),
        "paths_attempted": t.detail,
    })
}

fn tracker_api_json(t: &TrackerRow) -> Value {
    json!({
        "ip_address": t.ip_address,
        "attempt_count": t.count,
        "first_attempt_at": web_time(&t.first_at),
        "last_attempt_at": web_time(&t.last_at),
        "api_keys_tried": t.detail,
    })
}

/// `GET /security/api/data`.
pub fn dashboard_data(ctx: &AppState) -> Result<Value> {
    let now = ctx.now();
    let settings = ctx
        .sqlite
        .conn()
        .and_then(|c| webui::security_settings(&c))?;
    let conn = ctx.logs.conn()?;
    let bans = store::all_bans(&conn, now)?;
    let s404 = store::suspicious_404(&conn, 1, now)?;
    let sapi = store::suspicious_api(&conn, 1, now)?;
    Ok(json!({
        "banned_ips": bans.iter().map(ban_json).collect::<Vec<_>>(),
        "suspicious_ips": s404.iter().map(tracker_404_json).collect::<Vec<_>>(),
        "api_abuse_ips": sapi.iter().map(tracker_api_json).collect::<Vec<_>>(),
        "security_settings": settings,
    }))
}

/// `GET /security/stats`.
pub fn stats(ctx: &AppState) -> Result<Value> {
    let now = ctx.now();
    let conn = ctx.logs.conn()?;
    let bans = store::all_bans(&conn, now)?;
    let permanent = bans.iter().filter(|b| b.is_permanent).count();
    let trackers = store::suspicious_404(&conn, 1, now)?;
    let suspicious = trackers.iter().filter(|t| t.count >= 5).count();
    let near = trackers
        .iter()
        .filter(|t| t.count >= 15 && t.count < 20)
        .count();
    Ok(json!({
        "total_bans": bans.len(),
        "permanent_bans": permanent,
        "temporary_bans": bans.len() - permanent,
        "suspicious_ips": suspicious,
        "near_threshold": near,
    }))
}

/// `GET /security/api/login-activity`.
pub fn login_activity(ctx: &AppState, limit: i64, status: Option<&str>) -> Result<Value> {
    let conn = ctx.logs.conn()?;
    let rows = store::login_attempts(&conn, limit, status)?;
    Ok(Value::Array(
        rows.into_iter()
            .map(|(a, ts)| {
                json!({
                    "username": a.username,
                    "ip_address": a.ip_address,
                    "device_info": a.device_info,
                    "status": a.status,
                    "login_type": a.login_type,
                    "broker": a.broker,
                    "failure_reason": a.failure_reason,
                    "timestamp": store::parse_ts(&ts).map(iso_ist).unwrap_or(ts),
                })
            })
            .collect(),
    ))
}

/// A stable, non-secret label for a session (never the cookie value).
pub fn session_label(id: &str) -> String {
    hex::encode(Sha256::digest(id.as_bytes()))[..16].to_string()
}

/// `GET /security/api/active-sessions`.
pub fn active_sessions(ctx: &AppState, current_session_id: &str) -> Value {
    let broker = ctx.get_broker_session().map(|b| b.broker_id);
    let sessions: Vec<Value> = ctx
        .sessions
        .authenticated_sessions()
        .into_iter()
        .map(|s| {
            json!({
                "session_id": session_label(&s.id),
                "device_info": Value::Null,
                "ip_address": Value::Null,
                "broker": broker,
                "login_time": s.authenticated_at.or(s.created_at).map(iso_ist),
                "last_seen": s.last_seen.map(iso_ist),
            })
        })
        .collect();
    json!({
        "status": "success",
        "current_session_id": session_label(current_session_id),
        "sessions": sessions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_time_is_ist_12_hour() {
        assert_eq!(web_time("2026-10-05 04:30:00"), "05-10-2026 10:00:00 AM");
        assert_eq!(web_time("garbage"), "Unknown");
        assert_eq!(session_label("abc").len(), 16);
        assert_ne!(session_label("abc"), "abc");
    }
}
