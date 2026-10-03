//! Minimal Engine.IO v4 / Socket.IO v5 client codec, enough for the XTS
//! market-data socket (the web uses python-socketio 5 / python-engineio 4,
//! i.e. `EIO=4`, websocket transport only).
//!
//! Engine.IO text frame: one type digit then data
//! (`0` open, `1` close, `2` ping, `3` pong, `4` message, `5` upgrade,
//! `6` noop). Binary frames are message payloads (EIO4) and carry the
//! attachments of a Socket.IO binary event. Socket.IO packet inside an
//! Engine.IO message: `<type>[<attachments>-][<nsp>,][<ack id>][<json>]`
//! (`0` connect, `1` disconnect, `2` event, `3` ack, `4` connect error,
//! `5` binary event, `6` binary ack).

use serde_json::Value;

/// One Engine.IO packet.
#[derive(Debug, Clone, PartialEq)]
pub enum EioPacket<'a> {
    Open(Value),
    Close,
    Ping(&'a str),
    Pong(&'a str),
    Message(&'a str),
    Upgrade,
    Noop,
}

pub fn decode_eio(text: &str) -> Option<EioPacket<'_>> {
    let mut chars = text.chars();
    let kind = chars.next()?;
    let rest = &text[1..];
    Some(match kind {
        '0' => EioPacket::Open(serde_json::from_str(rest).unwrap_or(Value::Null)),
        '1' => EioPacket::Close,
        '2' => EioPacket::Ping(rest),
        '3' => EioPacket::Pong(rest),
        '4' => EioPacket::Message(rest),
        '5' => EioPacket::Upgrade,
        '6' => EioPacket::Noop,
        _ => return None,
    })
}

/// One Socket.IO packet (default namespace or another; the namespace is
/// not used by XTS).
#[derive(Debug, Clone, PartialEq)]
pub enum SioPacket {
    Connect(Value),
    Disconnect,
    Event {
        name: String,
        args: Vec<Value>,
    },
    Ack,
    ConnectError(Value),
    BinaryEvent {
        attachments: usize,
        name: String,
        args: Vec<Value>,
    },
    BinaryAck,
}

pub fn decode_sio(payload: &str) -> Option<SioPacket> {
    let b = payload.as_bytes();
    let kind = *b.first()?;
    let mut pos = 1;
    let mut attachments = 0usize;
    if matches!(kind, b'5' | b'6') {
        let start = pos;
        while pos < b.len() && b[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos >= b.len() || b[pos] != b'-' {
            return None;
        }
        attachments = payload[start..pos].parse().ok()?;
        pos += 1;
    }
    if pos < b.len() && b[pos] == b'/' {
        match payload[pos..].find(',') {
            Some(i) => pos += i + 1,
            None => pos = b.len(),
        }
    }
    while pos < b.len() && b[pos].is_ascii_digit() {
        pos += 1;
    }
    let data = &payload[pos..];
    let json = || -> Value { serde_json::from_str(data).unwrap_or(Value::Null) };
    let event = || -> Option<(String, Vec<Value>)> {
        let mut arr = match serde_json::from_str::<Value>(data).ok()? {
            Value::Array(a) => a,
            _ => return None,
        };
        if arr.is_empty() {
            return None;
        }
        let name = arr.remove(0).as_str()?.to_string();
        Some((name, arr))
    };
    Some(match kind {
        b'0' => SioPacket::Connect(json()),
        b'1' => SioPacket::Disconnect,
        b'2' => {
            let (name, args) = event()?;
            SioPacket::Event { name, args }
        }
        b'3' => SioPacket::Ack,
        b'4' => SioPacket::ConnectError(json()),
        b'5' => {
            let (name, args) = event()?;
            SioPacket::BinaryEvent {
                attachments,
                name,
                args,
            }
        }
        b'6' => SioPacket::BinaryAck,
        _ => return None,
    })
}

/// Client connect to the default namespace (Socket.IO v5 requires it).
pub const CONNECT: &str = "40";
/// Engine.IO pong.
pub fn pong(probe: &str) -> String {
    format!("3{}", probe)
}

/// `42["name",arg...]`
pub fn encode_event(name: &str, args: &[Value]) -> String {
    let mut arr = Vec::with_capacity(args.len() + 1);
    arr.push(Value::String(name.to_string()));
    arr.extend(args.iter().cloned());
    format!("42{}", Value::Array(arr))
}

/// Text of a connect error (`44{"message":"..."}`).
pub fn error_message(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(_) => v
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

/// An Engine.IO v3 server prefixes binary frames with `0x04`. An XTS binary
/// packet itself starts with packet type 4 or 260 (little endian), so a
/// frame `04 04 00 ..` or `04 04 01 ..` is the prefixed form.
pub fn strip_eio3_prefix(data: &[u8]) -> &[u8] {
    if data.len() >= 17 && data[0] == 4 {
        let t = u16::from_le_bytes([data[1], data[2]]);
        if t == 4 || t == 260 {
            return &data[1..];
        }
    }
    data
}
