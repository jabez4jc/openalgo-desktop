//! Main-database settings and tables behind the admin and monitoring pages:
//! security thresholds (web `settings_db` security columns), the crypto
//! leverage setting (web `leverage_db`) and freeze quantities (web
//! `qty_freeze_db`).

use crate::db::sqlite::migrations::column_exists;
use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    if !column_exists(conn, table, column)? {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

/// Migration `051_security_settings` (web defaults: auto-ban off, 100/0,
/// 100/0, repeat limit 2).
pub fn migrate_security_settings(conn: &Connection) -> Result<()> {
    add_column(
        conn,
        "settings",
        "security_auto_ban_enabled",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column(
        conn,
        "settings",
        "security_404_threshold",
        "INTEGER NOT NULL DEFAULT 100",
    )?;
    add_column(
        conn,
        "settings",
        "security_404_ban_duration",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column(
        conn,
        "settings",
        "security_api_threshold",
        "INTEGER NOT NULL DEFAULT 100",
    )?;
    add_column(
        conn,
        "settings",
        "security_api_ban_duration",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column(
        conn,
        "settings",
        "security_repeat_offender_limit",
        "INTEGER NOT NULL DEFAULT 2",
    )?;
    Ok(())
}

/// Migration `052_leverage_config`: one row, 0 = broker default.
pub fn migrate_leverage(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS leverage_config (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            leverage REAL NOT NULL DEFAULT 0,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
         );
         INSERT OR IGNORE INTO leverage_config (id, leverage) VALUES (1, 0);",
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SecuritySettings {
    pub auto_ban_enabled: bool,
    #[serde(rename = "404_threshold")]
    pub threshold_404: i64,
    #[serde(rename = "404_ban_duration")]
    pub ban_duration_404: i64,
    pub api_threshold: i64,
    pub api_ban_duration: i64,
    pub repeat_offender_limit: i64,
}

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            auto_ban_enabled: false,
            threshold_404: 100,
            ban_duration_404: 0,
            api_threshold: 100,
            api_ban_duration: 0,
            repeat_offender_limit: 2,
        }
    }
}

pub fn security_settings(conn: &Connection) -> Result<SecuritySettings> {
    let s = conn
        .query_row(
            "SELECT security_auto_ban_enabled, security_404_threshold, security_404_ban_duration,
                    security_api_threshold, security_api_ban_duration, security_repeat_offender_limit
             FROM settings WHERE id = 1",
            [],
            |r| {
                Ok(SecuritySettings {
                    auto_ban_enabled: r.get(0)?,
                    threshold_404: r.get(1)?,
                    ban_duration_404: r.get(2)?,
                    api_threshold: r.get(3)?,
                    api_ban_duration: r.get(4)?,
                    repeat_offender_limit: r.get(5)?,
                })
            },
        )
        .optional()?;
    Ok(s.unwrap_or_default())
}

pub fn set_security_settings(conn: &Connection, s: &SecuritySettings) -> Result<()> {
    conn.execute(
        "UPDATE settings SET security_auto_ban_enabled = ?1, security_404_threshold = ?2,
            security_404_ban_duration = ?3, security_api_threshold = ?4,
            security_api_ban_duration = ?5, security_repeat_offender_limit = ?6,
            updated_at = datetime('now')
         WHERE id = 1",
        params![
            s.auto_ban_enabled,
            s.threshold_404,
            s.ban_duration_404,
            s.api_threshold,
            s.api_ban_duration,
            s.repeat_offender_limit
        ],
    )?;
    Ok(())
}

pub fn leverage(conn: &Connection) -> Result<f64> {
    Ok(conn
        .query_row(
            "SELECT leverage FROM leverage_config WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0.0))
}

pub fn set_leverage(conn: &Connection, value: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO leverage_config (id, leverage, updated_at) VALUES (1, ?1, datetime('now'))
         ON CONFLICT(id) DO UPDATE SET leverage = excluded.leverage, updated_at = excluded.updated_at",
        [value as f64],
    )?;
    Ok(())
}

// ------------------------------------------------------------- freeze qty

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FreezeQty {
    pub id: i64,
    pub exchange: String,
    pub symbol: String,
    pub freeze_qty: i64,
}

pub fn freeze_list(conn: &Connection) -> Result<Vec<FreezeQty>> {
    let mut st = conn
        .prepare("SELECT id, exchange, symbol, freeze_qty FROM qty_freeze ORDER BY symbol, id")?;
    let v = st
        .query_map([], |r| {
            Ok(FreezeQty {
                id: r.get(0)?,
                exchange: r.get(1)?,
                symbol: r.get(2)?,
                freeze_qty: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn freeze_get(conn: &Connection, id: i64) -> Result<Option<FreezeQty>> {
    Ok(conn
        .query_row(
            "SELECT id, exchange, symbol, freeze_qty FROM qty_freeze WHERE id = ?1",
            [id],
            |r| {
                Ok(FreezeQty {
                    id: r.get(0)?,
                    exchange: r.get(1)?,
                    symbol: r.get(2)?,
                    freeze_qty: r.get(3)?,
                })
            },
        )
        .optional()?)
}

pub fn freeze_count(conn: &Connection, exchange: Option<&str>) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM qty_freeze WHERE ?1 IS NULL OR exchange = ?1",
        [exchange],
        |r| r.get(0),
    )?)
}

/// Add an entry; `Ok(None)` when the symbol already exists on the exchange.
pub fn freeze_add(
    conn: &Connection,
    exchange: &str,
    symbol: &str,
    qty: i64,
) -> Result<Option<FreezeQty>> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO qty_freeze (exchange, symbol, freeze_qty) VALUES (?1, ?2, ?3)",
        params![exchange, symbol, qty],
    )?;
    if n == 0 {
        return Ok(None);
    }
    freeze_get(conn, conn.last_insert_rowid())
}

pub fn freeze_update(conn: &Connection, id: i64, qty: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE qty_freeze SET freeze_qty = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![qty, id],
    )? > 0)
}

pub fn freeze_delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM qty_freeze WHERE id = ?1", [id])? > 0)
}

