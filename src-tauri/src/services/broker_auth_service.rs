//! Broker sign-in, session persistence, resume and revocation.
//!
//! * OAuth brokers: `start_oauth` stores a server-generated `state` and
//!   returns the broker's authorize URL; the callback (`complete_oauth`)
//!   consumes the state, exchanges the code for a token in Rust, persists it
//!   encrypted and publishes `broker.connected`. The code never reaches the
//!   frontend.
//! * Form brokers (client id, PIN, TOTP): `login_with_form`.
//! * After a password sign-in, `try_resume` brings back the stored session
//!   if it was issued after the last 03:00 IST boundary and the broker still
//!   accepts it (validated with a funds call, like the web).
//! * Logout and the daily boundary call `revoke`.

use crate::brokers::{catalog, BrokerCredentials};
use crate::db::sqlite::{auth, credentials, oauth_state};
use crate::error::{AppError, Result};
use crate::events::{Event, SessionEndReason};
use crate::security::Secret;
use crate::session::web::random_token;
use crate::state::{AppState, BrokerSession};
use std::collections::HashMap;
use std::time::Duration;

pub const OAUTH_STATE_TTL_MINUTES: i64 = 10;
const RESUME_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

pub struct BrokerAuthService;

/// Inputs from a broker login form (web field names).
#[derive(Debug, Default, Clone)]
pub struct FormLogin {
    pub client_id: Option<String>,
    pub password: Option<Secret>,
    pub totp: Option<Secret>,
}

impl FormLogin {
    pub fn from_fields(f: &HashMap<String, String>) -> Self {
        let pick = |keys: &[&str]| {
            keys.iter().find_map(|k| {
                f.get(*k)
                    .filter(|v| !v.trim().is_empty())
                    .map(|v| v.trim().to_string())
            })
        };
        FormLogin {
            client_id: pick(&["userid", "clientid", "client_id", "mobile"]),
            password: pick(&["pin", "password", "mpin"]).map(Secret::new),
            totp: pick(&["totp", "twofa", "otp"]).map(Secret::new),
        }
    }
}

impl BrokerAuthService {
    /// Broker chosen in settings (web: from REDIRECT_URL).
    pub fn active_broker(state: &AppState) -> Option<String> {
        state.server_config().active_broker
    }

    fn load_credentials(
        state: &AppState,
        broker: &str,
    ) -> Result<credentials::BrokerCredentialSet> {
        let conn = state.sqlite.conn()?;
        credentials::load(&conn, &state.security, broker)?
            .filter(|c| !c.api_key.is_empty())
            .ok_or_else(|| {
                AppError::Validation(
                    "Add your broker API key and secret in Profile, Broker Configuration, then try again."
                        .into(),
                )
            })
    }

