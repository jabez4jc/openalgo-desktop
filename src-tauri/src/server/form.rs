//! Body extractor for the session routes. The web frontend posts some forms
//! as `multipart/form-data` (`FormData`), some as urlencoded and some as
//! JSON; all three become a flat string map.
//!
//! CSRF is checked before the handler runs, by the session middleware
//! (header `X-CSRFToken` or form field `csrf_token`).

use crate::server::envelope::{error, BAD_REQUEST_MSG};
use axum::{
    body::Bytes,
    extract::{FromRequest, Multipart, Request},
    http::{header, StatusCode},
    response::Response,
};
use serde_json::Value;
use std::collections::HashMap;
use subtle::ConstantTimeEq;

pub const MAX_FIELDS: usize = 64;

pub struct FormData(pub HashMap<String, String>);

impl FormData {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(|s| s.as_str())
    }

    pub fn non_empty(&self, k: &str) -> Option<String> {
        self.get(k)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    }
}

pub fn csrf_rejected() -> Response {
    error(
        StatusCode::BAD_REQUEST,
        "Your session has expired. Refresh the page and try again.",
    )
}

pub fn tokens_match(a: &str, b: &str) -> bool {
    !a.is_empty() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

impl<S: Send + Sync> FromRequest<S> for FormData {
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let ct = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let bad = || error(StatusCode::BAD_REQUEST, BAD_REQUEST_MSG);
        let map: HashMap<String, String> = if ct.starts_with("multipart/form-data") {
            let mut mp = Multipart::from_request(req, state)
                .await
                .map_err(|_| bad())?;
            let mut m = HashMap::new();
            while let Some(field) = mp.next_field().await.map_err(|_| bad())? {
                if m.len() >= MAX_FIELDS {
                    return Err(bad());
                }
                let name = field.name().unwrap_or_default().to_string();
                if field.file_name().is_some() {
                    continue;
                }
                let text = field.text().await.map_err(|_| bad())?;
                m.insert(name, text);
            }
            m
        } else {
            let bytes = Bytes::from_request(req, state).await.map_err(|_| bad())?;
            if bytes.is_empty() {
                HashMap::new()
            } else if ct.starts_with("application/json") {
                let v: Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
                match v {
                    Value::Object(o) => o
                        .into_iter()
                        .take(MAX_FIELDS)
                        .filter_map(|(k, v)| scalar(&v).map(|s| (k, s)))
                        .collect(),
                    _ => return Err(bad()),
                }
            } else {
                serde_urlencoded::from_bytes::<Vec<(String, String)>>(&bytes)
                    .map_err(|_| bad())?
                    .into_iter()
                    .take(MAX_FIELDS)
                    .collect()
            }
        };
        Ok(FormData(map))
    }
}
