//! Lenient deserialisers for broker JSON.
//!
//! Brokers send numbers as numbers, as strings, or as `null` (Kite leaves
//! holdings numerics null for scrips it cannot price). `#[serde(default)]`
//! alone rejects an explicit `null`, so one such row would fail a whole
//! book. These coerce like the web's `_to_float` / `_to_int`.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

fn to_f64(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
        Value::Bool(b) => f64::from(u8::from(*b)),
        _ => 0.0,
    }
}

/// Number, numeric string or null -> f64 (0 on anything else).
pub fn f64_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(to_f64(&Value::deserialize(d)?))
}

/// Number, numeric string or null -> i64 (fractions truncated, like
/// Python's `int(float(x))`).
pub fn i64_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match &v {
        Value::Number(n) => n.as_i64().unwrap_or_else(|| to_f64(&v) as i64),
        _ => to_f64(&v) as i64,
    })
}

/// String, number or null -> String (null -> empty).
pub fn string_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Row {
        #[serde(default, deserialize_with = "f64_lenient")]
        a: f64,
        #[serde(default, deserialize_with = "i64_lenient")]
        b: i64,
        #[serde(default, deserialize_with = "string_lenient")]
        c: String,
    }

    #[test]
    fn coerces_like_the_web() {
        let r: Row = serde_json::from_str(r#"{"a":null,"b":"12","c":null}"#).unwrap();
        assert_eq!((r.a, r.b, r.c.as_str()), (0.0, 12, ""));
        let r: Row = serde_json::from_str(r#"{"a":"1.5","b":7.9,"c":42}"#).unwrap();
        assert_eq!((r.a, r.b, r.c.as_str()), (1.5, 7, "42"));
        let r: Row = serde_json::from_str("{}").unwrap();
        assert_eq!((r.a, r.b), (0.0, 0));
    }
}
