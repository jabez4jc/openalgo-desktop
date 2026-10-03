//! SmartAPI password + TOTP login (web `api/auth_api.py`).

use super::{read_envelope, AngelBroker, Reply};
use crate::brokers::common::de::string_lenient;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;

pub const LOGIN_PATH: &str = "/rest/auth/angelbroking/user/v1/loginByPassword";

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct LoginData {
    #[serde(default, deserialize_with = "string_lenient")]
    jwtToken: String,
    #[serde(default, deserialize_with = "string_lenient")]
    feedToken: String,
}

pub async fn authenticate(b: &AngelBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let missing =
        |what: &str| AppError::Validation(format!("Enter your Angel One {} to log in.", what));
    let client_id = creds
        .client_id
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| missing("client ID"))?;
    let password = creds
        .password
        .filter(|s| !s.is_empty())
        .ok_or_else(|| missing("PIN"))?;
    let totp = creds
        .totp
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| missing("TOTP"))?;
    if creds.api_key.is_empty() {
        return Err(AppError::Validation(
            "Your Angel One API key is missing. Add it on the broker settings page.".into(),
        ));
    }
    let body = json!({"clientcode": client_id, "password": password, "totp": totp});
    let resp = b
        .request(Method::POST, LOGIN_PATH, &creds.api_key, None)
        .json(&body)
        .send()
        .await?;
    let env = match read_envelope::<LoginData>(resp).await? {
        Reply::Ok(env) => env,
        Reply::RateLimited => {
            return Err(AppError::Auth(
                "Angel One is limiting login attempts right now. Wait a minute and try again."
                    .into(),
            ))
        }
        Reply::Denied => {
            return Err(AppError::Auth(
                "Angel One did not accept your API key. Check it on the broker settings page."
                    .into(),
            ))
        }
    };
    // web: success is `data.jwtToken` being present.
    match env.data {
        Some(d) if !d.jwtToken.is_empty() => Ok(AuthResponse {
            // Stored as `api_key:jwt` so every call can send X-PrivateKey.
            auth_token: format!("{}:{}", creds.api_key, d.jwtToken),
            feed_token: (!d.feedToken.is_empty()).then_some(d.feedToken),
            user_id: client_id,
            user_name: None,
        }),
        _ => {
            tracing::warn!(code = %env.errorcode, "Angel One login refused");
            Err(AppError::Auth(if env.message.trim().is_empty() {
                "Angel One did not accept the login. Check your client ID, PIN and TOTP.".into()
            } else {
                env.message.trim().to_string()
            }))
        }
    }
}
