//! OpenAlgo API key: verification for `/api/v1`, regeneration and order mode.
//!
//! Verification: SHA-256 of the presented key indexes a bounded cache
//! (positive and negative results, 5 minutes). On a miss the HMAC index finds
//! the one candidate row and Argon2 verifies it. Regenerating the key clears
//! the cache.

use crate::db::sqlite::api_keys;
use crate::error::Result;
use crate::security::Secret;
use crate::state::AppState;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const CACHE_CAP: usize = 1024;
pub const CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Default)]
pub struct ApiKeyCache {
    map: Mutex<HashMap<[u8; 32], (bool, Instant)>>,
}

impl ApiKeyCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn digest(key: &str) -> [u8; 32] {
        Sha256::digest(key.as_bytes()).into()
    }

    pub fn get(&self, key: &str, now: Instant) -> Option<bool> {
        let mut map = self.map.lock();
        let d = Self::digest(key);
        match map.get(&d) {
            Some((valid, exp)) if *exp > now => Some(*valid),
            Some(_) => {
                map.remove(&d);
                None
            }
            None => None,
        }
    }

    pub fn put(&self, key: &str, valid: bool, now: Instant) {
        let mut map = self.map.lock();
        if map.len() >= CACHE_CAP {
            map.retain(|_, (_, exp)| *exp > now);
            while map.len() >= CACHE_CAP {
                let oldest = map.iter().min_by_key(|(_, (_, e))| *e).map(|(k, _)| *k);
                match oldest {
                    Some(k) => {
                        map.remove(&k);
                    }
                    None => break,
                }
            }
        }
        map.insert(Self::digest(key), (valid, now + CACHE_TTL));
    }

    pub fn clear(&self) {
        self.map.lock().clear();
    }

    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct ApiKeyService;

impl ApiKeyService {
    /// Whether `api_key` is the stored OpenAlgo key.
    pub fn is_valid(state: &AppState, api_key: &str) -> bool {
        if api_key.is_empty() {
            return false;
        }
        let now = Instant::now();
        if let Some(v) = state.api_keys.get(api_key, now) {
            return v;
        }
        if !state.security.is_unlocked() {
            return false;
        }
        let valid = match Self::verify_uncached(state, api_key) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("API key check failed: {}", e);
                return false;
            }
        };
        state.api_keys.put(api_key, valid, now);
        valid
    }

    fn verify_uncached(state: &AppState, api_key: &str) -> Result<bool> {
        let lookup = state.security.api_key_lookup(api_key)?;
        let row = {
            let conn = state.sqlite.conn()?;
            api_keys::find_by_lookup(&conn, &lookup)?
        };
        let Some(row) = row else {
            return Ok(false);
        };
        let ok = state.security.verify_password(api_key, &row.key_hash)?;
        if ok {
            if let Ok(conn) = state.sqlite.conn() {
                let _ = api_keys::touch(&conn, row.id);
            }
        }
        Ok(ok)
    }

    /// Generate and store a new key for `username`; returns it once.
    pub fn regenerate(state: &AppState, username: &str) -> Result<Secret> {
        let key = api_keys::generate_api_key();
        {
            let conn = state.sqlite.conn()?;
            api_keys::upsert_for_user(&conn, &state.security, username, &key)?;
        }
        state.api_keys.clear();
        Ok(Secret::new(key))
    }

    /// The key for the signed-in session (API key page, session status).
    pub fn current(state: &AppState) -> Result<Option<Secret>> {
        let conn = state.sqlite.conn()?;
        api_keys::get_plaintext(&conn, &state.security)
    }

    pub fn order_mode(state: &AppState) -> Result<String> {
        let conn = state.sqlite.conn()?;
        Ok(api_keys::get_order_mode(&conn)?.unwrap_or_else(|| "auto".into()))
    }

    pub fn set_order_mode(state: &AppState, mode: &str) -> Result<bool> {
        let conn = state.sqlite.conn()?;
        api_keys::set_order_mode(&conn, mode)
    }
}

/// Used by the older services that re-check the key.
pub fn require_valid(state: &AppState, api_key: &str) -> Result<()> {
    if ApiKeyService::is_valid(state, api_key) {
        Ok(())
    } else {
        Err(crate::error::AppError::Auth(
            "Invalid openalgo apikey".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_expires_and_stays_bounded() {
        let c = ApiKeyCache::new();
        let t0 = Instant::now();
        c.put("k", true, t0);
        assert_eq!(c.get("k", t0), Some(true));
        assert_eq!(c.get("k", t0 + CACHE_TTL + Duration::from_secs(1)), None);
        for i in 0..(CACHE_CAP * 2) {
            c.put(&format!("k{}", i), false, t0);
        }
        assert!(c.len() <= CACHE_CAP);
        c.clear();
        assert!(c.is_empty());
    }
}
