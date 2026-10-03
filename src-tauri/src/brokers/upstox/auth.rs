//! Upstox OAuth code exchange (web `api/auth_api.py`).
//!
//! `POST /v2/login/authorization/token`, form-encoded `code`, `client_id`,
//! `client_secret`, `redirect_uri`, `grant_type=authorization_code`. Upstox
//! refuses the exchange unless `redirect_uri` is byte-identical to the one
//! on the authorize URL, so the catalogue records the redirect it built the
//! login URL with (`remember_redirect_uri`) and the exchange reuses it.
//! (`BrokerCredentials` has no redirect field yet; when it gains one, that
//! wins.)

use super::{mapping, UpstoxBroker};
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use parking_lot::RwLock;
use serde_json::Value;

/// The web convention for the shipped default port.
pub const DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:5000/upstox/callback";

static REDIRECT_URI: RwLock<Option<String>> = RwLock::new(None);

/// Record the redirect URI the Upstox login URL was built with.
pub fn remember_redirect_uri(uri: &str) {
    let uri = uri.trim();
    if !uri.is_empty() {
        *REDIRECT_URI.write() = Some(uri.to_string());
    }
}

/// The redirect URI for the code exchange.
pub fn redirect_uri() -> String {
    REDIRECT_URI
        .read()
        .clone()
        .unwrap_or_else(|| DEFAULT_REDIRECT_URI.to_string())
}

/// The form `authenticate_broker` posts.
pub fn token_form(
    code: &str,
    api_key: &str,
    api_secret: &str,
    redirect: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("code", code.to_string()),
        ("client_id", api_key.to_string()),
        ("client_secret", api_secret.to_string()),
        ("redirect_uri", redirect.to_string()),
        ("grant_type", "authorization_code".to_string()),
    ]
}

pub async fn authenticate(b: &UpstoxBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let code = creds
        .auth_code
        .or(creds.request_token)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Upstox did not return a login code. Start the Upstox login again.".into(),
            )
        })?;
    if creds.api_key.trim().is_empty() {
        return Err(AppError::Validation(
            "Your Upstox API key is missing. Add it on the broker settings page.".into(),
        ));
    }
    let secret = creds.api_secret.filter(|s| !s.is_empty()).ok_or_else(|| {
        AppError::Validation(
            "Your Upstox API secret is missing. Add it on the broker settings page.".into(),
        )
    })?;
    let form = token_form(&code, creds.api_key.trim(), &secret, &redirect_uri());
    let resp = b
        .http
        .post(b.api("/v2/login/authorization/token"))
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        let token = body
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                AppError::Auth(
                    "Upstox accepted the login but returned no session. Log in to Upstox again."
                        .into(),
                )
            })?;
        let s = |k: &str| {
            body.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_default()
        };
        return Ok(AuthResponse {
            auth_token: token.to_string(),
            feed_token: None,
            user_id: s("user_id"),
            user_name: Some(s("user_name")).filter(|n| !n.is_empty()),
        });
    }
    tracing::warn!(
        status = status.as_u16(),
        code = mapping::error_code(&body).unwrap_or_default(),
        "Upstox login refused"
    );
    let detail = mapping::error_text(&body).unwrap_or_default();
    Err(
        if detail.contains("redirect") || detail.contains("Redirect") {
            AppError::Auth(
            "Upstox refused the login because the redirect URL does not match your Upstox app. Set the app's redirect URL to the one shown in OpenAlgo, then log in again."
                .into(),
        )
        } else if detail.is_empty() {
            AppError::Auth("Upstox refused the login. Log in to Upstox again.".into())
        } else {
            AppError::Auth(format!(
                "Upstox refused the login: {}. Check your API key and secret, then log in again.",
                detail
            ))
        },
    )
}
