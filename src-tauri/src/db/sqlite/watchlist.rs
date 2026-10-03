//! Charting-terminal watchlists (web `database/watchlist_db.py`): named
//! lists of instruments, bounded per user and per list.

use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// Instruments in one list.
pub const MAX_ITEMS_PER_LIST: usize = 250;
/// Lists per user.
pub const MAX_LISTS_PER_USER: i64 = 50;

/// Migration `061_watchlists`.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS watchlists (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id TEXT NOT NULL,
            name TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            CONSTRAINT uq_watchlist_user_name UNIQUE (user_id, name)
         );
         CREATE INDEX IF NOT EXISTS ix_watchlists_user_id ON watchlists(user_id);
         CREATE TABLE IF NOT EXISTS watchlist_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            watchlist_id INTEGER NOT NULL REFERENCES watchlists(id) ON DELETE CASCADE,
            symbol TEXT NOT NULL,
            exchange TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            CONSTRAINT uq_watchlist_item UNIQUE (watchlist_id, symbol, exchange)
         );
         CREATE INDEX IF NOT EXISTS ix_watchlist_items_watchlist_id ON watchlist_items(watchlist_id);",
    )?;
    Ok(())
}

fn item_json(id: i64, symbol: &str, exchange: &str, position: i64) -> Value {
    json!({"id": id, "symbol": symbol, "exchange": exchange, "position": position})
}

