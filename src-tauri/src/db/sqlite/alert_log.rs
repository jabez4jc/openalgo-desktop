//! Chart alert firings (web `database/alert_log_db.py`): one row per fire,
//! written by the /trading page and read back by its Log tab.
//!
//! Bounded two ways: rows older than [`RETENTION_DAYS`] are trimmed on write
//! (at most [`TRIM_BATCH`] per write, as on the web), and a user never keeps
//! more than [`MAX_ROWS_PER_USER`] rows, so a runaway alert cannot grow the
//! table without limit inside the retention window.

use crate::error::Result;
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};

pub const MAX_LOG_PAGE: i64 = 200;
pub const RETENTION_DAYS: i64 = 90;
pub const TRIM_BATCH: i64 = 500;
pub const MAX_ROWS_PER_USER: i64 = 10_000;

const TS: &str = "%Y-%m-%d %H:%M:%S%.6f";

/// Migration `062_alert_log`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS alert_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id TEXT NOT NULL,
            alert_id TEXT NOT NULL,
            title TEXT NOT NULL DEFAULT '',
            kind TEXT NOT NULL DEFAULT 'price',
            condition TEXT NOT NULL DEFAULT '',
            symbol TEXT NOT NULL DEFAULT '',
            exchange TEXT NOT NULL DEFAULT '',
            interval TEXT NOT NULL DEFAULT '',
            price REAL,
            message TEXT,
            delivered TEXT NOT NULL DEFAULT '',
            fired_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS ix_alert_log_user_fired ON alert_log(user_id, fired_at);",
    )?;
    Ok(())
}

/// One firing as the chart reported it.
#[derive(Debug, Clone, Default)]
pub struct Fire {
    pub alert_id: String,
    pub title: String,
    pub kind: String,
    pub condition: String,
    pub symbol: String,
    pub exchange: String,
    pub interval: String,
    pub price: Option<f64>,
    pub message: Option<String>,
    pub delivered: String,
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(false)) => String::new(),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => String::new(),
        Some(Value::Bool(true)) => "True".into(),
        Some(other) => other.to_string(),
    }
}

/// Web `_as_float`: a finite number (a numeric string counts) or `None`.
fn as_float(v: Option<&Value>) -> Option<f64> {
    let f = match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }?;
    f.is_finite().then_some(f)
}

/// Web `_channels`: unique names, sorted, whole names within 64 characters.
pub fn channels(delivered: &[Value]) -> String {
    let mut names: Vec<String> = delivered
        .iter()
        .map(|v| match v {
            Value::String(s) => s.trim().to_string(),
            other => other.to_string().trim().to_string(),
        })
        .filter(|s| !s.is_empty())
        .collect();
    names.sort();
    names.dedup();
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;
    for name in names {
        let cost = name.chars().count() + usize::from(!kept.is_empty());
        if used + cost > 64 {
            break;
        }
        used += cost;
        kept.push(name);
    }
    kept.join(",")
}

impl Fire {
    /// Read a chart payload defensively, with the web's field limits.
    pub fn from_payload(p: &serde_json::Map<String, Value>, delivered: &[Value]) -> Option<Self> {
        let alert_id = cut(text(p.get("alertId")).trim(), 64);
        if alert_id.is_empty() {
            return None;
        }
        let kind = text(p.get("kind"));
        let message = cut(&text(p.get("message")), 2000);
        Some(Self {
            alert_id,
            title: cut(&text(p.get("title")), 200),
            kind: cut(if kind.is_empty() { "price" } else { &kind }, 24),
            condition: cut(&text(p.get("condition")), 24),
            symbol: cut(&text(p.get("symbol")), 60),
            exchange: cut(&text(p.get("exchange")), 20),
            interval: cut(&text(p.get("interval")), 16),
            price: as_float(p.get("price")),
            message: (!message.is_empty()).then_some(message),
            delivered: channels(delivered),
        })
    }
}

fn epoch(ts: &str) -> Value {
    NaiveDateTime::parse_from_str(ts, TS)
        .or_else(|_| NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S"))
        .map(|d| {
            let t = d.and_utc();
            json!(t.timestamp() as f64 + f64::from(t.timestamp_subsec_micros()) / 1e6)
        })
        .unwrap_or(Value::Null)
}

fn to_json(r: &Row) -> rusqlite::Result<Value> {
    let delivered: String = r.get("delivered")?;
    let message: Option<String> = r.get("message")?;
    let fired_at: String = r.get("fired_at")?;
    Ok(json!({
        "id": r.get::<_, i64>("id")?,
        "alertId": r.get::<_, String>("alert_id")?,
        "title": r.get::<_, String>("title")?,
        "kind": r.get::<_, String>("kind")?,
        "condition": r.get::<_, String>("condition")?,
        "symbol": r.get::<_, String>("symbol")?,
        "exchange": r.get::<_, String>("exchange")?,
        "interval": r.get::<_, String>("interval")?,
        "price": r.get::<_, Option<f64>>("price")?,
        "message": message.unwrap_or_default(),
        "delivered": delivered.split(',').filter(|s| !s.is_empty()).collect::<Vec<_>>(),
        "firedAt": epoch(&fired_at),
    }))
}

/// Write one firing and return it; housekeeping failures never fail it.
pub fn record(conn: &Connection, user: &str, fire: &Fire, now: DateTime<Utc>) -> Result<Value> {
    conn.execute(
        "INSERT INTO alert_log (user_id, alert_id, title, kind, condition, symbol, exchange, interval,
                                price, message, delivered, fired_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            user,
            fire.alert_id,
            fire.title,
            fire.kind,
            fire.condition,
            fire.symbol,
            fire.exchange,
            fire.interval,
            fire.price,
            fire.message,
            fire.delivered,
            now.naive_utc().format(TS).to_string(),
        ],
    )?;
    let id = conn.last_insert_rowid();
    let row = conn.query_row(
        "SELECT * FROM alert_log WHERE id = ?1",
        params![id],
        to_json,
    )?;
    if let Err(e) = trim(conn, user, now) {
        tracing::warn!("Could not trim the alert log: {}", e);
    }
    Ok(row)
}

