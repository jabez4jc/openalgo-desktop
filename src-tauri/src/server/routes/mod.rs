//! The session route table. Every browser-session route is declared here
//! with its access level, and the router is built from this table, so a
//! route cannot be added without saying who may call it. The guard test
//! walks the same table.

pub mod account;
pub mod admin;
pub mod app_config;
pub mod auth;
pub mod broker;
pub mod health;
pub mod latency;
pub mod leverage;
pub mod log;
pub mod market_calendar;
pub mod playground;
pub mod security;
pub mod settings;
pub mod traffic;
pub mod websocket_example;
pub mod webui;

use crate::server::middleware::{require_user, require_user_for_json};
use crate::state::AppState;
use axum::{
    http::Method,
    middleware,
    routing::{delete, get, post, put, MethodRouter},
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
    (@route PUT, $f:path) => { put($f) };
    (@route DELETE, $f:path) => { delete($f) };
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
        // Desktop server settings (the older path is an alias).
        r!(GET, "/settings/api/server", User, settings::get_server),
        r!(POST, "/settings/api/server", User, settings::save_server),
        r!(GET, "/api/desktop/settings", User, settings::get_server),
        r!(POST, "/api/desktop/settings", User, settings::save_server),
        r!(GET, "/settings/analyze-mode", User, settings::analyze_mode),
        r!(
            GET,
            "/api/websocket/config",
            User,
            websocket_example::config
        ),
        r!(
            GET,
            "/api/websocket/apikey",
            User,
            websocket_example::apikey
        ),
        r!(GET, "/api/config/host", User, app_config::host),
        // Admin (web blueprints/admin.py)
        r!(GET, "/admin/api/stats", User, admin::stats),
        r!(GET, "/admin/api/freeze", User, admin::freeze_list),
        r!(POST, "/admin/api/freeze", User, admin::freeze_add),
        r!(PUT, "/admin/api/freeze/{id}", User, admin::freeze_edit),
        r!(DELETE, "/admin/api/freeze/{id}", User, admin::freeze_delete),
        r!(POST, "/admin/api/freeze/upload", User, admin::freeze_upload),
        r!(GET, "/admin/api/holidays", User, admin::holidays),
        r!(POST, "/admin/api/holidays", User, admin::holiday_add),
        r!(
            DELETE,
            "/admin/api/holidays/{id}",
            User,
            admin::holiday_delete
        ),
        r!(GET, "/admin/api/timings", User, admin::timings),
        r!(
            PUT,
            "/admin/api/timings/{exchange}",
            User,
            admin::timing_edit
        ),
        r!(POST, "/admin/api/timings/check", User, admin::timing_check),
        r!(GET, "/admin/api/errors", User, admin::errors),
        r!(POST, "/admin/api/errors/client", User, admin::errors_client),
        r!(GET, "/admin/api/errors/stats", User, admin::errors_stats),
        r!(GET, "/admin/api/errors/groups", User, admin::errors_groups),
        r!(GET, "/admin/api/system", User, admin::system),
        r!(
            POST,
            "/admin/api/system/diagnostics",
            User,
            admin::diagnostics
        ),
        r!(GET, "/admin/api/system/report", User, admin::report),
        // Logs and monitoring
        r!(GET, "/logs", UserJson, log::view),
        r!(GET, "/logs/export", User, log::export),
        r!(GET, "/traffic/api/logs", User, traffic::logs),
        r!(GET, "/traffic/api/stats", User, traffic::stats),
        r!(GET, "/traffic/export", User, traffic::export),
        r!(GET, "/latency/api/logs", User, latency::logs),
        r!(GET, "/latency/api/stats", User, latency::stats),
        r!(
            GET,
            "/latency/api/broker/{broker}/stats",
            User,
            latency::broker_stats
        ),
        r!(GET, "/latency/export", User, latency::export),
        r!(POST, "/security/ban", User, security::ban),
        r!(POST, "/security/unban", User, security::unban),
        r!(POST, "/security/ban-host", User, security::ban_host),
        r!(POST, "/security/clear-404", User, security::clear_404),
        r!(GET, "/security/api/data", User, security::data),
        r!(GET, "/security/stats", User, security::stats),
        r!(POST, "/security/settings", User, security::settings),
        r!(
            GET,
            "/security/api/login-activity",
            User,
            security::login_activity
        ),
        r!(
            POST,
            "/security/api/login-activity/clear",
            User,
            security::clear_login_activity
        ),
        r!(
            GET,
            "/security/api/active-sessions",
            User,
            security::active_sessions
        ),
        r!(GET, "/health", UserJson, health::status),
        r!(GET, "/health/status", User, health::status),
        r!(GET, "/health/check", User, health::check),
        r!(GET, "/health/api/current", User, health::current),
        r!(GET, "/health/api/history", User, health::history),
        r!(GET, "/health/api/stats", User, health::stats),
        r!(GET, "/health/api/alerts", User, health::alerts),
        r!(
            POST,
            "/health/api/alerts/{alert_id}/acknowledge",
            User,
            health::acknowledge
        ),
        r!(
            POST,
            "/health/api/alerts/{alert_id}/resolve",
            User,
            health::resolve
        ),
        r!(GET, "/health/export", User, health::export),
        r!(GET, "/playground/api-key", User, playground::api_key),
        r!(GET, "/playground/endpoints", User, playground::endpoints),
        r!(GET, "/leverage/api/current", User, leverage::current),
        r!(POST, "/leverage/api/update", User, leverage::update),
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
