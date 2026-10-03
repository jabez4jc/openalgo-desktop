//! The OpenAlgo API key (web `api_keys` table equivalent, one key per user).
//!
//! Stored three ways, each for one job:
//! * `lookup_hmac`: HMAC-SHA256(pepper, key), unique-indexed, finds the row
//!   in O(1) without trying Argon2 against every key;
//! * `key_hash`: Argon2id(key + pepper), the actual verification;
//! * `encrypted_key`: AES-GCM with AAD, so the signed-in trader can see and
//!   copy the key on the API key page, as on the web.

use crate::error::Result;
use crate::security::crypto::Aad;
use crate::security::{Secret, SecurityManager};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone)]
pub struct ApiKeyRow {
    pub id: i64,
    pub name: String,
    pub key_hash: String,
    pub order_mode: String,
}

pub fn aad(name: &str) -> Aad {
    Aad::new("api_keys", "encrypted_key", name)
}

/// 64 hex characters from 32 random bytes, same as the web.
pub fn generate_api_key() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn find_by_lookup(conn: &Connection, lookup: &str) -> Result<Option<ApiKeyRow>> {
    Ok(conn
        .query_row(
            "SELECT id, name, key_hash, order_mode FROM api_keys WHERE lookup_hmac = ?1",
            [lookup],
            |r| {
                Ok(ApiKeyRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    key_hash: r.get(2)?,
                    order_mode: r.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Replace the user's key with `api_key`. Keeps the order mode.
pub fn upsert_for_user(
    conn: &Connection,
    security: &SecurityManager,
    username: &str,
    api_key: &str,
) -> Result<i64> {
    let order_mode = get_order_mode(conn)?.unwrap_or_else(|| "auto".to_string());
    let key_hash = security.hash_password(api_key)?;
    let lookup = security.api_key_lookup(api_key)?;
    let (ct, nonce) = security.encrypt(api_key, &aad(username))?;
    conn.execute("DELETE FROM api_keys", [])?;
    conn.execute(
        "INSERT INTO api_keys (name, key_hash, encrypted_key, nonce, permissions, lookup_hmac, order_mode)
         VALUES (?1, ?2, ?3, ?4, 'all', ?5, ?6)",
        params![username, key_hash, ct, nonce, lookup, order_mode],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The key in plaintext, for the signed-in session only.
pub fn get_plaintext(conn: &Connection, security: &SecurityManager) -> Result<Option<Secret>> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT name, encrypted_key, nonce FROM api_keys ORDER BY id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match row {
        Some((name, ct, nonce)) => Ok(Some(security.decrypt(&ct, &nonce, &aad(&name))?)),
        None => Ok(None),
    }
}

pub fn has_key(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM api_keys", [], |r| r.get(0))?;
    Ok(n > 0)
}

pub fn get_order_mode(conn: &Connection) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT order_mode FROM api_keys ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn set_order_mode(conn: &Connection, mode: &str) -> Result<bool> {
    Ok(conn.execute("UPDATE api_keys SET order_mode = ?1", [mode])? > 0)
}

pub fn touch(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE api_keys SET last_used_at = datetime('now') WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

pub fn delete_all(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM api_keys", [])?;
    Ok(())
}
