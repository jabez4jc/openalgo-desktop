//! Kotak Neo two-step sign-in (web `api/auth_api.py`).
//!
//! Step 1, TOTP: `POST https://mis.kotaksecurities.com/login/1.0/tradeApiLogin`
//! with `Authorization: <access token>`, `neo-fin-key: neotradeapi` and
//! `{"mobileNumber": "+91..", "ucc": "<UCC>", "totp": ".."}` -> a view
//! `token` and `sid`.
//!
//! Step 2, MPIN: `POST .../tradeApiValidate` with the same two headers plus
//! `sid: <view sid>` and `Auth: <view token>`, body `{"mpin": ".."}` -> the
//! trading `token`, `sid`, the dynamic `baseUrl` and the `dataCenter`.
//!
//! Both steps succeed only when `data.status == "success"`. The steps are
//! public so a route can drive them as two forms; `authenticate` runs both
//! from one form, as the web's single POST does.

use super::{KotakBroker, KotakSession};
use crate::brokers::common::http;
use crate::brokers::{AuthResponse, BrokerCredentials};
use crate::error::{AppError, Result};
use reqwest::StatusCode;
use serde_json::{json, Value};

/// The view session from step 1. `Debug` is redacted.
#[derive(Clone)]
pub struct ViewSession {
    pub token: String,
    pub sid: String,
}

impl std::fmt::Debug for ViewSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ViewSession([REDACTED])")
    }
}

/// web mobile normalisation: strip `+91`, spaces and a leading `91` on a
/// 12-digit number, then prefix `+91`.
pub fn normalize_mobile(mobile: &str) -> String {
    let mut m = mobile.trim().replace("+91", "").replace(' ', "");
    if m.starts_with("91") && m.len() == 12 {
        m = m[2..].to_string();
    }
    format!("+91{}", m)
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

fn step_ok(v: &Value) -> bool {
    v.get("data")
        .and_then(|d| d.get("status"))
        .and_then(Value::as_str)
        == Some("success")
}

fn step_error(v: &Value, step: &str) -> AppError {
    let msg = s(v, "errMsg")
        .or_else(|| s(v, "message"))
        .unwrap_or_else(|| format!("{} failed", step));
    tracing::warn!("Kotak {} refused", step);
    AppError::Auth(match step {
        "TOTP login" => format!(
            "Kotak did not accept the mobile number or TOTP: {}. Make sure TOTP is registered in the Kotak NEO app (Settings, Security, Enable TOTP) and use a fresh code.",
            msg
        ),
        _ => format!("Kotak did not accept the MPIN: {}. Check the MPIN and try again.", msg),
    })
}

fn creds_ucc_token(creds: &BrokerCredentials) -> Result<(String, String)> {
    let ucc = creds.api_key.trim().to_string();
    if ucc.is_empty() {
        return Err(AppError::Validation(
            "Your Kotak UCC is missing. Enter it as the API key in Profile, Broker Configuration."
                .into(),
        ));
    }
    let token = creds
        .api_secret
        .clone()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "Your Kotak Neo access token is missing. Enter it as the API secret in Profile, Broker Configuration."
                    .into(),
            )
        })?;
    Ok((ucc, token))
}

/// Step 1: mobile + TOTP -> view session.
pub async fn totp_login(
    b: &KotakBroker,
    ucc: &str,
    access_token: &str,
    mobile: &str,
    totp: &str,
) -> Result<ViewSession> {
    let body = json!({
        "mobileNumber": normalize_mobile(mobile),
        "ucc": ucc,
        "totp": totp.trim(),
    });
    let resp = b
        .http
        .post(format!("{}/login/1.0/tradeApiLogin", b.login_base_url))
        .header("Authorization", access_token)
        .header("neo-fin-key", "neotradeapi")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await?;
    let (_, v): (StatusCode, Value) = http::read_json("kotak", resp).await?;
    if !step_ok(&v) {
        return Err(step_error(&v, "TOTP login"));
    }
    let d = &v["data"];
    match (s(d, "token"), s(d, "sid")) {
        (Some(token), Some(sid)) => Ok(ViewSession { token, sid }),
        _ => Err(AppError::Auth(
            "Kotak accepted the TOTP but returned no session. Try logging in again.".into(),
        )),
    }
}

/// Step 2: MPIN -> trading session (`token:::sid:::baseUrl:::accessToken:::dataCenter`).
pub async fn validate_mpin(
    b: &KotakBroker,
    access_token: &str,
    view: &ViewSession,
    mpin: &str,
) -> Result<KotakSession> {
    let resp = b
        .http
        .post(format!("{}/login/1.0/tradeApiValidate", b.login_base_url))
        .header("Authorization", access_token)
        .header("neo-fin-key", "neotradeapi")
        .header("sid", &view.sid)
        .header("Auth", &view.token)
        .header("Content-Type", "application/json")
        .body(json!({"mpin": mpin.trim()}).to_string())
        .send()
        .await?;
    let (_, v): (StatusCode, Value) = http::read_json("kotak", resp).await?;
    if !step_ok(&v) {
        return Err(step_error(&v, "MPIN validation"));
    }
    let d = &v["data"];
    let (Some(token), Some(sid)) = (s(d, "token"), s(d, "sid")) else {
        return Err(AppError::Auth(
            "Kotak accepted the MPIN but returned no session. Try logging in again.".into(),
        ));
    };
    let base_url = s(d, "baseUrl").unwrap_or_default();
    let data_center = s(d, "dataCenter").unwrap_or_default();
    if base_url.is_empty() {
        tracing::warn!("Kotak MPIN validation returned no baseUrl; trading calls will fail");
        return Err(AppError::Auth(
            "Kotak did not say which server your account uses. Log in again; if it keeps happening, contact Kotak support."
                .into(),
        ));
    }
    if data_center.is_empty() {
        tracing::warn!("Kotak returned no dataCenter; streaming uses the default feed");
    }
    Ok(KotakSession {
        token,
        sid,
        base_url: base_url.trim_end_matches('/').to_string(),
        access_token: access_token.to_string(),
        data_center,
    })
}

pub async fn authenticate(b: &KotakBroker, creds: BrokerCredentials) -> Result<AuthResponse> {
    let (ucc, access_token) = creds_ucc_token(&creds)?;
    let mobile = creds.client_id.clone().unwrap_or_default();
    let totp = creds.totp.clone().unwrap_or_default();
    let mpin = creds.password.clone().unwrap_or_default();
    if mobile.trim().is_empty() || totp.trim().is_empty() || mpin.trim().is_empty() {
        return Err(AppError::Validation(
            "Please provide Mobile Number, TOTP, and MPIN".into(),
        ));
    }
    let view = totp_login(b, &ucc, &access_token, &mobile, &totp).await?;
    let session = validate_mpin(b, &access_token, &view, &mpin).await?;
    *b.ucc.lock() = Some(ucc.clone());
    // Resolve this data centre's feed host now, while we are async.
    super::streaming::resolve_feed_url(b, &session.data_center).await;
    Ok(AuthResponse {
        auth_token: session.compose(),
        feed_token: None,
        user_id: ucc,
        user_name: None,
    })
}
