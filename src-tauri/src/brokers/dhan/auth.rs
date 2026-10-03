//! Dhan sign-in (web `api/auth_api.py`, `blueprints/brlogin.py` dhan routes).
//!
//! Two paths, as on the web:
//! 1. Consent ("Individual" app) flow: `generate_consent` (POST
//!    `/app/generate-consent?client_id=` with `app_id`/`app_secret` headers)
//!    returns a `consentAppId`; the browser goes to
//!    `/login/consentApp-login?consentAppId=`; Dhan redirects back with
//!    `tokenId`, which `authenticate` consumes (POST
//!    `/app/consumeApp-consent?tokenId=`) for the access token and the
//!    `dhanClientId`.
//! 2. Pasted access token: a code longer than 100 characters is the access
//!    token itself (web `authenticate_broker`).
//!
//! Either way the token is then validated with `GET /v2/fundlimit` (web
//! `test_auth_token`) before the session is stored.
//!
//! The partner flow (`/partner/generate-consent`, `/consent-login`,
//! `/partner/consume-consent`) exists in the web's `dhan_sandbox` only and
//! is never called by its `authenticate_broker`; it is provided here for the
//! same reason.

use super::{split_api_key, DhanBroker, DhanSession, Variant};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use reqwest::{Method, StatusCode};
use serde_json::Value;

/// What `consumeApp-consent` returns.
#[derive(Clone)]
pub struct ConsentSession {
    pub access_token: String,
    pub client_id: Option<String>,
    pub client_name: Option<String>,
    pub expiry_time: Option<String>,
}

impl std::fmt::Debug for ConsentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsentSession")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| match x {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .filter(|x| !x.is_empty())
}

fn app_credentials(creds: &BrokerCredentials) -> Result<(Option<String>, String, String)> {
    let (client_id, app_id) = split_api_key(&creds.api_key);
    let app_id = app_id.ok_or_else(|| {
        AppError::Validation(
            "Enter your Dhan API key as client_id:::api_key in Profile, Broker Configuration."
                .into(),
        )
    })?;
    let secret = creds
        .api_secret
        .clone()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Your Dhan API secret is missing. Add it in Profile, Broker Configuration.".into(),
            )
        })?;
    let client_id = creds
        .client_id
        .clone()
        .filter(|c| !c.trim().is_empty())
        .or(client_id);
    Ok((client_id, app_id, secret))
}

/// Step 1 of the consent flow: the `consentAppId` for this login.
pub async fn generate_consent(b: &DhanBroker, creds: &BrokerCredentials) -> Result<String> {
    let (client_id, app_id, secret) = app_credentials(creds)?;
    let client_id = client_id.ok_or_else(|| {
        AppError::Validation(
            "Your Dhan client ID is missing. Enter the API key as client_id:::api_key in Profile, Broker Configuration."
                .into(),
        )
    })?;
    let url = format!(
        "{}/app/generate-consent?client_id={}",
        b.auth_base_url,
        urlencoding::encode(&client_id)
    );
    let resp = b
        .http
        .post(&url)
        .header("app_id", app_id)
        .header("app_secret", secret)
        .send()
        .await?;
    let (status, v): (StatusCode, Value) = http::read_json("dhan", resp).await?;
    if status == StatusCode::OK && s(&v, "status").as_deref() == Some("success") {
        if let Some(id) = s(&v, "consentAppId") {
            return Ok(id);
        }
    }
    tracing::warn!(
        status = status.as_u16(),
        "Dhan did not generate a login consent"
    );
    Err(AppError::Auth(
        "Dhan did not start the login. Check the API key (client_id:::api_key) and API secret in Profile, Broker Configuration."
            .into(),
    ))
}

/// Step 2: where the browser signs in.
pub fn consent_login_url(b: &DhanBroker, consent_app_id: &str) -> String {
    format!(
        "{}/login/consentApp-login?consentAppId={}",
        b.auth_base_url,
        urlencoding::encode(consent_app_id)
    )
}

/// Step 1 and 2 together: the URL to open for a Dhan consent login.
pub async fn login_url(b: &DhanBroker, creds: &BrokerCredentials) -> Result<String> {
    let id = generate_consent(b, creds).await?;
    Ok(consent_login_url(b, &id))
}

/// Step 3: exchange the callback `tokenId` for an access token.
pub async fn consume_consent(
    b: &DhanBroker,
    creds: &BrokerCredentials,
    token_id: &str,
) -> Result<ConsentSession> {
    let (_, app_id, secret) = app_credentials(creds)?;
    let url = format!(
        "{}/app/consumeApp-consent?tokenId={}",
        b.auth_base_url,
        urlencoding::encode(token_id)
    );
    let resp = b
        .http
        .post(&url)
        .header("app_id", app_id)
        .header("app_secret", secret)
        .header("Content-Type", "application/json")
        .send()
        .await?;
    let (status, v): (StatusCode, Value) = http::read_json("dhan", resp).await?;
    parse_consume(status, &v)
}

/// Partner flow, step 1 (web `dhan_sandbox` `generate_partner_consent`).
pub async fn generate_partner_consent(
    b: &DhanBroker,
    partner_id: &str,
    partner_secret: &str,
) -> Result<String> {
    let resp = b
        .http
        .post(format!("{}/partner/generate-consent", b.auth_base_url))
        .header("partner_id", partner_id)
        .header("partner_secret", partner_secret)
        .send()
        .await?;
    let (status, v): (StatusCode, Value) = http::read_json("dhan", resp).await?;
    match s(&v, "consentId") {
        Some(id) if status == StatusCode::OK => Ok(format!(
            "{}/consent-login?consentId={}",
            b.auth_base_url,
            urlencoding::encode(&id)
        )),
        _ => Err(AppError::Auth(
            "Dhan did not start the partner login. Check the partner ID and secret.".into(),
        )),
    }
}