fn trim(conn: &Connection, user: &str, now: DateTime<Utc>) -> Result<()> {
    let cutoff = (now - Duration::days(RETENTION_DAYS))
        .naive_utc()
        .format(TS)
        .to_string();
    conn.execute(
        "DELETE FROM alert_log WHERE id IN (
            SELECT id FROM alert_log WHERE user_id = ?1 AND fired_at < ?2 LIMIT ?3)",
        params![user, cutoff, TRIM_BATCH],
    )?;
    // Hard cap: the newest MAX_ROWS_PER_USER rows stay.
    let edge: Option<i64> = conn
        .query_row(
            "SELECT id FROM alert_log WHERE user_id = ?1 ORDER BY fired_at DESC, id DESC
             LIMIT 1 OFFSET ?2",
            params![user, MAX_ROWS_PER_USER],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(edge) = edge {
        conn.execute(
            "DELETE FROM alert_log WHERE id IN (
                SELECT id FROM alert_log WHERE user_id = ?1 AND id <= ?2 ORDER BY id LIMIT ?3)",
            params![user, edge, TRIM_BATCH],
        )?;
    }
    Ok(())
}

/// Web `list_fires`: newest first, `limit` clamped to 1..=200 (anything
/// unreadable is 200).
pub fn list(conn: &Connection, user: &str, limit: Option<&str>) -> Result<Vec<Value>> {
    let capped = match limit.map(str::trim) {
        None | Some("") => MAX_LOG_PAGE,
        Some(s) => match s.parse::<i64>() {
            Ok(n) => n.clamp(1, MAX_LOG_PAGE),
            Err(_) => MAX_LOG_PAGE,
        },
    };
    let mut stmt = conn.prepare_cached(
        "SELECT * FROM alert_log WHERE user_id = ?1 ORDER BY fired_at DESC, id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![user, capped], to_json)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Delete the user's log, or one alert's rows. Returns the count.
pub fn clear(conn: &Connection, user: &str, alert_id: Option<&str>) -> Result<i64> {
    let n = match alert_id.filter(|a| !a.is_empty()) {
        Some(a) => conn.execute(
            "DELETE FROM alert_log WHERE user_id = ?1 AND alert_id = ?2",
            params![user, cut(a, 64)],
        )?,
        None => conn.execute("DELETE FROM alert_log WHERE user_id = ?1", params![user])?,
    };
    Ok(n as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn records_lists_trims_and_clears() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let p = json!({"alertId": "a1", "price": "101.5", "title": "x"});
        let f = Fire::from_payload(
            p.as_object().unwrap(),
            &[
                json!("whatsapp"),
                json!("telegram"),
                json!("telegram"),
                json!(" "),
            ],
        )
        .unwrap();
        assert_eq!(f.delivered, "telegram,whatsapp");
        let row = record(&c, "u", &f, t0).unwrap();
        assert_eq!(row["price"], 101.5);
        assert_eq!(row["kind"], "price");
        assert_eq!(row["delivered"], json!(["telegram", "whatsapp"]));
        assert_eq!(row["firedAt"], json!(t0.timestamp() as f64));
        // 91 days later the old row is trimmed by the next write.
        record(&c, "u", &f, t0 + Duration::days(91)).unwrap();
        assert_eq!(list(&c, "u", None).unwrap().len(), 1);
        assert_eq!(list(&c, "u", Some("x")).unwrap().len(), 1);
        assert_eq!(clear(&c, "u", Some("nope")).unwrap(), 0);
        assert_eq!(clear(&c, "u", None).unwrap(), 1);
        assert!(Fire::from_payload(json!({"alertId": "  "}).as_object().unwrap(), &[]).is_none());
        let long: Vec<Value> = (0..20).map(|i| json!(format!("channel{i:02}"))).collect();
        assert!(channels(&long).len() <= 64);
    }

    #[test]
    fn rows_per_user_are_capped() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        c.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO alert_log (user_id, alert_id, fired_at) SELECT 'u', 'a', ?2 FROM n",
            params![MAX_ROWS_PER_USER, t0.naive_utc().format(TS).to_string()],
        )
        .unwrap();
        let f = Fire::from_payload(json!({"alertId": "b"}).as_object().unwrap(), &[]).unwrap();
        record(&c, "u", &f, t0 + Duration::seconds(1)).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM alert_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, MAX_ROWS_PER_USER);
    }
}
