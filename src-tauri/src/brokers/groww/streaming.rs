//! Groww live data: NATS over WebSocket with protobuf payloads (web
//! `streaming/nats_websocket.py`, `groww_nats.py`, `groww_adapter.py`).
//!
//! The shared manager talks to a loopback relay (`upstox::relay`); the
//! relay's `GrowwUpstream` does the Groww-specific connect work on every
//! (re)connect:
//! 1. a fresh Ed25519 nkey pair; `POST /v1/api/apex/v1/socket/token/create/`
//!    with `{"socketKey": "<U...>"}` -> `{token, subscriptionId}` (on any
//!    failure the web falls back to the auth token and `direct_auth`);
//! 2. `wss://socket-api.groww.in` with the web's headers;
//! 3. NATS: server `INFO` (nonce) -> `CONNECT {jwt, nkey, sig}` + `PING`;
//!    `+OK` / the first `PONG` (or 2 s, like the web) means ready; server
//!    `PING` gets `PONG`; the client pings every 10 s; `-ERR` naming
//!    authorization ends the session as an auth failure.
//!
//! Each complete `MSG` op reaches `GrowwFeed::parse` as one binary frame.
//! Subjects: `/ld/{eq|fo}/{nse|bse}/price.{token}` (LTP/quote) and
//! `.../book.{token}` (depth). NSE indices use the OpenAlgo symbol as the
//! token, BSE indices the numeric token; depth on an index falls back to
//! price. Depth subscriptions add a shadow price subscription because the
//! book carries no LTP/OHLC, and a per-instrument merge cache joins them.