    /// Start an OAuth login: store a fresh `state`, return the authorize URL.
    pub fn start_oauth(state: &AppState, broker: &str) -> Result<String> {
        if state.brokers.get(broker).is_none() {
            return Err(AppError::Validation(format!(
                "Signing in to {} is not available in this version of OpenAlgo Desktop yet.",
                broker
            )));
        }
        let creds = Self::load_credentials(state, broker)?;
        let redirect = state.server_config().redirect_url_for(broker);
        let st = random_token();
        let url = catalog::authorize_url(broker, creds.api_key.expose(), &redirect, &st)
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "{} signs in with a form, not a browser redirect.",
                    broker
                ))
            })?;
        {
            let conn = state.sqlite.conn()?;
            oauth_state::insert(
                &conn,
                &st,
                broker,
                state.now(),
                chrono::Duration::minutes(OAUTH_STATE_TTL_MINUTES),
            )?;
        }
        Ok(url)
    }

    /// Finish an OAuth login from the callback query parameters.
    pub async fn complete_oauth(
        state: &AppState,
        broker: &str,
        params: &HashMap<String, String>,
    ) -> Result<BrokerSession> {
        let st = params.get("state").cloned().unwrap_or_default();
        let ok = !st.is_empty() && {
            let conn = state.sqlite.conn()?;
            oauth_state::consume(&conn, &st, broker, state.now())?
        };
        if !ok {
            return Err(AppError::Auth(
                "This broker sign-in was not started from OpenAlgo or has expired. Start the broker login again from OpenAlgo."
                    .into(),
            ));
        }
        let code = catalog::extract_code(broker, params).ok_or_else(|| {
            AppError::Auth("The broker did not complete the sign-in. Try again.".into())
        })?;
        let creds = Self::load_credentials(state, broker)?;
        let input = BrokerCredentials {
            api_key: creds.api_key.expose().to_string(),
            api_secret: creds.api_secret.as_ref().map(|s| s.expose().to_string()),
            client_id: creds.client_id.clone(),
            request_token: Some(code.clone()),
            auth_code: Some(code),
            ..Default::default()
        };
        Self::authenticate(state, broker, input).await
    }

    /// Form login (Angel and the other client-id/PIN/TOTP brokers).
    pub async fn login_with_form(
        state: &AppState,
        broker: &str,
        form: FormLogin,
    ) -> Result<BrokerSession> {
        if catalog::auth_type(broker) != catalog::AuthType::Form {
            return Err(AppError::Validation(
                "This broker signs in through the broker's own page.".into(),
            ));
        }
        let creds = Self::load_credentials(state, broker)?;
        let input = BrokerCredentials {
            api_key: creds.api_key.expose().to_string(),
            api_secret: creds.api_secret.as_ref().map(|s| s.expose().to_string()),
            client_id: form.client_id.or(creds.client_id.clone()),
            password: form.password.map(|s| s.expose().to_string()),
            totp: form.totp.map(|s| s.expose().to_string()),
            ..Default::default()
        };
        Self::authenticate(state, broker, input).await
    }

    async fn authenticate(
        state: &AppState,
        broker_id: &str,
        input: BrokerCredentials,
    ) -> Result<BrokerSession> {
        let broker = state.brokers.get(broker_id).ok_or_else(|| {
            AppError::Validation(format!(
                "Signing in to {} is not available in this version of OpenAlgo Desktop yet.",
                broker_id
            ))
        })?;
        // No database connection is held across this await.
        let resp = broker.authenticate(input).await?;
        let session = BrokerSession {
            broker_id: broker_id.to_string(),
            auth_token: Secret::new(resp.auth_token),
            feed_token: resp.feed_token.map(Secret::new),
            user_id: resp.user_id,
            user_name: resp.user_name,
            authenticated_at: state.now(),
        };
        Self::persist(state, &session)?;
        Ok(session)
    }

    /// Store and activate a session, then announce it.
    pub fn persist(state: &AppState, s: &BrokerSession) -> Result<()> {
        {
            let conn = state.sqlite.conn()?;
            auth::upsert(
                &conn,
                &state.security,
                &auth::StoredBrokerSession {
                    broker_id: s.broker_id.clone(),
                    auth_token: s.auth_token.clone(),
                    feed_token: s.feed_token.clone(),
                    user_id: Some(s.user_id.clone()),
                    user_name: s.user_name.clone(),
                    authenticated_at: s.authenticated_at,
                },
            )?;
            crate::config::save(
                &conn,
                &crate::config::ServerConfigUpdate {
                    active_broker: Some(s.broker_id.clone()),
                    ..Default::default()
                },
            )?;
        }
        let _ = state.reload_config();
        state.set_broker_session(Some(s.clone()));
        state.api_keys.clear();
        state.bus.publish(Event::BrokerConnected {
            broker: s.broker_id.clone(),
            user_id: Some(s.user_id.clone()),
        });
        tracing::info!("Broker session started for {}", s.broker_id);
        Ok(())
    }

    /// Resume the stored session after a password sign-in. `Ok(None)` when
    /// there is nothing to resume or the broker no longer accepts it.
    pub async fn try_resume(state: &AppState) -> Result<Option<BrokerSession>> {
        if let Some(s) = state.get_broker_session() {
            return Ok(Some(s));
        }
        let stored = {
            let conn = state.sqlite.conn()?;
            match auth::latest_active(&conn, &state.security) {
                Ok(s) => s,
                Err(AppError::Locked) => None,
                Err(e) => return Err(e),
            }
        };
        let Some(stored) = stored else {
            return Ok(None);
        };
        let cfg = state.server_config();
        if !crate::session::boundary::is_fresh(
            stored.authenticated_at,
            state.now(),
            cfg.session_expiry_hour,
            cfg.session_expiry_minute,
        ) {
            let conn = state.sqlite.conn()?;
            auth::revoke(&conn, &stored.broker_id)?;
            return Ok(None);
        }
        let Some(broker) = state.brokers.get(&stored.broker_id) else {
            return Ok(None);
        };
        // Like the web: a cheap funds call proves the token still works.
        match tokio::time::timeout(
            RESUME_CHECK_TIMEOUT,
            broker.get_funds(&crate::brokers::types::AuthToken::new(
                stored.auth_token.expose(),
            )),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::info!("Stored broker session not accepted by broker: {}", e.code());
                return Ok(None);
            }
            Err(_) => {
                tracing::info!("Broker did not answer the session check in time");
                return Ok(None);
            }
        }
        let session = BrokerSession {
            broker_id: stored.broker_id,
            auth_token: stored.auth_token,
            feed_token: stored.feed_token,
            user_id: stored.user_id.unwrap_or_default(),
            user_name: stored.user_name,
            authenticated_at: stored.authenticated_at,
        };
        state.set_broker_session(Some(session.clone()));
        tracing::info!("Resumed broker session for {}", session.broker_id);
        Ok(Some(session))
    }

    /// End the broker session everywhere: stored row revoked, memory cleared,
    /// feed closed, symbol cache dropped, subscribers told.
    pub async fn revoke(state: &AppState, reason: SessionEndReason) -> Result<()> {
        {
            let conn = state.sqlite.conn()?;
            auth::revoke_all(&conn)?;
        }
        state.set_broker_session(None);
        state.api_keys.clear();
        state.clear_symbol_cache();
        let _ = state.websocket.disconnect().await;
        state.bus.publish(Event::BrokerSessionEnded { reason });
        Ok(())
    }

    /// Whether the stored broker session ended (revoked/expired) rather than
    /// never having existed.
    pub fn had_revoked_session(state: &AppState) -> bool {
        state
            .sqlite
            .conn()
            .ok()
            .and_then(|c| auth::has_revoked(&c).ok())
            .unwrap_or(false)
    }
}
