//! Data migrations that need the key material, run after the security manager
//! is unlocked.
//!
//! `043_crypto_aad_v1` re-encrypts every ciphertext written before associated
//! data existed (auth tokens, broker credentials, the API key) and fills the
//! API key's HMAC lookup column. A row that already decrypts with AAD is left
//! as it is, so the migration is safe to resume after an interruption and
//! never touches rows written by this version. A row that decrypts with
//! neither is unrecoverable and is removed (the trader re-enters it).

use super::migrations;
use super::SqliteDb;
use crate::error::Result;
use crate::security::crypto::Aad;
use crate::security::{Secret, SecurityManager};
use rusqlite::{params, Connection};

pub const AAD_MIGRATION: &str = "043_crypto_aad_v1";

/// (row key, ciphertext, nonce, second ciphertext, second nonce)
type CipherRow = (String, String, String, Option<String>, Option<String>);

pub fn run(db: &SqliteDb, security: &SecurityManager) -> Result<()> {
    if !security.is_unlocked() {
        return Ok(());
    }
    let conn = db.conn()?;
    if migrations::is_applied(&conn, AAD_MIGRATION)? {
        if security.legacy_migration_pending() {
            security.finish_legacy_migration()?;
        }
        return Ok(());
    }
    tracing::info!("Re-encrypting stored secrets with associated data");
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        migrate_auth(&conn, security)?;
        migrate_credentials(&conn, security)?;
        migrate_api_keys(&conn, security)?;
        migrations::mark_applied(&conn, AAD_MIGRATION)
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    drop(conn);
    security.finish_legacy_migration()?;
    Ok(())
}

enum Outcome {
    AlreadyCurrent,
    Reencrypted(String, String, Secret),
    Unreadable,
}

fn upgrade(security: &SecurityManager, ct: &str, nonce: &str, aad: &Aad) -> Result<Outcome> {
    if ct.is_empty() {
        return Ok(Outcome::AlreadyCurrent);
    }
    if security.decrypt(ct, nonce, aad).is_ok() {
        return Ok(Outcome::AlreadyCurrent);
    }
    match security.decrypt_legacy(ct, nonce) {
        Ok(plain) => {
            let (c, n) = security.encrypt(plain.expose(), aad)?;
            Ok(Outcome::Reencrypted(c, n, plain))
        }
        Err(_) => Ok(Outcome::Unreadable),
    }
}

fn migrate_auth(conn: &Connection, security: &SecurityManager) -> Result<()> {
    let rows: Vec<CipherRow> = {
        let mut stmt = conn.prepare(
            "SELECT broker_id, auth_token_encrypted, auth_token_nonce, feed_token_encrypted, feed_token_nonce FROM auth",
        )?;
        let r = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        r
    };
    for (broker, ct, nonce, fct, fnonce) in rows {
        match upgrade(
            security,
            &ct,
            &nonce,
            &Aad::new("auth", "auth_token", &broker),
        )? {
            Outcome::AlreadyCurrent => {}
            Outcome::Reencrypted(c, n, _) => {
                conn.execute(
                    "UPDATE auth SET auth_token_encrypted = ?1, auth_token_nonce = ?2 WHERE broker_id = ?3",
                    params![c, n, broker],
                )?;
            }
            Outcome::Unreadable => {
                tracing::warn!("Dropping unreadable stored broker session for {}", broker);
                conn.execute("DELETE FROM auth WHERE broker_id = ?1", [&broker])?;
                continue;
            }
        }
        if let (Some(fc), Some(fnn)) = (fct, fnonce) {
            match upgrade(
                security,
                &fc,
                &fnn,
                &Aad::new("auth", "feed_token", &broker),
            )? {
                Outcome::Reencrypted(c, n, _) => {
                    conn.execute(
                        "UPDATE auth SET feed_token_encrypted = ?1, feed_token_nonce = ?2 WHERE broker_id = ?3",
                        params![c, n, broker],
                    )?;
                }
                Outcome::Unreadable => {
                    conn.execute(
                        "UPDATE auth SET feed_token_encrypted = NULL, feed_token_nonce = NULL WHERE broker_id = ?1",
                        [&broker],
                    )?;
                }
                Outcome::AlreadyCurrent => {}
            }
        }
    }
    Ok(())
}