fn items_of(conn: &Connection, watchlist_id: i64) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, symbol, exchange, position FROM watchlist_items WHERE watchlist_id = ?1
         ORDER BY position, id",
    )?;
    let rows = stmt.query_map(params![watchlist_id], |r| {
        Ok(item_json(
            r.get(0)?,
            &r.get::<_, String>(1)?,
            &r.get::<_, String>(2)?,
            r.get(3)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn serialize(conn: &Connection, id: i64, name: &str, position: i64) -> Result<Value> {
    Ok(json!({"id": id, "name": name, "position": position, "items": items_of(conn, id)?}))
}

fn owned(conn: &Connection, user: &str, id: i64) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM watchlists WHERE id = ?1 AND user_id = ?2",
            params![id, user],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Every list of a user, ordered, each with its instruments.
pub fn lists(conn: &Connection, user: &str) -> Result<Vec<Value>> {
    let rows: Vec<(i64, String, i64)> = {
        let mut stmt = conn.prepare_cached(
            "SELECT id, name, position FROM watchlists WHERE user_id = ?1 ORDER BY position, id",
        )?;
        let rows = stmt.query_map(params![user], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    rows.iter()
        .map(|(id, name, pos)| serialize(conn, *id, name, *pos))
        .collect()
}

/// Create a list, optionally pre-filled. `None` when the name is taken or
/// the user is at the list cap.
pub fn create(
    conn: &Connection,
    user: &str,
    name: &str,
    items: &[(String, String)],
) -> Result<Option<Value>> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    let tx = conn.unchecked_transaction()?;
    let clash = tx
        .query_row(
            "SELECT 1 FROM watchlists WHERE user_id = ?1 AND name = ?2",
            params![user, name],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if clash {
        return Ok(None);
    }
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM watchlists WHERE user_id = ?1",
        params![user],
        |r| r.get(0),
    )?;
    if count >= MAX_LISTS_PER_USER {
        tracing::warn!("Watchlist cap reached ({} lists)", count);
        return Ok(None);
    }
    tx.execute(
        "INSERT INTO watchlists (user_id, name, position) VALUES (?1, ?2, ?3)",
        params![user, name, count],
    )?;
    let id = tx.last_insert_rowid();
    for (position, (symbol, exchange)) in items.iter().take(MAX_ITEMS_PER_LIST).enumerate() {
        let (s, e) = (symbol.trim().to_uppercase(), exchange.trim().to_uppercase());
        if !s.is_empty() && !e.is_empty() {
            tx.execute(
                "INSERT OR IGNORE INTO watchlist_items (watchlist_id, symbol, exchange, position)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, s, e, position as i64],
            )?;
        }
    }
    let out = serialize(&tx, id, name, count)?;
    tx.commit()?;
    Ok(Some(out))
}

/// Rename a list. False when it is missing or the name is used.
pub fn rename(conn: &Connection, user: &str, id: i64, name: &str) -> Result<bool> {
    let name = name.trim();
    if name.is_empty() || !owned(conn, user, id)? {
        return Ok(false);
    }
    let clash = conn
        .query_row(
            "SELECT 1 FROM watchlists WHERE user_id = ?1 AND name = ?2 AND id != ?3",
            params![user, name, id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if clash {
        return Ok(false);
    }
    conn.execute(
        "UPDATE watchlists SET name = ?1, updated_at = datetime('now') WHERE id = ?2 AND user_id = ?3",
        params![name, id, user],
    )?;
    Ok(true)
}

/// Remove a list and its instruments.
pub fn delete(conn: &Connection, user: &str, id: i64) -> Result<bool> {
    if !owned(conn, user, id)? {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM watchlist_items WHERE watchlist_id = ?1",
        params![id],
    )?;
    tx.execute("DELETE FROM watchlists WHERE id = ?1", params![id])?;
    tx.commit()?;
    Ok(true)
}

/// Empty a list, keeping the list.
pub fn clear(conn: &Connection, user: &str, id: i64) -> Result<bool> {
    if !owned(conn, user, id)? {
        return Ok(false);
    }
    conn.execute(
        "DELETE FROM watchlist_items WHERE watchlist_id = ?1",
        params![id],
    )?;
    Ok(true)
}

/// Append an instrument; an existing one is returned as is. `None` when the
/// list is missing or full.
pub fn add_item(
    conn: &Connection,
    user: &str,
    id: i64,
    symbol: &str,
    exchange: &str,
) -> Result<Option<Value>> {
    let (symbol, exchange) = (symbol.trim().to_uppercase(), exchange.trim().to_uppercase());
    if symbol.is_empty() || exchange.is_empty() || !owned(conn, user, id)? {
        return Ok(None);
    }
    if let Some((iid, pos)) = conn
        .query_row(
            "SELECT id, position FROM watchlist_items WHERE watchlist_id = ?1 AND symbol = ?2 AND exchange = ?3",
            params![id, symbol, exchange],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?
    {
        return Ok(Some(item_json(iid, &symbol, &exchange, pos)));
    }
    let (count, max): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(MAX(position), -1) FROM watchlist_items WHERE watchlist_id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if count as usize >= MAX_ITEMS_PER_LIST {
        tracing::warn!("Watchlist {} is full", id);
        return Ok(None);
    }
    conn.execute(
        "INSERT INTO watchlist_items (watchlist_id, symbol, exchange, position) VALUES (?1, ?2, ?3, ?4)",
        params![id, symbol, exchange, max + 1],
    )?;
    Ok(Some(item_json(
        conn.last_insert_rowid(),
        &symbol,
        &exchange,
        max + 1,
    )))
}

pub fn remove_item(conn: &Connection, user: &str, id: i64, item_id: i64) -> Result<bool> {
    if !owned(conn, user, id)? {
        return Ok(false);
    }
    let n = conn.execute(
        "DELETE FROM watchlist_items WHERE id = ?1 AND watchlist_id = ?2",
        params![item_id, id],
    )?;
    Ok(n == 1)
}

/// Rewrite the display order. Unknown ids are ignored; items the caller left
/// out keep their relative order after the ones it sent.
pub fn reorder(conn: &Connection, user: &str, id: i64, order: &[i64]) -> Result<bool> {
    if !owned(conn, user, id)? {
        return Ok(false);
    }
    let current: Vec<i64> = {
        let mut stmt = conn.prepare_cached(
            "SELECT id FROM watchlist_items WHERE watchlist_id = ?1 ORDER BY position, id",
        )?;
        let rows = stmt.query_map(params![id], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut remaining = current.clone();
    let mut sequence: Vec<i64> = Vec::with_capacity(current.len());
    for i in order {
        if let Some(p) = remaining.iter().position(|x| x == i) {
            sequence.push(remaining.remove(p));
        }
    }
    sequence.extend(remaining);
    let tx = conn.unchecked_transaction()?;
    for (pos, iid) in sequence.iter().enumerate() {
        tx.execute(
            "UPDATE watchlist_items SET position = ?1 WHERE id = ?2",
            params![pos as i64, iid],
        )?;
    }
    tx.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crud_caps_and_reorder() {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        let l = create(&c, "u", " Main ", &[("sbin".into(), "nse".into())])
            .unwrap()
            .unwrap();
        assert_eq!(l["name"], "Main");
        assert_eq!(l["items"][0]["symbol"], "SBIN");
        let id = l["id"].as_i64().unwrap();
        assert!(create(&c, "u", "Main", &[]).unwrap().is_none());
        let a = add_item(&c, "u", id, "infy", "NSE").unwrap().unwrap();
        assert_eq!(a["position"], 1);
        let again = add_item(&c, "u", id, "INFY", "nse").unwrap().unwrap();
        assert_eq!(again["id"], a["id"]);
        assert!(add_item(&c, "other", id, "TCS", "NSE").unwrap().is_none());
        assert!(reorder(&c, "u", id, &[a["id"].as_i64().unwrap(), 999]).unwrap());
        let ls = lists(&c, "u").unwrap();
        assert_eq!(ls[0]["items"][0]["symbol"], "INFY");
        assert_eq!(ls[0]["items"][1]["position"], 1);
        assert!(rename(&c, "u", id, "Second").unwrap());
        assert!(clear(&c, "u", id).unwrap());
        assert_eq!(lists(&c, "u").unwrap()[0]["items"], json!([]));
        assert!(delete(&c, "u", id).unwrap());
        assert!(!delete(&c, "u", id).unwrap());
        for i in 0..MAX_LISTS_PER_USER {
            assert!(create(&c, "u", &format!("L{i}"), &[]).unwrap().is_some());
        }
        assert!(create(&c, "u", "one more", &[]).unwrap().is_none());
    }
}
