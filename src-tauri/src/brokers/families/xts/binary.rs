//! `xts-binary-packet` and the 1105 text format.
//!
//! Binary packet (jainamxts `ws:584-614`, rmoney `ws:765-783`), little
//! endian, 16-byte header then payload:
//!
//! | offset | type | field |
//! | --- | --- | --- |
//! | 0 | u16 | packet type (4 plain, 260 = 0x104 LZ4 compressed) |
//! | 2 | u16 | message code (1501, 1502, 1512, ...) |
//! | 4 | i16 | exchange segment |
//! | 6 | i32 | exchange instrument id |
//! | 10 | i16 | book type |
//! | 12 | i16 | market type |
//! | 14 | u16 | uncompressed size |
//!
//! Compressed packets are skipped like the web (no LZ4 there either). The
//! payload layout is not documented in the web; both decoders read doubles
//! at offsets the web found by inspection, with the same plausibility
//! windows. They are ported as written, except that the jainam depth
//! decoder does not invent a level at `ltp * 0.999 / 1.001` when it finds
//! none (`ws:1022-1025`): a trader must never see a made-up price.

use serde_json::{json, Map, Value};

pub const HEADER_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub packet_type: u16,
    pub message_code: u16,
    pub segment: i16,
    pub instrument_id: i32,
    pub book_type: i16,
    pub market_type: i16,
    pub uncompressed_size: u16,
}

