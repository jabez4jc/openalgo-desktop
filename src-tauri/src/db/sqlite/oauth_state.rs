//! Pending broker OAuth flows.
//!
//! The server generates a random `state` when the trader starts a broker
//! login, stores only its SHA-256 with the broker and an expiry, and the
//! callback consumes it exactly once. The table never holds more than a
//! handful of rows: expired ones are purged on every insert and the count is
//! capped.

use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

pub const MAX_PENDING: i64 = 16;

pub fn hash_state(state: &str) -> String {
    hex::encode(Sha256::digest(state.as_bytes()))
}

pub fn insert(
    conn: &Connection,
    state: &str,
    broker: &str,
    now: DateTime<Utc>,
    ttl: Duration,
) -> Result<()> {
    conn.execute(
        "DELETE FROM pending_oauth WHERE expires_at <= ?1",
        [now.to_rfc3339()],
    )?;
    conn.execute(
        "INSERT INTO pending_oauth (state_hash, broker, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            hash_state(state),
            broker,
            now.to_rfc3339(),
            (now + ttl).to_rfc3339()
        ],
    )?;
    conn.execute(
        "DELETE FROM pending_oauth WHERE state_hash NOT IN
            (SELECT state_hash FROM pending_oauth ORDER BY created_at DESC LIMIT ?1)",
        [MAX_PENDING],
    )?;
    Ok(())
}

/// Consume `state` for `broker`. True only for a matching, unexpired,
/// unused state. The row is deleted whether or not it was still valid.
pub fn consume(conn: &Connection, state: &str, broker: &str, now: DateTime<Utc>) -> Result<bool> {
    let h = hash_state(state);
    let row: Option<(String, String)> = {
        let mut stmt =
            conn.prepare("SELECT broker, expires_at FROM pending_oauth WHERE state_hash = ?1")?;
        let mut rows = stmt.query([&h])?;
        match rows.next()? {
            Some(r) => Some((r.get(0)?, r.get(1)?)),
            None => None,
        }
    };
    let Some((stored_broker, expires)) = row else {
        return Ok(false);
    };
    conn.execute("DELETE FROM pending_oauth WHERE state_hash = ?1", [&h])?;
    let not_expired = DateTime::parse_from_rfc3339(&expires)
        .map(|e| e.with_timezone(&Utc) > now)
        .unwrap_or(false);
    Ok(not_expired && stored_broker == broker)
}

pub fn count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM pending_oauth", [], |r| r.get(0))?)
}
