//! Optional outbound proxy for broker calls.
//!
//! Brokers only accept orders from an IP address the trader registered. A
//! trader on a changing home connection points the app at a proxy on a server
//! with a fixed address, and every broker REST call leaves from there. The
//! proxy address and username are stored as typed; the password is encrypted
//! and never returned. A change applies after the app restarts (see
//! `brokers::common::http`).

use crate::brokers::common::http;
use crate::error::{AppError, Result};
use crate::security::crypto::Aad;
use crate::security::SecurityManager;
use reqwest::Url;
use rusqlite::{params, Connection};

fn aad() -> Aad {
    Aad::new("settings", "broker_proxy_pass", "1")
}

/// What the settings page may see: never the password itself.
#[derive(Debug, PartialEq)]
pub struct ProxyView {
    pub url: Option<String>,
    pub username: Option<String>,
    pub has_password: bool,
}

pub struct ProxyUpdate {
    /// Empty turns the proxy off.
    pub url: String,
    pub username: String,
    /// `None` keeps the stored password, `Some("")` removes it.
    pub password: Option<String>,
}

fn invalid() -> AppError {
    AppError::Validation(
        "Enter the proxy address as http://host:port, for example http://203.0.113.10:3128. \
         Put the username and password in their own fields."
            .into(),
    )
}

/// Check a proxy address and return it without a trailing slash.
pub fn validate_url(s: &str) -> Result<String> {
    let u = Url::parse(s.trim()).map_err(|_| invalid())?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.path() != "/"
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(u.as_str().trim_end_matches('/').to_string())
}

/// The proxy URL with credentials attached (percent-encoded by `Url`).
fn with_credentials(url: &str, user: Option<&str>, pass: Option<&str>) -> Result<Url> {
    let mut u = Url::parse(url).map_err(|_| invalid())?;
    if let Some(user) = user.filter(|u| !u.is_empty()) {
        let _ = u.set_username(user);
        let _ = u.set_password(pass);
    }
    Ok(u)
}

pub fn get(conn: &Connection) -> Result<ProxyView> {
    Ok(conn.query_row(
        "SELECT broker_proxy_url, broker_proxy_user, broker_proxy_pass_encrypted IS NOT NULL
         FROM settings WHERE id = 1",
        [],
        |r| {
            Ok(ProxyView {
                url: r.get::<_, Option<String>>(0)?.filter(|s| !s.is_empty()),
                username: r.get::<_, Option<String>>(1)?.filter(|s| !s.is_empty()),
                has_password: r.get(2)?,
            })
        },
    )?)
}

pub fn save(conn: &Connection, sec: &SecurityManager, u: &ProxyUpdate) -> Result<()> {
    if u.url.trim().is_empty() {
        conn.execute(
            "UPDATE settings SET broker_proxy_url = NULL, broker_proxy_user = NULL,
                    broker_proxy_pass_encrypted = NULL, broker_proxy_pass_nonce = NULL
             WHERE id = 1",
            [],
        )?;
        return Ok(());
    }
    let url = validate_url(&u.url)?;
    let user = u.username.trim();
    conn.execute(
        "UPDATE settings SET broker_proxy_url = ?1, broker_proxy_user = ?2 WHERE id = 1",
        params![url, (!user.is_empty()).then_some(user)],
    )?;
    match u.password.as_deref() {
        None => {}
        Some("") => {
            conn.execute(
                "UPDATE settings SET broker_proxy_pass_encrypted = NULL,
                        broker_proxy_pass_nonce = NULL WHERE id = 1",
                [],
            )?;
        }
        Some(p) => {
            let (ct, nonce) = sec.encrypt(p, &aad())?;
            conn.execute(
                "UPDATE settings SET broker_proxy_pass_encrypted = ?1,
                        broker_proxy_pass_nonce = ?2 WHERE id = 1",
                params![ct, nonce],
            )?;
        }
    }
    Ok(())
}

