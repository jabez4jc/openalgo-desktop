//! Broker catalogue: every web broker id, how it signs in, and for the OAuth
//! brokers how the authorize URL is built and which callback parameter
//! carries the code. Kept apart from the adapters so the login flow can be
//! driven (and tested) without touching adapter code.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    /// Browser redirect to the broker, back to `/<broker>/callback`.
    OAuth,
    /// In-app form (client id, PIN or password, TOTP) posted to
    /// `/<broker>/callback`.
    Form,
}

/// Every broker the web supports, in its directory order.
pub const ALL_BROKERS: &[&str] = &[
    "aliceblue",
    "angel",
    "arrow",
    "compositedge",
    "definedge",
    "deltaexchange",
    "dhan",
    "dhan_sandbox",
    "firstock",
    "fivepaisa",
    "fivepaisaxts",
    "flattrade",
    "fyers",
    "groww",
    "hdfcsecurities",
    "hdfcsky",
    "ibulls",
    "iifl",
    "iiflcapital",
    "indmoney",
    "jainamxts",
    "kotak",
    "motilal",
    "mstock",
    "nubra",
    "paytm",
    "pocketful",
    "rmoney",
    "samco",
    "shoonya",
    "tradejini",
    "tradesmart",
    "upstox",
    "wisdom",
    "zebu",
    "zerodha",
];

pub fn auth_type(broker: &str) -> AuthType {
    match broker {
        "zerodha" | "fyers" | "upstox" | "dhan" | "arrow" | "paytm" | "pocketful" | "hdfcsky"
        | "hdfcsecurities" | "flattrade" | "compositedge" | "iiflcapital" => AuthType::OAuth,
        _ => AuthType::Form,
    }
}

/// Authorize URL with the server-generated `state`, or `None` when the
/// broker has no OAuth flow wired in this build.
pub fn authorize_url(
    broker: &str,
    api_key: &str,
    redirect_url: &str,
    state: &str,
) -> Option<String> {
    let enc = |s: &str| urlencoding::encode(s).into_owned();
    match broker {
        // Kite returns `redirect_params` verbatim on the callback query.
        "zerodha" => Some(format!(
            "https://kite.zerodha.com/connect/login?v=3&api_key={}&redirect_params={}",
            enc(api_key),
            enc(&format!("state={}", state))
        )),
        "fyers" => Some(format!(
            "https://api-t1.fyers.in/api/v3/generate-authcode?client_id={}&redirect_uri={}&response_type=code&state={}",
            enc(api_key),
            enc(redirect_url),
            enc(state)
        )),
        "upstox" => {
            // The code exchange must repeat this redirect byte for byte.
            crate::brokers::upstox::remember_redirect_uri(redirect_url);
            Some(format!(
            "https://api.upstox.com/v2/login/authorization/dialog?response_type=code&client_id={}&redirect_uri={}&state={}",
            enc(api_key),
            enc(redirect_url),
            enc(state)
        ))
        }
        _ => None,
    }
}

/// One field of a broker's in-app login form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct LoginField {
    /// Form field name posted to `/<broker>/callback`.
    pub name: &'static str,
    /// Label shown to the trader.
    pub label: &'static str,
    /// Masked input; never echoed back.
    pub secret: bool,
    pub required: bool,
}

/// Extra fields a broker's login form shows (beyond the stored API key and
/// secret). Empty for brokers that sign in by redirect or need nothing.
pub fn login_fields(broker: &str) -> &'static [LoginField] {
    match broker {
        // Groww: TOTP for a TOTP API key, or a pasted access token; with
        // neither, the stored API key and secret sign in (approval flow).
        "groww" => &[
            LoginField {
                name: "totp",
                label: "TOTP (if your Groww API key uses TOTP)",
                secret: true,
                required: false,
            },
            LoginField {
                name: "password",
                label: "Access token (if you paste one from Groww)",
                secret: true,
                required: false,
            },
        ],
        _ => &[],
    }
}

/// The authorization code on a callback query, per broker.
///
/// Zerodha sends `request_token` (the desktop used to read `code`, which Kite
/// never sends, so Zerodha login could not succeed). Fyers sends `auth_code`
/// and also an unrelated `code=200`, so it must not fall back to `code`.
pub fn extract_code(broker: &str, params: &HashMap<String, String>) -> Option<String> {
    let get = |k: &str| params.get(k).filter(|v| !v.is_empty()).cloned();
    match broker {
        "zerodha" => get("request_token"),
        "fyers" => get("auth_code"),
        // Dhan consent redirect (web brlogin accepts all three spellings).
        "dhan" | "dhan_sandbox" => get("tokenId")
            .or_else(|| get("token_id"))
            .or_else(|| get("token")),
        "arrow" | "hdfcsecurities" | "hdfcsky" => get("request_token")
            .or_else(|| get("requestToken"))
            .or_else(|| get("request-token"))
            .or_else(|| get("code")),
        _ => get("code").or_else(|| get("request_token")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn zerodha_uses_request_token() {
        let p = q(&[
            ("status", "success"),
            ("request_token", "rt123"),
            ("action", "login"),
            ("code", "ignored"),
        ]);
        assert_eq!(extract_code("zerodha", &p).as_deref(), Some("rt123"));
        assert_eq!(extract_code("zerodha", &q(&[("code", "x")])), None);
    }

    #[test]
    fn fyers_uses_auth_code_not_status_code() {
        let p = q(&[("s", "ok"), ("code", "200"), ("auth_code", "ac1")]);
        assert_eq!(extract_code("fyers", &p).as_deref(), Some("ac1"));
    }

    #[test]
    fn urls_carry_state() {
        let z = authorize_url(
            "zerodha",
            "kkey",
            "http://127.0.0.1:5000/zerodha/callback",
            "st1",
        )
        .unwrap();
        assert!(z.contains("api_key=kkey"));
        assert!(z.contains("redirect_params=state%3Dst1"));
        let f = authorize_url(
            "fyers",
            "APP-100",
            "http://127.0.0.1:5000/fyers/callback",
            "st2",
        )
        .unwrap();
        assert!(f.contains("state=st2"));
        assert!(f.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Ffyers%2Fcallback"));
        assert!(authorize_url("angel", "k", "r", "s").is_none());
    }

    #[test]
    fn groww_login_fields_are_optional_secrets() {
        let f = login_fields("groww");
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].name, f[1].name), ("totp", "password"));
        assert!(f.iter().all(|x| x.secret && !x.required));
        assert_eq!(auth_type("groww"), AuthType::Form);
        assert!(login_fields("zerodha").is_empty());
    }

    #[test]
    fn dhan_uses_token_id() {
        let p = q(&[("tokenId", "tid-1"), ("code", "x")]);
        assert_eq!(extract_code("dhan", &p).as_deref(), Some("tid-1"));
        assert_eq!(
            extract_code("dhan", &q(&[("token_id", "t2")])).as_deref(),
            Some("t2")
        );
        assert_eq!(extract_code("dhan", &q(&[("code", "x")])), None);
        assert_eq!(auth_type("kotak"), AuthType::Form);
        assert_eq!(auth_type("dhan_sandbox"), AuthType::Form);
    }

    #[test]
    fn catalogue_has_all_web_brokers() {
        assert_eq!(ALL_BROKERS.len(), 36);
        assert_eq!(auth_type("angel"), AuthType::Form);
        assert_eq!(auth_type("zerodha"), AuthType::OAuth);
    }
}
