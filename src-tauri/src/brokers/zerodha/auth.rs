//! Kite session exchange (web `api/auth_api.py`).

use super::mapping::KiteEnvelope;
use super::{kite_error, ZerodhaBroker};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// `sha256(api_key + request_token + api_secret)` as hex.
pub fn checksum(api_key: &str, request_token: &str, api_secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(api_key.as_bytes());
    h.update(request_token.as_bytes());
    h.update(api_secret.as_bytes());
    hex::encode(h.finalize())
}

#[derive(Deserialize)]
struct Session {
    access_token: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    user_name: Option<String>,
}

pub async fn authenticate(b: &ZerodhaBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let request_token = creds
        .request_token
        .or(creds.auth_code)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Zerodha did not return a login code. Start the Zerodha login again.".into(),
            )
        })?;
    let api_secret = creds.api_secret.filter(|s| !s.is_empty()).ok_or_else(|| {
        AppError::Validation(
            "Your Zerodha API secret is missing. Add it on the broker settings page.".into(),
        )
    })?;
    if creds.api_key.is_empty() {
        return Err(AppError::Validation(
            "Your Zerodha API key is missing. Add it on the broker settings page.".into(),
        ));
    }
    let sum = checksum(&creds.api_key, &request_token, &api_secret);
    let form = [
        ("api_key", creds.api_key.as_str()),
        ("request_token", request_token.as_str()),
        ("checksum", sum.as_str()),
    ];
    let resp = b
        .http
        .post(format!("{}/session/token", b.base_url))
        .header("X-Kite-Version", "3")
        .form(&form)
        .send()
        .await?;
    let (_, env): (_, KiteEnvelope<Session>) = http::read_json("zerodha", resp).await?;
    if env.status != "success" {
        tracing::warn!(error_type = %env.error_type, "Zerodha login refused");
        return Err(match env.error_type.as_str() {
            "TokenException" => AppError::Auth(
                "The Zerodha login code has expired or was already used. Log in to Zerodha again."
                    .into(),
            ),
            "InputException" => AppError::Auth(
                "Zerodha did not accept your API key or secret. Check them on the broker settings page."
                    .into(),
            ),
            t => match kite_error(t, &env.message) {
                AppError::Broker(m) => AppError::Auth(m),
                e => e,
            },
        });
    }
    let session = env.data.ok_or_else(|| {
        AppError::Auth(
            "Zerodha accepted the login but returned no session. Log in to Zerodha again.".into(),
        )
    })?;
    Ok(AuthResponse {
        // web stores `f"{BROKER_API_KEY}:{access_token}"`
        auth_token: format!("{}:{}", creds.api_key, session.access_token),
        // The feed authenticates with the access token from `auth_token`.
        feed_token: None,
        user_id: session.user_id,
        user_name: session.user_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_kite_docs_construction() {
        // sha256("abc" + "def" + "ghi")
        assert_eq!(
            checksum("abc", "def", "ghi"),
            "19cc02f26df43cc571bc9ed7b0c4d29224a3ec229529221725ef76d021c8326f"
        );
    }
}