fn migrate_credentials(conn: &Connection, security: &SecurityManager) -> Result<()> {
    let rows: Vec<CipherRow> = {
        let mut stmt = conn.prepare(
            "SELECT broker_id, api_key_encrypted, api_key_nonce, api_secret_encrypted, api_secret_nonce FROM broker_credentials",
        )?;
        let r = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        r
    };
    for (broker, kct, kn, sct, sn) in rows {
        let aad_k = Aad::new("broker_credentials", "api_key", &broker);
        match upgrade(security, &kct, &kn, &aad_k)? {
            Outcome::AlreadyCurrent => {}
            Outcome::Reencrypted(c, n, _) => {
                conn.execute(
                    "UPDATE broker_credentials SET api_key_encrypted = ?1, api_key_nonce = ?2 WHERE broker_id = ?3",
                    params![c, n, broker],
                )?;
            }
            Outcome::Unreadable => {
                tracing::warn!("Dropping unreadable broker credentials for {}", broker);
                conn.execute(
                    "DELETE FROM broker_credentials WHERE broker_id = ?1",
                    [&broker],
                )?;
                conn.execute(
                    "DELETE FROM configured_brokers WHERE broker_id = ?1",
                    [&broker],
                )?;
                continue;
            }
        }
        if let (Some(c0), Some(n0)) = (sct, sn) {
            let aad_s = Aad::new("broker_credentials", "api_secret", &broker);
            match upgrade(security, &c0, &n0, &aad_s)? {
                Outcome::Reencrypted(c, n, _) => {
                    conn.execute(
                        "UPDATE broker_credentials SET api_secret_encrypted = ?1, api_secret_nonce = ?2 WHERE broker_id = ?3",
                        params![c, n, broker],
                    )?;
                }
                Outcome::Unreadable => {
                    conn.execute(
                        "UPDATE broker_credentials SET api_secret_encrypted = NULL, api_secret_nonce = NULL WHERE broker_id = ?1",
                        [&broker],
                    )?;
                }
                Outcome::AlreadyCurrent => {}
            }
        }
    }
    Ok(())
}

