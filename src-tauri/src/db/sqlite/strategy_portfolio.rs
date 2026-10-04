//! Strategy Builder portfolio (web `database/strategy_portfolio_db.py`):
//! saved strategies in two fixed watchlists, `mytrades` and `simulation`.
//! Single user, so no user column, like the web. Legs are stored as the
//! JSON the builder sent; timestamps are naive UTC ISO strings, as the
//! web's `isoformat()` prints them.

use crate::error::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::{json, Value};

pub const WATCHLISTS: [&str; 2] = ["mytrades", "simulation"];

/// Migration `063_strategy_portfolio`: the web's table and index.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS strategy_portfolio (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            watchlist VARCHAR(20) NOT NULL,
            name VARCHAR(120) NOT NULL,
            underlying VARCHAR(40) NOT NULL,
            exchange VARCHAR(20) NOT NULL,
            expiry VARCHAR(20),
            legs_json TEXT NOT NULL DEFAULT '[]',
            notes TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS ix_strategy_portfolio_watchlist
            ON strategy_portfolio(watchlist);",
    )?;
    Ok(())
}

/// One entry to save.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub watchlist: String,
    pub name: String,
    pub underlying: String,
    pub exchange: String,
    pub expiry: Option<String>,
    pub legs: Value,
    pub notes: Option<String>,
}

/// Python `datetime.isoformat()` of a naive UTC time.
pub fn iso(t: DateTime<Utc>) -> String {
    let n = t.naive_utc();
    if t.timestamp_subsec_micros() == 0 {
        n.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        n.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    }
}

fn serialize(r: &Row<'_>) -> rusqlite::Result<Value> {
    let legs: String = r.get(6)?;
    let legs: Value = serde_json::from_str(&legs).unwrap_or_else(|_| json!([]));
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "watchlist": r.get::<_, String>(1)?,
        "name": r.get::<_, String>(2)?,
        "underlying": r.get::<_, String>(3)?,
        "exchange": r.get::<_, String>(4)?,
        "expiry": r.get::<_, Option<String>>(5)?,
        "legs": legs,
        "notes": r.get::<_, Option<String>>(7)?,
        "created_at": r.get::<_, Option<String>>(8)?,
        "updated_at": r.get::<_, Option<String>>(9)?,
    }))
}

const COLUMNS: &str =
    "id, watchlist, name, underlying, exchange, expiry, legs_json, notes, created_at, updated_at";

/// Every entry, newest update first, optionally one watchlist only.
pub fn list(conn: &Connection, watchlist: Option<&str>) -> Result<Vec<Value>> {
    let sql = format!(
        "SELECT {} FROM strategy_portfolio WHERE (?1 IS NULL OR watchlist = ?1)
         ORDER BY updated_at DESC, id DESC",
        COLUMNS
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params![watchlist], serialize)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Value>> {
    let sql = format!("SELECT {} FROM strategy_portfolio WHERE id = ?1", COLUMNS);
    Ok(conn.query_row(&sql, params![id], serialize).optional()?)
}

/// Create (`id` None) or replace an entry; `None` when the id is unknown.
pub fn save(
    conn: &Connection,
    id: Option<i64>,
    e: &Entry,
    now: DateTime<Utc>,
) -> Result<Option<Value>> {
    let legs = serde_json::to_string(&e.legs).unwrap_or_else(|_| "[]".into());
    let ts = iso(now);
    let id = match id {
        Some(id) => {
            let n = conn.execute(
                "UPDATE strategy_portfolio SET name = ?1, watchlist = ?2, underlying = ?3,
                    exchange = ?4, expiry = ?5, legs_json = ?6, notes = ?7, updated_at = ?8
                 WHERE id = ?9",
                params![
                    e.name,
                    e.watchlist,
                    e.underlying,
                    e.exchange,
                    e.expiry,
                    legs,
                    e.notes,
                    ts,
                    id
                ],
            )?;
            if n == 0 {
                return Ok(None);
            }
            id
        }
        None => {
            conn.execute(
                "INSERT INTO strategy_portfolio
                    (watchlist, name, underlying, exchange, expiry, legs_json, notes, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![e.watchlist, e.name, e.underlying, e.exchange, e.expiry, legs, e.notes, ts],
            )?;
            conn.last_insert_rowid()
        }
    };
    get(conn, id)
}

/// Delete; false when the id is unknown.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM strategy_portfolio WHERE id = ?1", params![id])? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(name: &str, wl: &str) -> Entry {
        Entry {
            watchlist: wl.into(),
            name: name.into(),
            underlying: "NIFTY".into(),
            exchange: "NFO".into(),
            expiry: Some("30OCT26".into()),
            legs: json!([{"symbol": "NIFTY30OCT2625000CE", "side": "SELL", "lots": 1}]),
            notes: None,
        }
    }

    #[test]
    fn crud_round_trip_and_migration_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 3, 8, 30, 0).unwrap();
        let t1 = t0 + chrono::Duration::milliseconds(1500);
        let a = save(&conn, None, &entry("Short straddle", "mytrades"), t0)
            .unwrap()
            .unwrap();
        assert_eq!(a["created_at"], "2026-10-03T08:30:00");
        assert_eq!(a["legs"][0]["side"], "SELL");
        let b = save(&conn, None, &entry("Iron fly", "simulation"), t1)
            .unwrap()
            .unwrap();
        assert_eq!(b["updated_at"], "2026-10-03T08:30:01.500000");
        let all = list(&conn, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0]["name"], "Iron fly");
        assert_eq!(list(&conn, Some("mytrades")).unwrap().len(), 1);

        let id = a["id"].as_i64().unwrap();
        let mut e = entry("Renamed", "simulation");
        e.notes = Some("hedge".into());
        let u = save(&conn, Some(id), &e, t1).unwrap().unwrap();
        assert_eq!(u["name"], "Renamed");
        assert_eq!(u["created_at"], "2026-10-03T08:30:00");
        assert_eq!(u["notes"], "hedge");
        assert!(save(&conn, Some(999), &e, t1).unwrap().is_none());
        assert!(delete(&conn, id).unwrap());
        assert!(!delete(&conn, id).unwrap());
        assert!(get(&conn, id).unwrap().is_none());
    }
}