impl Header {
    pub fn compressed(&self) -> bool {
        self.packet_type & 0x100 != 0
    }
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn i16_at(b: &[u8], o: usize) -> Option<i16> {
    Some(i16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn i32_at(b: &[u8], o: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

fn f64_at(b: &[u8], o: usize) -> Option<f64> {
    Some(f64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn plausible(v: f64) -> bool {
    v > 0.01 && v < 500_000.0
}

pub fn header(data: &[u8]) -> Option<Header> {
    if data.len() < HEADER_LEN {
        return None;
    }
    Some(Header {
        packet_type: u16_at(data, 0)?,
        message_code: u16_at(data, 2)?,
        segment: i16_at(data, 4)?,
        instrument_id: i32_at(data, 6)?,
        book_type: i16_at(data, 10)?,
        market_type: i16_at(data, 12)?,
        uncompressed_size: u16_at(data, 14)?,
    })
}

fn base(h: &Header, code: u16) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("ExchangeSegment".into(), json!(h.segment));
    m.insert("ExchangeInstrumentID".into(), json!(h.instrument_id));
    m.insert("MessageCode".into(), json!(code));
    m
}

// ---------------------------------------------------------------------------
// rmoney (`ws:739-844`)
// ---------------------------------------------------------------------------

fn rmoney_ltp(p: &[u8], code: u16) -> Option<f64> {
    let offsets: &[usize] = match code {
        1512 => &[2, 10, 18, 26, 34, 42],
        1501 => &[48, 52, 92, 156, 164, 172, 180],
        1502 => &[166, 164, 170, 174],
        _ => &[
            2, 10, 18, 26, 34, 42, 48, 52, 85, 92, 156, 164, 166, 172, 180,
        ],
    };
    for &o in offsets {
        if let Some(v) = f64_at(p, o).filter(|v| plausible(*v)) {
            return Some(round2(v));
        }
    }
    let max = p.len().saturating_sub(7).min(220);
    (0..max)
        .filter_map(|o| f64_at(p, o))
        .find(|v| plausible(*v))
        .map(round2)
}

/// rmoney `_on_xts_binary_packet`: a JSON-shaped message for the common
/// normaliser, or `None` (compressed, too short, no plausible LTP).
pub fn decode_rmoney(data: &[u8]) -> Option<Value> {
    let h = header(data)?;
    if h.compressed() {
        return None;
    }
    let p = &data[HEADER_LEN..];
    if p.len() < 2 {
        return None;
    }
    let code = if h.message_code != 0 {
        h.message_code
    } else {
        u16_at(p, 0)?
    };
    if ![1501, 1502, 1505, 1510, 1512].contains(&code) {
        return None;
    }
    let mut m = base(&h, code);
    m.insert("LastTradedPrice".into(), json!(rmoney_ltp(p, code)?));
    if code == 1501 {
        for (k, o) in [("Open", 156), ("High", 164), ("Low", 172), ("Close", 180)] {
            if let Some(v) = f64_at(p, o).filter(|v| plausible(*v)) {
                m.insert(k.into(), json!(round2(v)));
            }
        }
    }
    for o in [188usize, 196, 204, 120, 128, 136] {
        if let Some(v) = u64_at(p, o).filter(|v| *v > 0 && *v < 10_000_000_000) {
            m.insert("TotalTradedQuantity".into(), json!(v));
            break;
        }
        if let Some(v) = u32_at(p, o).filter(|v| *v > 0) {
            m.insert("TotalTradedQuantity".into(), json!(v));
            break;
        }
    }
    Some(Value::Object(m))
}

// ---------------------------------------------------------------------------
// jainamxts (`ws:580-1054`)
// ---------------------------------------------------------------------------

/// `_parse_touchline`: LTP at `offset`; for 1501 OHLC at 156/164/172/180.
fn jainam_touchline(p: &[u8], m: &mut Map<String, Value>, offset: usize, code: u16) {
    let Some(ltp) = f64_at(p, offset) else {
        return;
    };
    if !plausible(ltp) {
        jainam_scan(p, m, code);
        return;
    }
    m.insert("LastTradedPrice".into(), json!(round2(ltp)));
    if code == 1501 && 180 + 8 <= p.len() {
        for (k, o) in [("Open", 156), ("High", 164), ("Low", 172), ("Close", 180)] {
            if let Some(v) = f64_at(p, o).filter(|v| plausible(*v)) {
                m.insert(k.into(), json!(round2(v)));
            }
        }
    }
}

/// `_scan_and_parse`: first double in (1, 500000) within 400 bytes.
fn jainam_scan(p: &[u8], m: &mut Map<String, Value>, code: u16) {
    let max = p.len().saturating_sub(7).min(400);
    if let Some(o) = (0..max).find(|o| f64_at(p, *o).is_some_and(|v| v > 1.0 && v < 500_000.0)) {
        jainam_touchline(p, m, o, code);
    }
}

/// `_parse_ltp_packet` (1512).
fn jainam_ltp(p: &[u8], m: &mut Map<String, Value>) {
    let known = [2usize, 10, 18, 26, 34, 42]
        .into_iter()
        .filter_map(|o| f64_at(p, o))
        .find(|v| plausible(*v));
    let ltp = known.or_else(|| {
        let max = p.len().saturating_sub(7).min(100);
        (0..max)
            .filter_map(|o| f64_at(p, o))
            .find(|v| plausible(*v))
    });
    if let Some(v) = ltp {
        m.insert("LastTradedPrice".into(), json!(round2(v)));
    }
}

/// `_parse_touchline_with_ohlc` (1501): OHLC first, then an LTP inside
/// [min * 0.95, max * 1.05], else the close.
fn jainam_touchline_ohlc(p: &[u8], m: &mut Map<String, Value>) {
    let mut ohlc = Vec::new();
    for (k, o) in [("Open", 156), ("High", 164), ("Low", 172), ("Close", 180)] {
        if let Some(v) = f64_at(p, o).filter(|v| plausible(*v)) {
            m.insert(k.into(), json!(round2(v)));
            ohlc.push(v);
        }
    }
    let (lo, hi) = if ohlc.is_empty() {
        (0.01, 500_000.0)
    } else {
        (
            ohlc.iter().cloned().fold(f64::MAX, f64::min) * 0.95,
            ohlc.iter().cloned().fold(f64::MIN, f64::max) * 1.05,
        )
    };
    let close = m.get("Close").and_then(Value::as_f64).unwrap_or(0.0);
    let inside = |v: &f64| *v > lo && *v < hi;
    let ltp = [2usize, 10, 18, 26, 34, 42, 48, 52]
        .into_iter()
        .filter_map(|o| f64_at(p, o))
        .find(inside)
        .or_else(|| {
            let max = p.len().saturating_sub(7).min(150);
            (0..max).filter_map(|o| f64_at(p, o)).find(inside)
        })
        .or((close > 0.0).then_some(close));
    if let Some(v) = ltp.filter(|v| *v != 0.0) {
        m.insert("LastTradedPrice".into(), json!(round2(v)));
    }
}

fn level(price: f64, size: u32, orders: u16) -> Value {
    json!({
        "Price": round2(price),
        "Size": if size > 100_000_000 { 0 } else { size },
        "TotalOrders": if orders > 50_000 { 0 } else { orders },
    })
}

/// `_parse_depth` (1502): five 22-byte bid levels from offset 52, asks
/// scanned after them, a near-LTP scan as fallback.
fn jainam_depth(p: &[u8], m: &mut Map<String, Value>) {
    let ltp = m
        .get("LastTradedPrice")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if ltp <= 0.0 {
        return;
    }
    const START: usize = 52;
    const LEVEL: usize = 22;
    let mut bids: Vec<(f64, Value)> = Vec::new();
    let mut asks: Vec<(f64, Value)> = Vec::new();
    for i in 0..5 {
        let o = START + i * LEVEL;
        if o + LEVEL > p.len() {
            break;
        }
        let Some(price) = f64_at(p, o).filter(|v| plausible(*v)) else {
            continue;
        };
        let qty = u32_at(p, o + 8).unwrap_or(0);
        let orders = u16_at(p, o + 14).unwrap_or(0);
        bids.push((price, level(price, qty, orders)));
    }
    let ask_start = START + 5 * LEVEL;
    let ask_end = p.len().saturating_sub(20).min(ask_start + 100);
    for o in ask_start..ask_end {
        if asks.len() >= 5 {
            break;
        }
        let Some(price) = f64_at(p, o).filter(|v| plausible(*v) && *v > ltp) else {
            continue;
        };
        let qty = u32_at(p, o + 8).unwrap_or(0);
        let orders = u16_at(p, o + 14).unwrap_or(0);
        asks.push((price, level(price, qty, orders)));
    }
    if bids.len() < 2 || asks.is_empty() {
        bids.clear();
        asks.clear();
        let max = p.len().saturating_sub(7).min(200);
        for o in 0..max {
            let Some(price) = f64_at(p, o) else { continue };
            if plausible(price) && ((price - ltp) / ltp).abs() < 0.05 {
                let l = level(price, 0, 0);
                if price <= ltp {
                    bids.push((price, l));
                } else {
                    asks.push((price, l));
                }
            }
        }
    }
    bids.sort_by(|a, b| b.0.total_cmp(&a.0));
    asks.sort_by(|a, b| a.0.total_cmp(&b.0));
    let take =
        |v: Vec<(f64, Value)>| -> Vec<Value> { v.into_iter().take(5).map(|x| x.1).collect() };
    m.insert("Bids".into(), Value::Array(take(bids)));
    m.insert("Asks".into(), Value::Array(take(asks)));
}

/// jainamxts `_on_xts_binary_packet`, keyed on the header's message code.
pub fn decode_jainam(data: &[u8]) -> Option<Value> {
    let h = header(data)?;
    if data.len() < HEADER_LEN + 1 || h.compressed() {
        return None;
    }
    let p = &data[HEADER_LEN..];
    if p.len() < 2 {
        return None;
    }
    let code = h.message_code;
    let mut m = base(&h, code);
    match code {
        1512 => jainam_ltp(p, &mut m),
        1501 => jainam_touchline_ohlc(p, &mut m),
        1502 => {
            if p.len() >= 166 + 80 {
                jainam_touchline(p, &mut m, 166, code);
            } else {
                jainam_scan(p, &mut m, code);
            }
            jainam_depth(p, &mut m);
        }
        // 1510 and others only ever produced an "LTP" from an OI packet,
        // which the web adapter then drops (unknown message code).
        _ => return None,
    }
    m.contains_key("LastTradedPrice")
        .then_some(Value::Object(m))
}

// ---------------------------------------------------------------------------
// 1105 text (`fivepaisaxts_websocket.py:604-668`)
// ---------------------------------------------------------------------------

/// `t:<seg>_<id>,110:<ltp>,111:<ltq>,...` -> a JSON-shaped message
/// (no `MessageCode`; the caller treats it as a touchline).
pub fn parse_1105(text: &str) -> Option<Value> {
    let mut parts = text.split(',');
    let first = parts.next()?.strip_prefix("t:")?;
    let (seg, id) = first.split_once('_')?;
    let mut m = Map::new();
    m.insert(
        "ExchangeSegment".into(),
        json!(seg.trim().parse::<i64>().ok()?),
    );
    m.insert(
        "ExchangeInstrumentID".into(),
        json!(id.trim().parse::<i64>().ok()?),
    );
    for part in parts {
        let Some((code, value)) = part.split_once(':') else {
            continue;
        };
        let name = match code.trim() {
            "110" => "LastTradedPrice",
            "111" => "LastTradedQuantity",
            "112" => "TotalTradedQuantity",
            "113" => "AverageTradedPrice",
            "114" => "Open",
            "115" => "High",
            "116" => "Low",
            "117" => "Close",
            "118" => "TotalBuyQuantity",
            "119" => "TotalSellQuantity",
            _ => continue,
        };
        if let Ok(v) = value.trim().parse::<f64>() {
            m.insert(name.into(), json!(v));
        }
    }
    Some(Value::Object(m))
}