/// Hand the saved proxy to the broker HTTP client. Called at start-up and
/// again after sign-in (password mode cannot decrypt before that). Does
/// nothing when no proxy is saved or the password cannot be read yet.
pub fn load(conn: &Connection, sec: &SecurityManager) {
    let row = conn.query_row(
        "SELECT broker_proxy_url, broker_proxy_user, broker_proxy_pass_encrypted,
                broker_proxy_pass_nonce FROM settings WHERE id = 1",
        [],
        |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        },
    );
    let Ok((Some(url), user, ct, nonce)) = row else {
        return;
    };
    let pass = match (ct, nonce) {
        (Some(ct), Some(nonce)) => match sec.decrypt(&ct, &nonce, &aad()) {
            Ok(p) => Some(p),
            // Locked until sign-in; load runs again then.
            Err(_) => return,
        },
        _ => None,
    };
    match with_credentials(&url, user.as_deref(), pass.as_ref().map(|p| p.expose())) {
        Ok(u) => {
            tracing::info!("Broker calls will go through the configured proxy");
            http::set_proxy(u);
        }
        Err(e) => tracing::warn!("Saved broker proxy is not usable: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn address_validation() {
        assert_eq!(
            validate_url(" http://140.245.16.208:3128/ ").unwrap(),
            "http://140.245.16.208:3128"
        );
        for bad in [
            "",
            "140.245.16.208:3128",
            "socks5://1.2.3.4:1080",
            "http://user:pw@1.2.3.4:3128",
            "http://1.2.3.4:3128/path",
        ] {
            assert!(validate_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn save_keeps_clears_and_never_exposes_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::sqlite::SqliteDb::new(&dir.path().join("x.db")).unwrap();
        let sec = SecurityManager::for_tests();
        let conn = db.conn().unwrap();
        let upd = |url: &str, user: &str, pass: Option<&str>| ProxyUpdate {
            url: url.into(),
            username: user.into(),
            password: pass.map(Into::into),
        };
        save(
            &conn,
            &sec,
            &upd("http://1.2.3.4:3128", "me", Some("secret")),
        )
        .unwrap();
        let v = get(&conn).unwrap();
        assert_eq!(v.url.as_deref(), Some("http://1.2.3.4:3128"));
        assert_eq!(v.username.as_deref(), Some("me"));
        assert!(v.has_password);
        let (ct, nonce): (String, String) = conn
            .query_row(
                "SELECT broker_proxy_pass_encrypted, broker_proxy_pass_nonce FROM settings",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(!ct.contains("secret"));
        assert_eq!(sec.decrypt(&ct, &nonce, &aad()).unwrap().expose(), "secret");
        // No password given: stored one is kept. Empty: removed.
        save(&conn, &sec, &upd("http://1.2.3.4:3128", "me", None)).unwrap();
        assert!(get(&conn).unwrap().has_password);
        save(&conn, &sec, &upd("http://1.2.3.4:3128", "me", Some(""))).unwrap();
        assert!(!get(&conn).unwrap().has_password);
        // Empty address turns the proxy off.
        save(&conn, &sec, &upd("", "me", None)).unwrap();
        assert_eq!(get(&conn).unwrap().url, None);
    }

    /// A request through the proxy reaches it as an absolute-URI request and
    /// carries the Basic credentials, including a password that needs
    /// percent-encoding.
    #[tokio::test]
    async fn client_sends_credentials_to_the_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap();
            s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        let url =
            with_credentials(&format!("http://{addr}"), Some("trader"), Some("p@ss:w/rd")).unwrap();
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::custom(move |_| Some(url.clone())))
            .build()
            .unwrap();
        let body = client
            .get("http://broker.example.test/orders")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
        let seen = server.join().unwrap().to_ascii_lowercase();
        assert!(
            seen.starts_with("get http://broker.example.test/orders"),
            "{seen}"
        );
        // base64("trader:p@ss:w/rd")
        assert!(
            seen.contains("proxy-authorization: basic dhjhzgvyonbac3m6dy9yza=="),
            "{seen}"
        );
    }
}