fn migrate_api_keys(conn: &Connection, security: &SecurityManager) -> Result<()> {
    let username: Option<String> = super::user::find_first(conn)?.map(|u| u.username);
    let rows: Vec<(i64, String, String, String, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, name, encrypted_key, nonce, lookup_hmac FROM api_keys ORDER BY id",
        )?;
        let r = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        r
    };
    let mut kept = false;
    for (id, name, ct, nonce, lookup) in rows {
        // One key per user, as on the web: keep the oldest readable key.
        if kept {
            conn.execute("DELETE FROM api_keys WHERE id = ?1", [id])?;
            continue;
        }
        let aad = super::api_keys::aad(&name);
        let plain = match upgrade(security, &ct, &nonce, &aad)? {
            Outcome::AlreadyCurrent => {
                if lookup.is_some() {
                    kept = true;
                    continue;
                }
                security.decrypt(&ct, &nonce, &aad)?
            }
            Outcome::Reencrypted(_, _, p) => p,
            Outcome::Unreadable => {
                conn.execute("DELETE FROM api_keys WHERE id = ?1", [id])?;
                continue;
            }
        };
        // Re-key the row under the account name so the API key page finds it.
        let new_name = username.clone().unwrap_or(name);
        let (c, n) = security.encrypt(plain.expose(), &super::api_keys::aad(&new_name))?;
        let lookup = security.api_key_lookup(plain.expose())?;
        conn.execute(
            "UPDATE api_keys SET name = ?1, encrypted_key = ?2, nonce = ?3, lookup_hmac = ?4 WHERE id = ?5",
            params![new_name, c, n, lookup, id],
        )?;
        kept = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Upgrade of a database exactly as the previous build left it: legacy
    //! schema (001-036), rows encrypted without AAD, keys in `secrets.dat`.
    use crate::security::crypto::DataCipher;
    use crate::security::{hashing, keystore::MemoryKeyStore, legacy};
    use crate::state::{AppState, OpenOptions};
    use rusqlite::{params, Connection};
    use std::sync::Arc;

    const KEY: [u8; 32] = [11u8; 32];
    const PEPPER: [u8; 32] = [22u8; 32];

    fn legacy_db(dir: &std::path::Path, api_key: &str) {
        legacy::write_for_test(dir, &KEY, &PEPPER);
        let old = DataCipher::new(&KEY).unwrap();
        let conn = Connection::open(dir.join("openalgo.db")).unwrap();
        crate::db::sqlite::migrations::run_legacy_schema(&conn).unwrap();
        let pw_hash = hashing::hash_password(&PEPPER, "Legacy@123").unwrap();
        conn.execute(
            "INSERT INTO users (username, password_hash) VALUES ('old', ?1)",
            [pw_hash],
        )
        .unwrap();
        let (tok, tn) = old.encrypt_legacy("old-broker-token");
        let (feed, fnn) = old.encrypt_legacy("old-feed");
        conn.execute(
            "INSERT INTO auth (broker_id, auth_token_encrypted, auth_token_nonce, feed_token_encrypted, feed_token_nonce, updated_at)
             VALUES ('zerodha', ?1, ?2, ?3, ?4, '2026-10-01 04:00:00')",
            params![tok, tn, feed, fnn],
        )
        .unwrap();
        let (k, kn) = old.encrypt_legacy("brokerkey");
        let (s, sn) = old.encrypt_legacy("brokersecret");
        conn.execute(
            "INSERT INTO broker_credentials (broker_id, api_key_encrypted, api_key_nonce, api_secret_encrypted, api_secret_nonce)
             VALUES ('zerodha', ?1, ?2, ?3, ?4)",
            params![k, kn, s, sn],
        )
        .unwrap();
        let key_hash = hashing::hash_password(&PEPPER, api_key).unwrap();
        let (ek, ekn) = old.encrypt_legacy(api_key);
        conn.execute(
            "INSERT INTO api_keys (name, key_hash, encrypted_key, nonce) VALUES ('default', ?1, ?2, ?3)",
            params![key_hash, ek, ekn],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO analyzer_logs (api_type, request_data, response_data) VALUES ('placeorder', '{\"apikey\":\"x\",\"symbol\":\"SBIN\"}', '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE settings SET webhook_port = 6000, webhook_host = '127.0.0.1', ngrok_url = 'https://me.ngrok.app' WHERE id = 1",
            [],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn upgrade_from_previous_build_keeps_every_secret_readable() {
        let dir = tempfile::tempdir().unwrap();
        let api_key = "a".repeat(64);
        legacy_db(dir.path(), &api_key);

        let store = Arc::new(MemoryKeyStore::new());
        let ctx = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: store.clone(),
                clock: Arc::new(crate::clock::SystemClock),
                brokers: Arc::new(crate::brokers::BrokerRegistry::with(vec![])),
            },
        )
        .unwrap();

        // Keys moved into the keystore, the XOR file is gone.
        assert!(!legacy::path(dir.path()).exists());
        assert!(
            crate::security::keystore::KeyStore::get(&*store, "data-key")
                .unwrap()
                .is_some()
        );

        let conn = ctx.sqlite.conn().unwrap();
        // Old password still works (same pepper).
        let row = crate::db::sqlite::user::find_by_username(&conn, "old")
            .unwrap()
            .unwrap();
        assert!(ctx
            .security
            .verify_password("Legacy@123", &row.password_hash)
            .unwrap());

        // Broker credentials re-encrypted with AAD and readable.
        let creds = crate::db::sqlite::credentials::load(&conn, &ctx.security, "zerodha")
            .unwrap()
            .unwrap();
        assert_eq!(creds.api_key.expose(), "brokerkey");
        assert_eq!(creds.api_secret.unwrap().expose(), "brokersecret");

        // API key: lookup index filled, verifies, re-keyed to the account.
        drop(conn);
        assert!(crate::services::apikey_service::ApiKeyService::is_valid(
            &ctx, &api_key
        ));
        assert_eq!(
            crate::services::apikey_service::ApiKeyService::current(&ctx)
                .unwrap()
                .unwrap()
                .expose(),
            api_key
        );

        // Auth row: re-encrypted, authenticated_at backfilled from updated_at
        // (an old token, so it is not resumed).
        let conn = ctx.sqlite.conn().unwrap();
        let at: String = conn
            .query_row(
                "SELECT authenticated_at FROM auth WHERE broker_id = 'zerodha'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(at, "2026-10-01T04:00:00Z");
        let s = crate::db::sqlite::auth::latest_active(&conn, &ctx.security)
            .unwrap()
            .unwrap();
        assert_eq!(s.auth_token.expose(), "old-broker-token");
        assert_eq!(s.feed_token.unwrap().expose(), "old-feed");

        // Settings backfilled from the old webhook settings, not defaults.
        let (port, host_server): (i64, Option<String>) = conn
            .query_row(
                "SELECT http_port, host_server FROM settings WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(port, 6000);
        assert_eq!(host_server.as_deref(), Some("https://me.ngrok.app"));
        drop(conn);

        // Sandbox logs copied to logs.db without the API key.
        assert_eq!(ctx.logs.count_analyzer_logs().unwrap(), 1);

        // A second start is a no-op (idempotent).
        let ctx2 = AppState::open(
            dir.path(),
            OpenOptions {
                keystore: store,
                clock: Arc::new(crate::clock::SystemClock),
                brokers: Arc::new(crate::brokers::BrokerRegistry::with(vec![])),
            },
        )
        .unwrap();
        assert_eq!(ctx2.logs.count_analyzer_logs().unwrap(), 1);
        let conn = ctx2.sqlite.conn().unwrap();
        let creds = crate::db::sqlite::credentials::load(&conn, &ctx2.security, "zerodha")
            .unwrap()
            .unwrap();
        assert_eq!(creds.api_key.expose(), "brokerkey");
        drop(conn);
        ctx.shutdown().await;
        ctx2.shutdown().await;
    }

    #[test]
    fn swapped_ciphertexts_do_not_decrypt_after_upgrade() {
        let sec = crate::security::SecurityManager::for_tests();
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::sqlite::SqliteDb::new(&dir.path().join("x.db")).unwrap();
        let conn = db.conn().unwrap();
        crate::db::sqlite::credentials::save(
            &conn,
            &sec,
            "fyers",
            crate::db::sqlite::credentials::CredentialUpdate {
                api_key: Some("KEY-100".into()),
                api_secret: Some("SECRET".into()),
                ..Default::default()
            },
        )
        .unwrap();
        // An attacker with write access swaps the secret into the key column.
        conn.execute_batch(
            "UPDATE broker_credentials SET api_key_encrypted = api_secret_encrypted, api_key_nonce = api_secret_nonce",
        )
        .unwrap();
        assert!(crate::db::sqlite::credentials::load(&conn, &sec, "fyers").is_err());
    }
}
