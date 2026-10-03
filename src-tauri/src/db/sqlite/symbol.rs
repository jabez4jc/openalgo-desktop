//! Symbol master persistence (`symtoken`, web `SymToken` columns).
//!
//! The table is a cache of the broker's master contract: it is replaced
//! wholesale on every download and read once at start-up into the
//! in-memory `SymbolResolver`. Runtime lookups never touch SQLite.

use crate::error::Result;
use crate::state::SymbolInfo;
use rusqlite::{params, Connection};

/// Columns, in the web's order.
const COLUMNS: &str =
    "symbol, brsymbol, name, exchange, brexchange, token, expiry, strike, lotsize, instrumenttype, tick_size";

const CREATE_TABLE: &str = r#"
CREATE TABLE symtoken (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol TEXT NOT NULL,
    brsymbol TEXT NOT NULL,
    name TEXT,
    exchange TEXT,
    brexchange TEXT,
    token TEXT,
    expiry TEXT,
    strike REAL,
    lotsize INTEGER,
    instrumenttype TEXT,
    tick_size REAL
)"#;

const CREATE_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_symtoken_symbol_exchange ON symtoken(symbol, exchange);
CREATE INDEX IF NOT EXISTS idx_symtoken_brsymbol_exchange ON symtoken(brsymbol, exchange);
CREATE INDEX IF NOT EXISTS idx_symtoken_token ON symtoken(token);
CREATE INDEX IF NOT EXISTS idx_symtoken_exchange ON symtoken(exchange);
CREATE INDEX IF NOT EXISTS idx_symtoken_name_exchange ON symtoken(name, exchange);
"#;

fn column_exists(conn: &Connection, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(symtoken)")?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(names.iter().any(|n| n == column))
}

fn table_exists(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='symtoken')",
        [],
        |r| r.get(0),
    )?)
}

/// Migration 043: rebuild `symtoken` with every web `SymToken` column
/// (`brsymbol`, `brexchange`, `expiry`, `strike`, `lotsize`,
/// `instrumenttype`, `tick_size`) and no `UNIQUE(exchange, symbol)` (brokers
/// list duplicate symbols; rows are distinct by token). Idempotent: a table
/// that already has `lotsize` is left alone. Existing rows are carried over
/// (old `lot_size` / `instrument_type` / nullable broker columns backfilled
/// from what is there) so a populated master survives the upgrade.
pub fn migrate_symtoken(conn: &Connection) -> Result<()> {
    if table_exists(conn)? && column_exists(conn, "lotsize")? {
        conn.execute_batch(CREATE_INDEXES)?;
        return Ok(());
    }
    if !table_exists(conn)? {
        conn.execute_batch(CREATE_TABLE)?;
        conn.execute_batch(CREATE_INDEXES)?;
        return Ok(());
    }
    let has = |c: &str| column_exists(conn, c);
    let brsymbol = if has("brsymbol")? {
        "COALESCE(NULLIF(brsymbol, ''), symbol)"
    } else {
        "symbol"
    };
    let brexchange = if has("brexchange")? {
        "COALESCE(NULLIF(brexchange, ''), exchange)"
    } else {
        "exchange"
    };
    let expiry = if has("expiry")? {
        "COALESCE(expiry, '')"
    } else {
        "''"
    };
    let strike = if has("strike")? {
        "COALESCE(strike, 0)"
    } else {
        "0"
    };
    let lotsize = if has("lot_size")? { "lot_size" } else { "1" };
    let itype = if has("instrument_type")? {
        "instrument_type"
    } else {
        "'EQ'"
    };
    let tick = if has("tick_size")? {
        "tick_size"
    } else {
        "0.05"
    };
    conn.execute_batch("ALTER TABLE symtoken RENAME TO symtoken_old")?;
    // Old indexes keep their names after the rename; drop them so the new
    // table can use the canonical names.
    conn.execute_batch(
        "DROP INDEX IF EXISTS idx_symtoken_exchange;
         DROP INDEX IF EXISTS idx_symtoken_token;
         DROP INDEX IF EXISTS idx_symtoken_symbol;
         DROP INDEX IF EXISTS idx_symtoken_brsymbol;",
    )?;
    conn.execute_batch(CREATE_TABLE)?;
    conn.execute_batch(&format!(
        "INSERT INTO symtoken ({cols})
         SELECT symbol, {brsymbol}, name, exchange, {brexchange}, token, {expiry}, {strike},
                {lotsize}, {itype}, {tick}
         FROM symtoken_old ORDER BY id",
        cols = COLUMNS,
    ))?;
    conn.execute_batch("DROP TABLE symtoken_old")?;
    conn.execute_batch(CREATE_INDEXES)?;
    Ok(())
}

