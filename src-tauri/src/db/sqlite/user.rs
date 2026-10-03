//! The single OpenAlgo user (web `users` table equivalent).

use crate::error::Result;
use crate::security::crypto::Aad;
use crate::security::{Secret, SecurityManager};
use rusqlite::{params, Connection, OptionalExtension, Row};

#[derive(Clone)]
pub struct UserRow {
    pub id: i64,
    pub username: String,
    pub email: Option<String>,
    pub password_hash: String,
    totp_secret_encrypted: Option<String>,
    totp_nonce: Option<String>,
    pub totp_enabled: bool,
    pub totp_required_for_login: bool,
    pub totp_required_for_password_reset: bool,
    pub totp_required_for_mcp: bool,
}

impl std::fmt::Debug for UserRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserRow")
            .field("id", &self.id)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TwoFactorFlags {
    pub enabled: bool,
    pub login: bool,
    pub password_reset: bool,
    pub mcp: bool,
}

fn aad(username: &str) -> Aad {
    Aad::new("users", "totp_secret", username)
}

const COLS: &str = "id, username, email, password_hash, totp_secret_encrypted, totp_nonce, \
    totp_enabled, totp_required_for_login, totp_required_for_password_reset, totp_required_for_mcp";

fn map(row: &Row<'_>) -> rusqlite::Result<UserRow> {
    Ok(UserRow {
        id: row.get(0)?,
        username: row.get(1)?,
        email: row.get(2)?,
        password_hash: row.get(3)?,
        totp_secret_encrypted: row.get(4)?,
        totp_nonce: row.get(5)?,
        totp_enabled: row.get::<_, i64>(6)? != 0,
        totp_required_for_login: row.get::<_, i64>(7)? != 0,
        totp_required_for_password_reset: row.get::<_, i64>(8)? != 0,
        totp_required_for_mcp: row.get::<_, i64>(9)? != 0,
    })
}

impl UserRow {
    /// Decrypt the TOTP secret. `None` for accounts created before TOTP
    /// existed in the desktop build.
    pub fn totp_secret(&self, security: &SecurityManager) -> Result<Option<Secret>> {
        match (&self.totp_secret_encrypted, &self.totp_nonce) {
            (Some(ct), Some(n)) if !ct.is_empty() => {
                Ok(Some(security.decrypt(ct, n, &aad(&self.username))?))
            }
            _ => Ok(None),
        }
    }

    pub fn is_totp_required_for(&self, purpose: &str) -> bool {
        self.totp_enabled
            && match purpose {
                "login" => self.totp_required_for_login,
                "password_reset" => self.totp_required_for_password_reset,
                "mcp" => self.totp_required_for_mcp,
                _ => false,
            }
    }
}

pub fn has_user(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
    Ok(n > 0)
}

pub fn find_first(conn: &Connection) -> Result<Option<UserRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {} FROM users ORDER BY id LIMIT 1", COLS),
            [],
            map,
        )
        .optional()?)
}

pub fn find_by_username(conn: &Connection, username: &str) -> Result<Option<UserRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {} FROM users WHERE username = ?1", COLS),
            [username],
            map,
        )
        .optional()?)
}

pub fn find_by_email(conn: &Connection, email: &str) -> Result<Option<UserRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {} FROM users WHERE email = ?1 COLLATE NOCASE", COLS),
            [email],
            map,
        )
        .optional()?)
}

pub fn insert(
    conn: &Connection,
    security: &SecurityManager,
    username: &str,
    email: &str,
    password_hash: &str,
    totp_secret: &str,
) -> Result<i64> {
    let (ct, nonce) = security.encrypt(totp_secret, &aad(username))?;
    conn.execute(
        "INSERT INTO users (username, email, password_hash, totp_secret_encrypted, totp_nonce)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![username, email, password_hash, ct, nonce],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Give an account created before TOTP existed a secret.
pub fn set_totp_secret(
    conn: &Connection,
    security: &SecurityManager,
    username: &str,
    secret: &str,
) -> Result<()> {
    let (ct, nonce) = security.encrypt(secret, &aad(username))?;
    conn.execute(
        "UPDATE users SET totp_secret_encrypted = ?1, totp_nonce = ?2, updated_at = datetime('now')
         WHERE username = ?3",
        params![ct, nonce, username],
    )?;
    Ok(())
}

pub fn update_password_hash(conn: &Connection, id: i64, hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE users SET password_hash = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![hash, id],
    )?;
    Ok(())
}

pub fn set_two_factor(conn: &Connection, id: i64, f: TwoFactorFlags) -> Result<()> {
    conn.execute(
        "UPDATE users SET totp_enabled = ?1, totp_required_for_login = ?2,
            totp_required_for_password_reset = ?3, totp_required_for_mcp = ?4,
            updated_at = datetime('now') WHERE id = ?5",
        params![f.enabled, f.login, f.password_reset, f.mcp, id],
    )?;
    Ok(())
}