use super::nkeys::KeyPair;
use super::proto;
use crate::brokers::common::streaming::{
    now_ms, BrokerFeed, FeedEvent, FeedMode, FeedSubscription, Message, NormalizedDepth,
    NormalizedTick, WsRequest,
};
use crate::brokers::types::DepthLevel;
use crate::brokers::upstox::relay::{self, Open, RelayHandle, Session, Step, Upstream};
use crate::error::{AppError, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub const SOCKET_TOKEN_URL: &str = "https://api.groww.in/v1/api/apex/v1/socket/token/create/";
pub const WS_URL: &str = "wss://socket-api.groww.in";
/// Client NATS keepalive (web: PING every 10 s).
pub const NATS_PING_EVERY: Duration = Duration::from_secs(10);
/// Treat the session as authenticated after this long without `+OK`.
pub const ASSUME_READY_AFTER: Duration = Duration::from_secs(2);
/// Socket-token request budget (web: 15 s).
const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest buffered partial NATS op; anything bigger is a broken stream.
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;

/// Where the feed connects (overridable for tests).
#[derive(Debug, Clone)]
pub struct FeedEndpoints {
    pub socket_token_url: String,
    pub ws_url: String,
}

impl Default for FeedEndpoints {
    fn default() -> Self {
        Self {
            socket_token_url: SOCKET_TOKEN_URL.into(),
            ws_url: WS_URL.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// NATS protocol
// ---------------------------------------------------------------------------

/// One NATS server operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Info(Value),
    Msg {
        subject: String,
        sid: u64,
        payload: Vec<u8>,
    },
    Ping,
    Pong,
    Ok,
    Err(String),
    Other(String),
}

fn find_crlf(b: &[u8]) -> Option<usize> {
    b.windows(2).position(|w| w == b"\r\n")
}

/// Parse the first complete op in `buf`: `Some((op, bytes consumed))`, or
/// `None` when more bytes are needed.
pub fn next_op(buf: &[u8]) -> Option<(Op, usize)> {
    let end = find_crlf(buf)?;
    let line = String::from_utf8_lossy(&buf[..end]).to_string();
    let after = end + 2;
    let mut words = line.split_whitespace();
    let verb = words.next().unwrap_or("").to_ascii_uppercase();
    match verb.as_str() {
        "MSG" | "HMSG" => {
            let args: Vec<&str> = words.collect();
            let headers = verb == "HMSG";
            // MSG subj sid [reply] len ; HMSG subj sid [reply] hlen len
            let (subject, sid, hlen, len) = match (headers, args.len()) {
                (false, 3) => (args[0], args[1], 0, args[2]),
                (false, 4) => (args[0], args[1], 0, args[3]),
                (true, 4) => (args[0], args[1], args[2].parse().ok()?, args[3]),
                (true, 5) => (args[0], args[1], args[3].parse().ok()?, args[4]),
                _ => return Some((Op::Other(line), after)),
            };
            let len: usize = len.parse().ok()?;
            if buf.len() < after + len + 2 {
                return None;
            }
            let body = &buf[after..after + len];
            let payload = body.get(hlen.min(len)..).unwrap_or(&[]).to_vec();
            Some((
                Op::Msg {
                    subject: subject.to_string(),
                    sid: sid.parse().unwrap_or(0),
                    payload,
                },
                after + len + 2,
            ))
        }
        "PING" => Some((Op::Ping, after)),
        "PONG" => Some((Op::Pong, after)),
        "+OK" => Some((Op::Ok, after)),
        "-ERR" => Some((
            Op::Err(line[4..].trim().trim_matches('\'').to_string()),
            after,
        )),
        "INFO" => {
            let json = line.find('{').map(|i| &line[i..]).unwrap_or("{}");
            Some((
                Op::Info(serde_json::from_str(json).unwrap_or(Value::Null)),
                after,
            ))
        }
        _ => Some((Op::Other(line), after)),
    }
}

/// Web `create_connect`.
pub fn connect_frame(jwt: &str, nkey: Option<&str>, sig: Option<&str>) -> String {
    let mut o = json!({
        "verbose": false,
        "pedantic": false,
        "tls_required": true,
        "jwt": jwt,
        "protocol": 1,
        "version": "2.10.18",
        "lang": "python3",
        "name": "nats.py",
        "headers": true,
        "no_responders": true,
    });
    if let (Some(k), Some(map)) = (nkey, o.as_object_mut()) {
        map.insert("nkey".into(), json!(k));
    }
    if let (Some(s), Some(map)) = (sig, o.as_object_mut()) {
        map.insert("sig".into(), json!(s));
    }
    format!("CONNECT {}\r\n", o)
}

// ---------------------------------------------------------------------------
// Relay side: socket token, connect, NATS session
// ---------------------------------------------------------------------------

/// Credentials minted by one `open` for the session that follows.
struct Minted {
    jwt: String,
    key: Option<KeyPair>,
}

pub struct GrowwUpstream {
    http: reqwest::Client,
    auth_token: crate::security::Secret,
    endpoints: FeedEndpoints,
    minted: parking_lot::Mutex<Option<Minted>>,
}

impl GrowwUpstream {
    pub fn new(http: reqwest::Client, auth_token: &str, endpoints: FeedEndpoints) -> Self {
        Self {
            http,
            auth_token: crate::security::Secret::new(auth_token),
            endpoints,
            minted: parking_lot::Mutex::new(None),
        }
    }

    /// Socket token for a fresh key pair; falls back to the auth token
    /// (`direct_auth`, no signature) like the web.
    async fn socket_token(&self) -> (String, String, Option<KeyPair>) {
        let key = KeyPair::generate();
        let auth = self.auth_token.expose();
        let resp = self
            .http
            .post(&self.endpoints.socket_token_url)
            .timeout(TOKEN_TIMEOUT)
            .header("x-request-id", uuid::Uuid::new_v4().to_string())
            .header("Authorization", format!("Bearer {}", auth))
            .header("Content-Type", "application/json")
            .header("x-client-id", "growwapi")
            .header("x-client-platform", "growwapi-python-client")
            .header("x-client-platform-version", "0.0.8")
            .header("x-api-version", "1.0")
            .json(&json!({"socketKey": key.public_key()}))
            .send()
            .await;
        if let Ok(r) = resp {
            let status = r.status();
            if status.is_success() {
                if let Ok(v) = r.json::<Value>().await {
                    let token = v.get("token").and_then(Value::as_str).unwrap_or("");
                    let sub = v
                        .get("subscriptionId")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !token.is_empty() {
                        return (token.to_string(), sub.to_string(), Some(key));
                    }
                }
            }
            tracing::warn!(
                status = status.as_u16(),
                "Groww socket token not issued; using the session token directly"
            );
        } else {
            tracing::warn!("Groww socket token request failed; using the session token directly");
        }
        (auth.to_string(), "direct_auth".to_string(), None)
    }

    fn request(&self, jwt: &str, subscription: &str, with_protocol: bool) -> Option<WsRequest> {
        let mut req = self.endpoints.ws_url.as_str().into_client_request().ok()?;
        let h = req.headers_mut();
        h.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", jwt)).ok()?,
        );
        h.insert(
            "X-Subscription-Id",
            HeaderValue::from_str(subscription).ok()?,
        );
        h.insert(
            "User-Agent",
            HeaderValue::from_static("Python/3.10 nats.py/2.10.18"),
        );
        h.insert("X-Client-Id", HeaderValue::from_static("nats-py"));
        h.insert("X-API-Version", HeaderValue::from_static("1.0"));
        if with_protocol {
            h.insert("Sec-WebSocket-Protocol", HeaderValue::from_static("nats"));
        }
        Some(req)
    }
}

#[async_trait]
impl Upstream for GrowwUpstream {
    fn broker(&self) -> &'static str {
        "groww"
    }

    async fn open(&self) -> Open {
        let (jwt, subscription, key) = self.socket_token().await;
        for with_protocol in [true, false] {
            let Some(req) = self.request(&jwt, &subscription, with_protocol) else {
                tracing::error!("Groww feed request could not be built");
                return Open::Unavailable;
            };
            match tokio_tungstenite::connect_async(req).await {
                Ok((ws, _)) => {
                    *self.minted.lock() = Some(Minted { jwt, key });
                    return Open::Ready(Box::new(ws));
                }
                Err(tokio_tungstenite::tungstenite::Error::Http(resp))
                    if matches!(resp.status().as_u16(), 401 | 403) =>
                {
                    tracing::warn!(
                        status = resp.status().as_u16(),
                        "Groww feed refused the session"
                    );
                    return Open::AuthFailed(
                        "Groww refused the live market data session. Log in to Groww again.".into(),
                    );
                }
                // The server did not echo the `nats` subprotocol: retry
                // without asking for it.
                Err(tokio_tungstenite::tungstenite::Error::Protocol(p))
                    if with_protocol
                        && p.to_string().to_ascii_lowercase().contains("subprotocol") =>
                {
                    continue
                }
                Err(e) => {
                    tracing::debug!("Groww feed connect failed: {}", e);
                    return Open::Unavailable;
                }
            }
        }
        Open::Unavailable
    }

    fn session(&self) -> Box<dyn Session> {
        let m = self.minted.lock().take();
        let (jwt, key) = match m {
            Some(m) => (m.jwt, m.key),
            None => (self.auth_token.expose().to_string(), None),
        };
        Box::new(NatsSession::new(jwt, key))
    }
}

/// Per-connection NATS state (relay side).
pub struct NatsSession {
    jwt: String,
    key: Option<KeyPair>,
    buf: Vec<u8>,
    connect_sent: bool,
    /// Feed frames that arrived before `CONNECT` went out.
    held: Vec<Message>,
}

impl NatsSession {
    pub fn new(jwt: String, key: Option<KeyPair>) -> Self {
        Self {
            jwt,
            key,
            buf: Vec::new(),
            connect_sent: false,
            held: Vec::new(),
        }
    }
}

fn is_auth_error(m: &str) -> bool {
    let m = m.to_ascii_lowercase();
    m.contains("authoriz") || m.contains("authenticat")
}

impl Session for NatsSession {
    fn ready_on_open(&self) -> bool {
        false
    }

    fn assume_ready_after(&self) -> Option<Duration> {
        Some(ASSUME_READY_AFTER)
    }

    fn keepalive(&self) -> Option<(Duration, Message)> {
        Some((NATS_PING_EVERY, Message::Text("PING\r\n".into())))
    }

    fn on_downstream(&mut self, msg: Message) -> Vec<Message> {
        match msg {
            m @ (Message::Text(_) | Message::Binary(_)) => {
                if self.connect_sent {
                    vec![m]
                } else {
                    self.held.push(m);
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn on_upstream(&mut self, msg: Message) -> Step {
        let mut step = Step::default();
        match msg {
            Message::Text(t) => self.buf.extend_from_slice(t.as_bytes()),
            Message::Binary(b) => self.buf.extend_from_slice(&b),
            Message::Ping(_) | Message::Pong(_) => {
                step.down.push(Message::Ping(Vec::new()));
                return step;
            }
            _ => return step,
        }
        if self.buf.len() > MAX_PENDING_BYTES {
            tracing::warn!("Groww feed sent an oversized frame; dropping it");
            self.buf.clear();
            return step;
        }
        let mut used = 0;
        while let Some((op, n)) = next_op(&self.buf[used..]) {
            let raw = &self.buf[used..used + n];
            match op {
                Op::Info(info) => {
                    let nonce = info.get("nonce").and_then(Value::as_str).unwrap_or("");
                    let (nkey, sig) = match (&self.key, nonce.is_empty()) {
                        (Some(k), false) => (Some(k.public_key()), Some(k.sign_nonce(nonce))),
                        _ => (None, None),
                    };
                    step.up.push(Message::Text(connect_frame(
                        &self.jwt,
                        nkey.as_deref(),
                        sig.as_deref(),
                    )));
                    step.up.push(Message::Text("PING\r\n".into()));
                    self.connect_sent = true;
                    step.up.append(&mut self.held);
                }
                Op::Ping => step.up.push(Message::Text("PONG\r\n".into())),
                Op::Pong => {
                    if self.connect_sent {
                        step.ready = true;
                    }
                    step.down.push(Message::Ping(Vec::new()));
                }
                Op::Ok => step.ready = true,
                Op::Err(m) => {
                    if is_auth_error(&m) {
                        tracing::warn!("Groww feed refused the session: {}", m);
                        step.auth_failed = Some(
                            "Groww refused the live market data session. Log in to Groww again."
                                .into(),
                        );
                    } else {
                        tracing::warn!("Groww feed error: {}", m);
                    }
                }
                Op::Msg { .. } => step.down.push(Message::Binary(raw.to_vec())),
                Op::Other(line) => tracing::debug!("Groww feed op ignored: {}", line),
            }
            used += n;
        }
        self.buf.drain(..used);
        step
    }
}

// ---------------------------------------------------------------------------
// Feed side
// ---------------------------------------------------------------------------

/// Subscription subject kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Price,
    Book,
}

/// Merged state of one instrument (LTP topic + book topic).
#[derive(Debug, Clone, Default)]
struct Merged {
    ltp: f64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: i64,
    ltt: i64,
    buy: Vec<DepthLevel>,
    sell: Vec<DepthLevel>,
    seen: bool,
}

struct Inst {
    sub: FeedSubscription,
    sids: Vec<u64>,
    merged: Merged,
}

type Key = (String, String);

pub struct GrowwFeed {
    upstream: Arc<GrowwUpstream>,
    relay: parking_lot::Mutex<Option<RelayHandle>>,
    next_sid: u64,
    instruments: HashMap<Key, Inst>,
    sids: HashMap<u64, (Key, Kind)>,
}

fn is_index(exchange: &str) -> bool {
    exchange.ends_with("_INDEX")
}

/// Subjects for a subscription (web `format_topic_for_groww` as used by
/// `subscribe_batch`).
pub fn subjects(sub: &FeedSubscription) -> Vec<String> {
    subjects_with_kind(sub)
        .into_iter()
        .map(|(s, _)| s)
        .collect()
}

fn subjects_with_kind(sub: &FeedSubscription) -> Vec<(String, Kind)> {
    let ex = super::mapping::groww_exchange(&sub.exchange).to_ascii_lowercase();
    let seg = if matches!(sub.exchange.as_str(), "NFO" | "BFO") {
        "fo"
    } else {
        "eq"
    };
    let token = if sub.exchange == "NSE_INDEX" {
        sub.symbol.as_str()
    } else {
        sub.token.as_str()
    };
    let price = (format!("/ld/{}/{}/price.{}", seg, ex, token), Kind::Price);
    if sub.mode == FeedMode::Depth && !is_index(&sub.exchange) {
        vec![
            price,
            (format!("/ld/{}/{}/book.{}", seg, ex, token), Kind::Book),
        ]
    } else {
        vec![price]
    }
}

fn levels(side: &[proto::DepthLevel]) -> Vec<DepthLevel> {
    side.iter()
        .take(5)
        .filter_map(|l| {
            let pq = l.price_qty.as_ref()?;
            let lvl = DepthLevel {
                price: pq.price,
                quantity: pq.quantity as i64,
                orders: l.orders,
            };
            (lvl.price > 0.0 || lvl.quantity > 0).then_some(lvl)
        })
        .collect()
}

impl GrowwFeed {
    pub fn new(http: reqwest::Client, auth_token: &str, endpoints: FeedEndpoints) -> Self {
        Self {
            upstream: Arc::new(GrowwUpstream::new(http, auth_token, endpoints)),
            relay: parking_lot::Mutex::new(None),
            next_sid: 1,
            instruments: HashMap::new(),
            sids: HashMap::new(),
        }
    }

    fn forget(&mut self, key: &Key) -> Option<Inst> {
        let inst = self.instruments.remove(key)?;
        for sid in &inst.sids {
            self.sids.remove(sid);
        }
        Some(inst)
    }

    /// Instruments currently registered (bounded by the manager's registry).
    pub fn instrument_count(&self) -> usize {
        self.instruments.len()
    }

    fn on_msg(&mut self, sid: u64, payload: &[u8]) -> Vec<FeedEvent> {
        let Some((key, _kind)) = self.sids.get(&sid).cloned() else {
            return Vec::new();
        };
        let Some(inst) = self.instruments.get_mut(&key) else {
            return Vec::new();
        };
        let Some(data) = proto::decode(payload) else {
            tracing::debug!("Groww feed payload could not be decoded");
            return Vec::new();
        };
        let m = &mut inst.merged;
        let mut is_price = false;
        if let Some(p) = &data.ltp_data {
            is_price = true;
            m.ltp = p.ltp;
            for (dst, v) in [
                (&mut m.open, p.open),
                (&mut m.high, p.high),
                (&mut m.low, p.low),
                (&mut m.close, p.close),
            ] {
                if v != 0.0 {
                    *dst = v;
                }
            }
            if p.volume != 0.0 {
                m.volume = p.volume as i64;
            }
            if p.ts_in_millis > 0.0 {
                m.ltt = p.ts_in_millis as i64;
            }
            m.seen = true;
        }
        if let Some(i) = &data.index_data {
            is_price = true;
            m.ltp = i.value;
            if i.ts_in_millis > 0.0 {
                m.ltt = i.ts_in_millis as i64;
            }
            m.seen = true;
        }
        let mut is_book = false;
        if let Some(d) = &data.depth_data {
            is_book = true;
            m.buy = levels(&d.buy);
            m.sell = levels(&d.sell);
            if d.ts_in_millis > 0.0 {
                m.ltt = d.ts_in_millis as i64;
            }
            m.seen = true;
        }
        let mode = inst.sub.mode;
        let depth_mode = mode == FeedMode::Depth && !is_index(&inst.sub.exchange);
        if !depth_mode && !is_price {
            // LTP / quote subscriptions ignore book ticks.
            return Vec::new();
        }
        if depth_mode && !(is_price || is_book) {
            return Vec::new();
        }
        let now = now_ms();
        let effective = if is_index(&inst.sub.exchange) && mode == FeedMode::Depth {
            FeedMode::Ltp
        } else {
            mode
        };
        let mut tick = NormalizedTick {
            symbol: inst.sub.symbol.clone(),
            exchange: inst.sub.exchange.clone(),
            mode: mode.code(),
            ltp: m.ltp,
            last_trade_time_ms: m.ltt,
            timestamp_ms: now,
            ..Default::default()
        };
        if effective != FeedMode::Ltp {
            tick.open = m.open;
            tick.high = m.high;
            tick.low = m.low;
            tick.close = m.close;
            tick.volume = m.volume;
            tick.derive_change();
        }
        let mut out = Vec::with_capacity(2);
        if depth_mode {
            tick.total_buy_quantity = m.buy.iter().map(|l| l.quantity).sum();
            tick.total_sell_quantity = m.sell.iter().map(|l| l.quantity).sum();
            let depth = NormalizedDepth {
                symbol: tick.symbol.clone(),
                exchange: tick.exchange.clone(),
                ltp: m.ltp,
                buy: m.buy.clone(),
                sell: m.sell.clone(),
                total_buy_quantity: tick.total_buy_quantity,
                total_sell_quantity: tick.total_sell_quantity,
                timestamp_ms: now,
            };
            out.push(FeedEvent::Tick(tick));
            out.push(FeedEvent::Depth(depth));
        } else {
            out.push(FeedEvent::Tick(tick));
        }
        out
    }

    fn parse_binary(&mut self, data: &[u8]) -> Vec<FeedEvent> {
        let mut out = Vec::new();
        let mut used = 0;
        while let Some((op, n)) = next_op(&data[used..]) {
            if let Op::Msg { sid, payload, .. } = op {
                out.extend(self.on_msg(sid, &payload));
            }
            used += n;
        }
        out
    }
}

impl BrokerFeed for GrowwFeed {
    fn broker(&self) -> &'static str {
        "groww"
    }

    fn ws_request(&self) -> Result<WsRequest> {
        let up = self.upstream.clone();
        let url = relay::ensure_started(&self.relay, move || up as Arc<dyn Upstream>)?;
        url.as_str()
            .into_client_request()
            .map_err(|_| AppError::Internal("Groww feed relay address is invalid".into()))
    }

    fn on_connected(&mut self) -> Vec<Message> {
        // NATS sids are per connection; everything is re-subscribed after
        // the relay reports ready.
        self.instruments.clear();
        self.sids.clear();
        self.next_sid = 1;
        Vec::new()
    }

    fn awaits_auth_ack(&self) -> bool {
        true
    }

    fn subscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut text = String::new();
        for s in subs {
            let key = s.instrument_key();
            self.forget(&key);
            let mut sids = Vec::new();
            for (subject, kind) in subjects_with_kind(s) {
                let sid = self.next_sid;
                self.next_sid += 1;
                self.sids.insert(sid, (key.clone(), kind));
                sids.push(sid);
                text.push_str(&format!("SUB {} {}\r\n", subject, sid));
            }
            self.instruments.insert(
                key,
                Inst {
                    sub: s.clone(),
                    sids,
                    merged: Merged::default(),
                },
            );
        }
        if text.is_empty() {
            return Vec::new();
        }
        text.push_str("PING\r\n");
        vec![Message::Text(text)]
    }

    fn unsubscribe_frames(&mut self, subs: &[FeedSubscription]) -> Vec<Message> {
        let mut text = String::new();
        for s in subs {
            if let Some(inst) = self.forget(&s.instrument_key()) {
                for sid in inst.sids {
                    text.push_str(&format!("UNSUB {}\r\n", sid));
                }
            }
        }
        if text.is_empty() {
            Vec::new()
        } else {
            vec![Message::Text(text)]
        }
    }

    fn parse(&mut self, msg: &Message) -> Vec<FeedEvent> {
        match msg {
            Message::Text(t) => match relay::control(t) {
                Some(Ok(())) => vec![FeedEvent::AuthOk],
                Some(Err(m)) => vec![FeedEvent::AuthFailed(m)],
                None => Vec::new(),
            },
            Message::Binary(b) => self.parse_binary(b),
            Message::Ping(_) | Message::Pong(_) => vec![FeedEvent::Heartbeat],
            _ => Vec::new(),
        }
    }

    fn supported_depth_levels(&self) -> &'static [u8] {
        &[5]
    }
}
