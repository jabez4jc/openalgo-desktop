//! The session route table. Every browser-session route is declared here
//! with its access level, and the router is built from this table, so a
//! route cannot be added without saying who may call it. The guard test
//! walks the same table.

pub mod account;
pub mod auth;
pub mod broker;

use crate::server::middleware::{require_user, require_user_for_json};
use crate::state::AppState;
use axum::{
    http::Method,
    middleware,
    routing::{get, post, MethodRouter},
    Router,
};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// No session needed (setup, sign-in, CSRF token, session status).
    Public,
    /// Signed-in OpenAlgo user required.
    User,
    /// A browser navigation gets the SPA; a JSON request needs the user.
    UserJson,
    /// Broker redirect target: public, verified by the server-issued state.
    BrokerCallback,
}

pub struct RouteSpec {
    pub path: &'static str,
    pub method: Method,
    pub access: Access,
    pub make: fn() -> MethodRouter<Arc<AppState>>,
}

macro_rules! r {
    ($m:ident, $path:expr, $acc:ident, $f:path) => {
        RouteSpec {
            path: $path,
            method: Method::$m,
            access: Access::$acc,
            make: || r!(@route $m, $f),
        }
    };
    (@route GET, $f:path) => { get($f) };
    (@route POST, $f:path) => { post($f) };
}

pub fn table() -> Vec<RouteSpec> {
    vec![
        // Public
        r!(GET, "/auth/csrf-token", Public, auth::csrf_token),
        r!(GET, "/auth/check-setup", Public, auth::check_setup),
        r!(GET, "/auth/app-info", Public, auth::app_info),
        r!(GET, "/auth/session-status", Public, auth::session_status),
        r!(GET, "/auth/broker-config", Public, broker::broker_config),
        r!(POST, "/setup", Public, auth::setup),
        r!(GET, "/auth/login", Public, auth::login_page),
        r!(POST, "/auth/login", Public, auth::login),
        r!(POST, "/auth/login/totp", Public, auth::login_totp),
        r!(GET, "/auth/logout", Public, auth::logout),
        r!(POST, "/auth/logout", Public, auth::logout),
        r!(POST, "/auth/reset-password", Public, auth::reset_password),
        r!(POST, "/auth/reset-account", Public, auth::reset_account),
        // Broker redirect target (state-verified)
        r!(
            GET,
            "/{broker}/callback",
            BrokerCallback,
            broker::oauth_callback
        ),
        // Signed-in user
        r!(POST, "/{broker}/callback", User, broker::form_login),
        r!(
            GET,
            "/{broker}/initiate-oauth",
            User,
            broker::initiate_oauth
        ),
        r!(
            POST,
            "/auth/broker/oauth/manual",
            User,
            broker::oauth_manual
        ),
        r!(GET, "/auth/analyzer-mode", User, auth::analyzer_mode),
        r!(POST, "/auth/analyzer-toggle", User, auth::analyzer_toggle),
        r!(GET, "/auth/dashboard-data", User, auth::dashboard_data),
        r!(GET, "/auth/profile-data", User, auth::profile_data),
        r!(
            POST,
            "/auth/change-password",
            User,
            auth::change_password_api
        ),
        r!(POST, "/auth/change", User, auth::change_password_legacy),
        r!(GET, "/auth/2fa/status", User, auth::two_factor_status),
        r!(
            POST,
            "/auth/2fa/configure",
            User,
            auth::two_factor_configure
        ),
        r!(GET, "/auth/active-sessions", User, auth::active_sessions),
        r!(GET, "/apikey", UserJson, account::get_apikey),
        r!(POST, "/apikey", User, account::regenerate),
        r!(POST, "/apikey/mode", User, account::set_mode),
        r!(
            GET,
            "/api/broker/credentials",
            User,
            broker::get_credentials
        ),
        r!(
            POST,
            "/api/broker/credentials",
            User,
            broker::update_credentials
        ),
        r!(GET, "/api/broker/capabilities", User, broker::capabilities),
        r!(
            GET,
            "/api/desktop/settings",
            User,
            account::get_server_settings
        ),
        r!(
            POST,
            "/api/desktop/settings",
            User,
            account::update_server_settings
        ),
    ]
}

/// Build the session router from the table, applying the guard per access.
pub fn router() -> Router<Arc<AppState>> {
    let mut by_path: Vec<(&'static str, MethodRouter<Arc<AppState>>)> = Vec::new();
    for spec in table() {
        let mr = (spec.make)();
        let mr = match spec.access {
            Access::Public | Access::BrokerCallback => mr,
            Access::User => mr.route_layer(middleware::from_fn(require_user)),
            Access::UserJson => mr.route_layer(middleware::from_fn(require_user_for_json)),
        };
        match by_path.iter_mut().find(|(p, _)| *p == spec.path) {
            Some((_, existing)) => {
                let prev = std::mem::take(existing);
                *existing = prev.merge(mr);
            }
            None => by_path.push((spec.path, mr)),
        }
    }
    by_path
        .into_iter()
        .fold(Router::new(), |r, (p, mr)| r.route(p, mr))
}