/// Replace the whole master in one transaction. Indexes are dropped for the
/// bulk insert and rebuilt once at the end, which is several times faster
/// than maintaining them per row on a 100k-row master.
pub fn store_symbols(conn: &mut Connection, symbols: &[SymbolInfo]) -> Result<()> {
    let start = std::time::Instant::now();
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM symtoken", [])?;
    tx.execute_batch(
        "DROP INDEX IF EXISTS idx_symtoken_symbol_exchange;
         DROP INDEX IF EXISTS idx_symtoken_brsymbol_exchange;
         DROP INDEX IF EXISTS idx_symtoken_token;
         DROP INDEX IF EXISTS idx_symtoken_exchange;
         DROP INDEX IF EXISTS idx_symtoken_name_exchange;",
    )?;
    {
        let mut stmt = tx.prepare(&format!(
            "INSERT INTO symtoken ({}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            COLUMNS
        ))?;
        for s in symbols {
            stmt.execute(params![
                s.symbol,
                s.brsymbol,
                s.name,
                s.exchange,
                s.brexchange,
                s.token,
                s.expiry,
                s.strike,
                s.lot_size,
                s.instrument_type,
                s.tick_size,
            ])?;
        }
    }
    tx.execute_batch(CREATE_INDEXES)?;
    tx.commit()?;
    tracing::info!(
        "Stored {} instruments in {:.2}s",
        symbols.len(),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolInfo> {
    Ok(SymbolInfo {
        symbol: r.get(0)?,
        brsymbol: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        exchange: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        brexchange: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        token: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        expiry: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        strike: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
        lot_size: r.get::<_, Option<i32>>(8)?.unwrap_or(1),
        instrument_type: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
        tick_size: r.get::<_, Option<f64>>(10)?.unwrap_or(0.0),
    })
}

/// Load the whole master (start-up). The caller hands it straight to the
/// resolver, which keeps the only copy.
pub fn load_symbols(conn: &Connection) -> Result<Vec<SymbolInfo>> {
    let mut stmt = conn.prepare(&format!("SELECT {} FROM symtoken ORDER BY id", COLUMNS))?;
    let rows = stmt
        .query_map([], row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn count_symbols(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM symtoken", [], |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brokers::common::symbols::tests::row as sym;

    fn legacy_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_legacy_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn migration_rebuilds_a_populated_legacy_table() {
        let conn = legacy_db();
        conn.execute_batch(
            "INSERT INTO symtoken (symbol, token, exchange, name, lot_size, tick_size, instrument_type, expiry, strike, brsymbol, brexchange)
             VALUES ('SBIN', '3045', 'NSE', 'SBIN', 1, 0.05, 'EQ', NULL, NULL, 'SBIN-EQ', 'NSE'),
                    ('NIFTY27OCT26FUT', '9', 'NFO', 'NIFTY', 65, 0.1, 'FUT', '27-OCT-26', 0, NULL, NULL);",
        )
        .unwrap();
        migrate_symtoken(&conn).unwrap();
        let rows = load_symbols(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].brsymbol, "SBIN-EQ");
        assert_eq!(rows[0].expiry, "");
        assert_eq!(rows[1].brsymbol, "NIFTY27OCT26FUT");
        assert_eq!(rows[1].brexchange, "NFO");
        assert_eq!(rows[1].lot_size, 65);
        assert_eq!(rows[1].expiry, "27-OCT-26");
        assert_eq!(rows[1].instrument_type, "FUT");
        // Idempotent.
        migrate_symtoken(&conn).unwrap();
        assert_eq!(count_symbols(&conn).unwrap(), 2);
        assert!(column_exists(&conn, "instrumenttype").unwrap());
        assert!(!column_exists(&conn, "lot_size").unwrap());
    }

    #[test]
    fn full_run_creates_the_web_schema_and_round_trips_every_column() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
        let mut conn = conn;
        let mut fut = sym(
            "NIFTY27OCT26FUT",
            "NIFTY26OCTFUT",
            "NFO",
            "10011906::::39109",
        );
        fut.expiry = "27-OCT-26".into();
        fut.strike = 0.0;
        fut.lot_size = 65;
        fut.instrument_type = "FUT".into();
        fut.tick_size = 0.1;
        let mut opt = fut.clone();
        opt.symbol = "NIFTY27OCT2625000CE".into();
        opt.token = "2".into();
        opt.strike = 25000.0;
        opt.instrument_type = "CE".into();
        // Duplicate symbol on one exchange is allowed (no UNIQUE constraint).
        let mut dup = opt.clone();
        dup.token = "3".into();
        store_symbols(&mut conn, &[fut.clone(), opt.clone(), dup]).unwrap();
        let rows = load_symbols(&conn).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], fut);
        assert_eq!(rows[1], opt);
        // Replaced wholesale.
        store_symbols(&mut conn, &[fut.clone()]).unwrap();
        assert_eq!(count_symbols(&conn).unwrap(), 1);
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND tbl_name='symtoken'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(idx >= 5);
    }

    #[test]
    fn bulk_store_is_fast() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::sqlite::migrations::run_migrations(&conn).unwrap();
        let mut conn = conn;
        let rows: Vec<SymbolInfo> = (0..100_000)
            .map(|i| {
                sym(
                    &format!("S{}", i),
                    &format!("S{}-EQ", i),
                    "NSE",
                    &i.to_string(),
                )
            })
            .collect();
        let t = std::time::Instant::now();
        store_symbols(&mut conn, &rows).unwrap();
        let loaded = load_symbols(&conn).unwrap();
        assert_eq!(loaded.len(), 100_000);
        // Generous bound for slow CI runners; locally this is well under 2 s.
        assert!(t.elapsed() < std::time::Duration::from_secs(30));
    }
}