/// Split one CSV line, honouring double quotes.
fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Parse the exchange's freeze file (web `load_freeze_qty_from_csv`): a
/// `SYMBOL` column and a column whose name contains `FRZ`. Rows that do not
/// parse are skipped.
pub fn parse_freeze_csv(text: &str) -> Option<Vec<(String, i64)>> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header = csv_fields(lines.next()?);
    let sym = header
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("SYMBOL"))?;
    let frz = header.iter().position(|h| {
        let u = h.trim().to_ascii_uppercase();
        u.contains("FRZ") || u == "VOL_FRZ_QTY"
    })?;
    let mut rows = Vec::new();
    for line in lines {
        let f = csv_fields(line);
        let (Some(s), Some(q)) = (f.get(sym), f.get(frz)) else {
            continue;
        };
        let s = s.trim();
        if s.is_empty() {
            continue;
        }
        if let Ok(q) = q.trim().parse::<i64>() {
            rows.push((s.to_string(), q));
        }
    }
    Some(rows)
}

/// Replace every entry for `exchange` with `rows`, in one transaction.
pub fn freeze_replace_exchange(
    conn: &Connection,
    exchange: &str,
    rows: &[(String, i64)],
) -> Result<()> {
    conn.execute_batch("SAVEPOINT freeze_upload")?;
    let r = (|| -> Result<()> {
        conn.execute("DELETE FROM qty_freeze WHERE exchange = ?1", [exchange])?;
        for (s, q) in rows {
            conn.execute(
                "INSERT INTO qty_freeze (exchange, symbol, freeze_qty) VALUES (?1, ?2, ?3)
                 ON CONFLICT(exchange, symbol) DO UPDATE SET freeze_qty = excluded.freeze_qty",
                params![exchange, s, q],
            )?;
        }
        Ok(())
    })();
    match r {
        Ok(()) => {
            conn.execute_batch("RELEASE freeze_upload")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO freeze_upload; RELEASE freeze_upload");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeze_csv_matches_the_web_loader() {
        let csv = "SYMBOL    ,VOL_FRZ_QTY\nNIFTY,1800\n\"BANKNIFTY\",600\nBAD,x\n,5\n";
        let rows = parse_freeze_csv(csv).unwrap();
        assert_eq!(
            rows,
            vec![("NIFTY".into(), 1800), ("BANKNIFTY".into(), 600)]
        );
        assert!(parse_freeze_csv("A,B\n1,2").is_none());
    }
}
