//! Groww sign-in (web `api/auth_api.py`, plus the TOTP and pasted-token
//! variants Groww's API offers).
//!
//! Variant order, from what the trader filled in:
//! 1. a TOTP code: `key_type: totp` with the TOTP API key;
//! 2. a pasted access token (the form's token field): validated with a
//!    funds call;
//! 3. an API secret: the web's approval checksum flow,
//!    `sha256(secret + epoch_seconds)`;
//! 4. an API key that is itself an access token (a JWT) with no secret.
//!
//! The stored session token is the raw Groww token.

use super::{error_message, Category, GrowwCore};
use crate::brokers::common::http;
use crate::brokers::types::AuthToken;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use reqwest::Method;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// `sha256(api_secret + timestamp)` as lowercase hex (web
/// `generate_checksum`).
pub fn checksum(api_secret: &str, timestamp: &str) -> String {
    let mut h = Sha256::new();
    h.update(api_secret.as_bytes());
    h.update(timestamp.as_bytes());
    hex::encode(h.finalize())
}

/// Whether a string has the shape of a JWT (`eyJ...` with three parts).
pub fn looks_like_jwt(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("eyJ") && s.matches('.').count() == 2
}

/// The four ways in, decided from the filled-in fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Variant {
    Totp { api_key: String, totp: String },
    PastedToken(String),
    Approval { api_key: String, api_secret: String },
}

pub fn choose_variant(creds: &BrokerCredentials) -> Result<Variant> {
    let nonempty = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let api_key = creds.api_key.trim().to_string();
    if let Some(totp) = nonempty(&creds.totp) {
        if api_key.is_empty() {
            return Err(AppError::Validation(
                "Your Groww TOTP API key is missing. Add it on the broker settings page.".into(),
            ));
        }
        return Ok(Variant::Totp { api_key, totp });
    }
    if let Some(token) = nonempty(&creds.password) {
        return Ok(Variant::PastedToken(token));
    }
    if let Some(api_secret) = nonempty(&creds.api_secret) {
        if api_key.is_empty() {
            return Err(AppError::Validation(
                "Your Groww API key is missing. Add it on the broker settings page.".into(),
            ));
        }
        return Ok(Variant::Approval {
            api_key,
            api_secret,
        });
    }
    if looks_like_jwt(&api_key) {
        return Ok(Variant::PastedToken(api_key));
    }
    Err(AppError::Validation(
        "Groww needs one of: your API key and API secret on the broker settings page, a TOTP code for a TOTP API key, or an access token pasted from Groww."
            .into(),
    ))
}

/// Exchange an API key for an access token (`/v1/token/api/access`).
async fn token_exchange(core: &GrowwCore, api_key: &str, body: Value) -> Result<String> {
    let resp = core
        .http
        .post(format!("{}/v1/token/api/access", core.base_url))
        .timeout(http::REQUEST_TIMEOUT)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status.is_success() {
        if let Some(t) = v
            .get("token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            return Ok(t.to_string());
        }
        tracing::warn!("Groww login answered without a token");
        return Err(AppError::Auth(
            "Groww accepted the login but returned no session. Try logging in again.".into(),
        ));
    }
    tracing::warn!(
        status = status.as_u16(),
        "Groww login refused: {}",
        error_message(&v)
    );
    if status.is_server_error() {
        return Err(AppError::Broker(
            "Groww's login service is not responding normally. Try again shortly.".into(),
        ));
    }
    Err(AppError::Auth(
        "Groww did not accept the login. Check your API key and secret (or TOTP) on the broker settings page and try again."
            .into(),
    ))
}

/// A pasted token is checked with a funds call before it is stored.
async fn validate_token(core: &GrowwCore, token: &str) -> Result<()> {
    let auth = AuthToken::new(token);
    let r = core
        .send(
            Method::GET,
            "/v1/margins/detail/user",
            &auth,
            None,
            Category::Other,
            false,
        )
        .await
        .map_err(|e| match e {
            AppError::Auth(_) => AppError::Auth(
                "Groww did not accept this access token. Generate a new one in Groww and paste it again."
                    .into(),
            ),
            other => other,
        })?;
    if r.is_success() {
        Ok(())
    } else {
        Err(AppError::Auth(
            "Groww did not accept this access token. Generate a new one in Groww and paste it again."
                .into(),
        ))
    }
}

pub async fn authenticate(core: &GrowwCore, creds: BrokerCredentials) -> Result<AuthResponse> {
    let token = match choose_variant(&creds)? {
        Variant::Totp { api_key, totp } => {
            token_exchange(core, &api_key, json!({"key_type": "totp", "totp": totp})).await?
        }
        Variant::PastedToken(t) => {
            validate_token(core, &t).await?;
            t
        }
        Variant::Approval {
            api_key,
            api_secret,
        } => {
            let ts = chrono::Utc::now().timestamp().to_string();
            let sum = checksum(&api_secret, &ts);
            token_exchange(
                core,
                &api_key,
                json!({"key_type": "approval", "checksum": sum, "timestamp": ts}),
            )
            .await?
        }
    };
    Ok(AuthResponse {
        auth_token: token,
        feed_token: None,
        // Groww's token response carries no user id.
        user_id: creds.client_id.unwrap_or_default(),
        user_name: None,
    })
}