/// Partner flow, step 3.
pub async fn consume_partner_consent(
    b: &DhanBroker,
    partner_id: &str,
    partner_secret: &str,
    token_id: &str,
) -> Result<ConsentSession> {
    let resp = b
        .http
        .post(format!(
            "{}/partner/consume-consent?tokenId={}",
            b.auth_base_url,
            urlencoding::encode(token_id)
        ))
        .header("partner_id", partner_id)
        .header("partner_secret", partner_secret)
        .send()
        .await?;
    let (status, v): (StatusCode, Value) = http::read_json("dhan", resp).await?;
    parse_consume(status, &v)
}

/// web `consume_consent`: HTTP 200 with `accessToken`.
pub fn parse_consume(status: StatusCode, v: &Value) -> Result<ConsentSession> {
    if status != StatusCode::OK {
        tracing::warn!(status = status.as_u16(), "Dhan refused the login consent");
        return Err(AppError::Auth(
            "Dhan did not complete the login. The login link may have expired; start the Dhan login again."
                .into(),
        ));
    }
    let access_token = s(v, "accessToken").ok_or_else(|| {
        AppError::Auth(
            "Dhan accepted the login but returned no session. Start the Dhan login again.".into(),
        )
    })?;
    Ok(ConsentSession {
        access_token,
        client_id: s(v, "dhanClientId"),
        client_name: s(v, "dhanClientName"),
        expiry_time: s(v, "expiryTime"),
    })
}

/// web `get_direct_access_token`: only the length is checked.
pub fn direct_access_token(token: &str) -> Result<String> {
    let t = token.trim();
    if t.len() < 50 {
        return Err(AppError::Validation(
            "That does not look like a Dhan access token. Copy the full token from web.dhan.co and paste it again."
                .into(),
        ));
    }
    Ok(t.to_string())
}

/// web `test_auth_token`: `GET /v2/fundlimit` must not be an auth error.
pub async fn validate_token(b: &DhanBroker, session: &DhanSession) -> Result<()> {
    let (status, v) = b
        .send(
            Method::GET,
            "/v2/fundlimit",
            session,
            None,
            None,
            super::Category::Trade,
        )
        .await?;
    if let Some(e) = super::dhan_error(&v) {
        tracing::warn!(
            status = status.as_u16(),
            "Dhan token check failed: {}",
            e.code()
        );
        return Err(match e {
            AppError::Auth(_) => AppError::Auth(
                "Dhan did not accept the access token. Generate a new token or log in to Dhan again."
                    .into(),
            ),
            other => other,
        });
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(AppError::Auth(
            "Dhan did not accept the access token. Generate a new token or log in to Dhan again."
                .into(),
        ));
    }
    Ok(())
}

pub async fn authenticate(b: &DhanBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let (key_client_id, _) = split_api_key(&creds.api_key);
    let cfg_client_id = creds
        .client_id
        .clone()
        .filter(|c| !c.trim().is_empty())
        .or(key_client_id);
    let code = creds
        .request_token
        .clone()
        .or(creds.auth_code.clone())
        .or(creds.password.clone())
        .filter(|c| !c.trim().is_empty());

    let (access_token, client_id, user_name) = match b.variant {
        Variant::Sandbox => {
            // web dhan_sandbox: the API secret is the access token; a pasted
            // JWT (> 100 chars with a dot) is used as is; otherwise a tokenId.
            match code.as_deref() {
                Some(c) if c != "dhan_sandbox" && c.len() > 100 && c.contains('.') => {
                    (direct_access_token(c)?, cfg_client_id, None)
                }
                Some(c) if c != "dhan_sandbox" => match consume_consent(b, &creds, c).await {
                    Ok(s) => (s.access_token, s.client_id.or(cfg_client_id), s.client_name),
                    Err(e) => match creds.api_secret.as_deref().filter(|t| !t.is_empty()) {
                        Some(t) => {
                            tracing::warn!("Dhan sandbox consent failed; using the saved token");
                            (direct_access_token(t)?, cfg_client_id, None)
                        }
                        None => return Err(e),
                    },
                },
                _ => {
                    let token = creds.api_secret.clone().unwrap_or_default();
                    (direct_access_token(&token)?, cfg_client_id, None)
                }
            }
        }
        Variant::Live => match code.as_deref() {
            Some(c) if c.len() > 100 => (direct_access_token(c)?, cfg_client_id, None),
            Some(c) => {
                let s = consume_consent(b, &creds, c).await?;
                (s.access_token, s.client_id.or(cfg_client_id), s.client_name)
            }
            None => {
                return Err(AppError::Validation(
                    "Dhan did not return a login code. Start the Dhan login again, or paste an access token."
                        .into(),
                ))
            }
        },
    };
    let session = DhanSession {
        client_id: client_id.clone(),
        access_token: access_token.clone(),
    };
    validate_token(b, &session).await?;
    let cid = client_id.unwrap_or_default();
    Ok(AuthResponse {
        auth_token: format!("{}:::{}", cid, access_token),
        feed_token: None,
        user_id: cid,
        user_name,
    })
}
