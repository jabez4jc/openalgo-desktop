//! Sandbox capital and margin (web `sandbox/fund_manager.py`).
//!
//! One funds row per user, created lazily at the configured starting capital
//! (1 crore by default). Every balance change is a compare-and-set against a
//! fresh read of all seven money columns, retried up to five times, exactly as
//! the web does; inside the engine's single-writer transactions it always
//! lands on the first attempt.
//!
//! Invariant kept by the engine (and checked by [`reconcile`]):
//! `used_margin == Σ open position margin + Σ pending order margin +
//! Σ active GTT margin`.

use super::config::SandboxConfig;
use super::db::dec_col;
use super::types::{dec_to_db, is_future, is_option, rupees, Action, Product, MARGIN_DP};
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::{Decimal, RoundingStrategy};

/// Attempts before a funds write gives up (web `_FUNDS_WRITE_ATTEMPTS`).
pub const FUNDS_WRITE_ATTEMPTS: usize = 5;

/// What a trader sees when every attempt lost (web `FUNDS_BUSY_MESSAGE`).
pub const FUNDS_BUSY_MESSAGE: &str =
    "Your sandbox funds were being updated by another order at the same moment. Please try again.";

/// One `sandbox_funds` row.
#[derive(Debug, Clone, PartialEq)]
pub struct FundsRow {
    pub user_id: String,
    pub total_capital: Decimal,
    pub available_balance: Decimal,
    pub used_margin: Decimal,
    pub realized_pnl: Decimal,
    pub today_realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_pnl: Decimal,
    pub last_reset_date: String,
    pub reset_count: i64,
    pub updated_at: String,
}

impl FundsRow {
    fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            user_id: r.get("user_id")?,
            total_capital: dec_col(r, "total_capital")?,
            available_balance: dec_col(r, "available_balance")?,
            used_margin: dec_col(r, "used_margin")?,
            realized_pnl: dec_col(r, "realized_pnl")?,
            today_realized_pnl: dec_col(r, "today_realized_pnl")?,
            unrealized_pnl: dec_col(r, "unrealized_pnl")?,
            total_pnl: dec_col(r, "total_pnl")?,
            last_reset_date: r.get("last_reset_date")?,
            reset_count: r.get("reset_count")?,
            updated_at: r.get("updated_at")?,
        })
    }

    fn money(&self) -> [Decimal; 7] {
        [
            self.total_capital,
            self.available_balance,
            self.used_margin,
            self.realized_pnl,
            self.today_realized_pnl,
            self.unrealized_pnl,
            self.total_pnl,
        ]
    }
}

/// A refused funds change, with the trader-facing reason.
#[derive(Debug, Clone, PartialEq)]
pub struct FundsRefusal(pub String);

pub fn read(conn: &Connection, user_id: &str) -> rusqlite::Result<Option<FundsRow>> {
    conn.query_row(
        "SELECT * FROM sandbox_funds WHERE user_id = ?1",
        params![user_id],
        FundsRow::from_row,
    )
    .optional()
}

/// The user's funds row, created at `capital` when missing.
pub fn ensure(
    conn: &Connection,
    user_id: &str,
    capital: Decimal,
    now: &str,
) -> rusqlite::Result<FundsRow> {
    if let Some(f) = read(conn, user_id)? {
        return Ok(f);
    }
    let c = dec_to_db(capital);
    conn.execute(
        "INSERT INTO sandbox_funds (user_id, total_capital, available_balance, used_margin,
           realized_pnl, today_realized_pnl, unrealized_pnl, total_pnl, last_reset_date,
           reset_count, created_at, updated_at)
         VALUES (?1, ?2, ?2, '0', '0', '0', '0', '0', ?3, 0, ?3, ?3)",
        params![user_id, c, now],
    )?;
    Ok(read(conn, user_id)?.unwrap_or(FundsRow {
        user_id: user_id.to_string(),
        total_capital: capital,
        available_balance: capital,
        used_margin: Decimal::ZERO,
        realized_pnl: Decimal::ZERO,
        today_realized_pnl: Decimal::ZERO,
        unrealized_pnl: Decimal::ZERO,
        total_pnl: Decimal::ZERO,
        last_reset_date: now.to_string(),
        reset_count: 0,
        updated_at: now.to_string(),
    }))
}

/// Write `new` only if the row still holds what `old` read.
fn write_if_unchanged(
    conn: &Connection,
    old: &FundsRow,
    new: &FundsRow,
    now: &str,
) -> rusqlite::Result<bool> {
    let o = old.money().map(dec_to_db);
    let n = new.money().map(dec_to_db);
    let changed = conn.execute(
        "UPDATE sandbox_funds SET total_capital = ?1, available_balance = ?2, used_margin = ?3,
           realized_pnl = ?4, today_realized_pnl = ?5, unrealized_pnl = ?6, total_pnl = ?7,
           updated_at = ?8
         WHERE user_id = ?9 AND total_capital = ?10 AND available_balance = ?11
           AND used_margin = ?12 AND realized_pnl = ?13 AND today_realized_pnl = ?14
           AND unrealized_pnl = ?15 AND total_pnl = ?16",
        params![
            n[0],
            n[1],
            n[2],
            n[3],
            n[4],
            n[5],
            n[6],
            now,
            old.user_id,
            o[0],
            o[1],
            o[2],
            o[3],
            o[4],
            o[5],
            o[6]
        ],
    )?;
    Ok(changed == 1)
}

/// Read, compute, compare-and-set; retried from a fresh read when another
/// writer moved the row in between.
pub fn apply(
    conn: &Connection,
    user_id: &str,
    capital: Decimal,
    now: &str,
    compute: impl Fn(&FundsRow) -> Result<FundsRow, FundsRefusal>,
) -> rusqlite::Result<Result<FundsRow, FundsRefusal>> {
    for _ in 0..FUNDS_WRITE_ATTEMPTS {
        let old = ensure(conn, user_id, capital, now)?;
        let new = match compute(&old) {
            Ok(n) => n,
            Err(r) => return Ok(Err(r)),
        };
        if new.money() == old.money() {
            return Ok(Ok(new));
        }
        if write_if_unchanged(conn, &old, &new, now)? {
            return Ok(Ok(new));
        }
    }
    tracing::warn!("Sandbox funds kept changing under every write attempt; nothing written");
    Ok(Err(FundsRefusal(FUNDS_BUSY_MESSAGE.to_string())))
}

/// The pre-check message the web shows before blocking
/// (`check_margin_available`).
pub fn check_available(funds: &FundsRow, required: Decimal) -> Result<(), FundsRefusal> {
    if funds.available_balance >= required {
        Ok(())
    } else {
        Err(FundsRefusal(format!(
            "Insufficient funds. Required: \u{20b9}{}, Available: \u{20b9}{}, Shortage: \u{20b9}{}",
            rupees(required),
            rupees(funds.available_balance),
            rupees(required - funds.available_balance)
        )))
    }
}

/// Block margin for an order: `available -= amount; used += amount`.
pub fn block(f: &FundsRow, amount: Decimal) -> Result<FundsRow, FundsRefusal> {
    if amount <= Decimal::ZERO {
        return Err(FundsRefusal(format!(
            "Block amount must be positive, got {}",
            rupees(amount)
        )));
    }
    if f.available_balance < amount {
        return Err(FundsRefusal(format!(
            "Insufficient funds. Required: \u{20b9}{}, Available: \u{20b9}{}",
            rupees(amount),
            rupees(f.available_balance)
        )));
    }
    let mut n = f.clone();
    n.available_balance -= amount;
    n.used_margin += amount;
    Ok(n)
}

/// Release margin and book P&L (web `_released`): `available += amount +
/// pnl; used -= amount; realized += pnl; total = realized + unrealized;
/// today += pnl` when `count_today`. Refuses a negative amount or more than
/// is reserved.
pub fn release(
    f: &FundsRow,
    amount: Decimal,
    realized_pnl: Decimal,
    count_today: bool,
) -> Result<FundsRow, FundsRefusal> {
    if amount < Decimal::ZERO {
        return Err(FundsRefusal(format!(
            "Release amount cannot be negative, got {}",
            rupees(amount)
        )));
    }
    if amount > f.used_margin {
        return Err(FundsRefusal(format!(
            "Cannot release \u{20b9}{}: only \u{20b9}{} is reserved",
            rupees(amount),
            rupees(f.used_margin)
        )));
    }
    let mut n = f.clone();
    n.available_balance = f.available_balance + amount + realized_pnl;
    n.used_margin = f.used_margin - amount;
    n.realized_pnl = f.realized_pnl + realized_pnl;
    n.total_pnl = n.realized_pnl + f.unrealized_pnl;
    if count_today {
        n.today_realized_pnl = f.today_realized_pnl + realized_pnl;
    }
    Ok(n)
}

/// T+1: the margin a CNC buy held becomes the holding. `used -= margin`, and
/// the difference between the holding's cost and that margin is settled
/// against available cash (zero at the default 1x CNC leverage, where the
/// web's `transfer_margin_to_holdings` is identical).
pub fn transfer_to_holdings(
    f: &FundsRow,
    margin: Decimal,
    cost: Decimal,
) -> Result<FundsRow, FundsRefusal> {
    if margin < Decimal::ZERO {
        return Err(FundsRefusal(format!(
            "Transfer amount must be positive, got {}",
            rupees(margin)
        )));
    }
    if margin > f.used_margin {
        return Err(FundsRefusal(format!(
            "Cannot transfer \u{20b9}{}: only \u{20b9}{} is reserved",
            rupees(margin),
            rupees(f.used_margin)
        )));
    }
    let mut n = f.clone();
    n.used_margin -= margin;
    n.available_balance -= cost - margin;
    Ok(n)
}

/// T+1: proceeds of a CNC sale credited to available cash.
pub fn credit(f: &FundsRow, amount: Decimal) -> Result<FundsRow, FundsRefusal> {
    if amount <= Decimal::ZERO {
        return Err(FundsRefusal(format!(
            "Credit amount must be positive, got {}",
            rupees(amount)
        )));
    }
    let mut n = f.clone();
    n.available_balance += amount;
    Ok(n)
}

/// Catch-up settlement of a position left over from a closed session: the
/// P&L goes to all-time realized only, and used margin is floored at zero.
pub fn prior_session_release(f: &FundsRow, amount: Decimal, pnl: Decimal) -> FundsRow {
    let mut n = f.clone();
    n.available_balance = f.available_balance + amount + pnl;
    n.used_margin = (f.used_margin - amount).max(Decimal::ZERO);
    n.realized_pnl = f.realized_pnl + pnl;
    n.total_pnl = n.realized_pnl + f.unrealized_pnl;
    n
}

/// GTT reservation change: positive blocks (needs available), negative
/// releases (never more than reserved).
pub fn margin_delta(f: &FundsRow, delta: Decimal) -> Result<FundsRow, FundsRefusal> {
    if delta == Decimal::ZERO {
        return Ok(f.clone());
    }
    if delta > Decimal::ZERO && f.available_balance < delta {
        return Err(FundsRefusal(format!(
            "Insufficient funds. Required: \u{20b9}{}",
            rupees(delta)
        )));
    }
    if delta < Decimal::ZERO && f.used_margin < -delta {
        return Err(FundsRefusal(format!(
            "Cannot release \u{20b9}{}: more than the reserved margin",
            rupees(-delta)
        )));
    }
    let mut n = f.clone();
    n.available_balance -= delta;
    n.used_margin += delta;
    Ok(n)
}

/// MTM: `unrealized = x; total = realized + x`.
pub fn set_unrealized(f: &FundsRow, unrealized: Decimal) -> FundsRow {
    let mut n = f.clone();
    n.unrealized_pnl = unrealized;
    n.total_pnl = f.realized_pnl + unrealized;
    n
}

/// Starting capital changed: `total = new; available = new - used + total_pnl`.
pub fn rebased(f: &FundsRow, capital: Decimal) -> FundsRow {
    let mut n = f.clone();
    n.total_capital = capital;
    n.available_balance = capital - f.used_margin + f.total_pnl;
    n
}

/// Reset the row to `capital`, `reset_count += 1`.
pub fn reset_row(
    conn: &Connection,
    user_id: &str,
    capital: Decimal,
    now: &str,
) -> rusqlite::Result<()> {
    ensure(conn, user_id, capital, now)?;
    let c = dec_to_db(capital);
    conn.execute(
        "UPDATE sandbox_funds SET total_capital = ?1, available_balance = ?1, used_margin = '0',
           realized_pnl = '0', today_realized_pnl = '0', unrealized_pnl = '0', total_pnl = '0',
           last_reset_date = ?2, reset_count = reset_count + 1, updated_at = ?2
         WHERE user_id = ?3",
        params![c, now, user_id],
    )?;
    Ok(())
}

/// Leverage for an instrument (web `_get_leverage`).
pub fn leverage(
    cfg: &SandboxConfig,
    symbol: &str,
    exchange: &str,
    product: Product,
    action: Action,
) -> Decimal {
    if exchange == "NSE" || exchange == "BSE" {
        return match product {
            Product::Mis => cfg.equity_mis_leverage,
            Product::Cnc | Product::Nrml => cfg.equity_cnc_leverage,
        };
    }
    if is_future(symbol, exchange) {
        return cfg.futures_leverage;
    }
    if is_option(symbol, exchange) {
        return match action {
            Action::Buy => cfg.option_buy_leverage,
            Action::Sell => cfg.option_sell_leverage,
        };
    }
    Decimal::ONE
}

/// `|qty| * price / leverage`, to the paisa.
pub fn margin_required(
    cfg: &SandboxConfig,
    symbol: &str,
    exchange: &str,
    product: Product,
    quantity: i64,
    price: Decimal,
    action: Action,
) -> Decimal {
    let lev = leverage(cfg, symbol, exchange, product, action);
    let lev = if lev <= Decimal::ZERO {
        Decimal::ONE
    } else {
        lev
    };
    (Decimal::from(quantity.abs()) * price / lev)
        .round_dp_with_strategy(MARGIN_DP, RoundingStrategy::MidpointAwayFromZero)
}

/// Total margin the books say is held: open positions, pending orders and
/// active GTTs.
pub fn expected_used_margin(conn: &Connection, user_id: &str) -> rusqlite::Result<Decimal> {
    let mut total = Decimal::ZERO;
    for sql in [
        "SELECT margin_blocked FROM sandbox_positions WHERE user_id = ?1 AND quantity != 0",
        "SELECT margin_blocked FROM sandbox_orders WHERE user_id = ?1
           AND order_status IN ('open','trigger pending')",
        "SELECT margin_blocked FROM sandbox_gtt WHERE user_id = ?1 AND gtt_status = 'active'",
    ] {
        let mut stmt = conn.prepare_cached(sql)?;
        let rows = stmt.query_map(params![user_id], |r| r.get::<_, String>(0))?;
        for v in rows {
            total += super::types::dec_from_db(&v?);
        }
    }
    Ok(total)
}

/// Detect (and with `auto_fix`, correct) a gap between `used_margin` and the
/// margin the books hold. Returns the discrepancy (`used - expected`).
///
/// Unlike the web's `validate_margin_consistency`, margin held by pending
/// orders counts as held: the web's version released a resting order's
/// margin after any unrelated fill.
pub fn reconcile(
    conn: &Connection,
    user_id: &str,
    auto_fix: bool,
    now: &str,
) -> rusqlite::Result<Decimal> {
    let Some(funds) = read(conn, user_id)? else {
        return Ok(Decimal::ZERO);
    };
    let expected = expected_used_margin(conn, user_id)?;
    let discrepancy = funds.used_margin - expected;
    if discrepancy == Decimal::ZERO {
        return Ok(discrepancy);
    }
    tracing::warn!(
        "Sandbox margin discrepancy for {}: used {} vs held {} ({})",
        user_id,
        funds.used_margin,
        expected,
        discrepancy
    );
    if auto_fix {
        let mut n = funds.clone();
        n.used_margin = expected;
        n.available_balance = funds.available_balance + discrepancy;
        if !write_if_unchanged(conn, &funds, &n, now)? {
            tracing::warn!("Sandbox margin reconciliation skipped: funds changed while it ran");
        }
    }
    Ok(discrepancy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn row(avail: Decimal, used: Decimal) -> FundsRow {
        FundsRow {
            user_id: "u".into(),
            total_capital: dec!(10000000),
            available_balance: avail,
            used_margin: used,
            realized_pnl: Decimal::ZERO,
            today_realized_pnl: Decimal::ZERO,
            unrealized_pnl: Decimal::ZERO,
            total_pnl: Decimal::ZERO,
            last_reset_date: String::new(),
            reset_count: 0,
            updated_at: String::new(),
        }
    }

    // Port of test/sandbox/test_fund_manager.py::test_margin_operations
    #[test]
    fn test_margin_operations() {
        let f = row(dec!(10000000), dec!(0));
        let f = block(&f, dec!(50000)).unwrap();
        assert_eq!(f.available_balance, dec!(9950000));
        assert_eq!(f.used_margin, dec!(50000));
        let f = release(&f, dec!(50000), dec!(1000), true).unwrap();
        assert_eq!(f.available_balance, dec!(10001000));
        assert_eq!(f.used_margin, dec!(0));
        assert_eq!(f.realized_pnl, dec!(1000));
        assert_eq!(f.today_realized_pnl, dec!(1000));
        assert_eq!(f.total_pnl, dec!(1000));
    }

    // Port of test_fund_manager.py::test_insufficient_funds
    #[test]
    fn test_insufficient_funds() {
        let f = row(dec!(1000), dec!(0));
        assert!(block(&f, dec!(5000)).is_err());
        assert!(check_available(&f, dec!(5000)).is_err());
        assert!(check_available(&f, dec!(1000)).is_ok());
    }

    // Port of test_fund_manager.py::test_leverage_calculations (MIS 5x, CNC 1x)
    #[test]
    fn test_leverage_calculations() {
        let cfg = SandboxConfig::default();
        let mis = margin_required(
            &cfg,
            "RELIANCE",
            "NSE",
            Product::Mis,
            100,
            dec!(1200),
            Action::Buy,
        );
        assert_eq!(mis, dec!(24000));
        let cnc = margin_required(
            &cfg,
            "RELIANCE",
            "NSE",
            Product::Cnc,
            100,
            dec!(1200),
            Action::Buy,
        );
        assert_eq!(cnc, dec!(120000));
        let fut = margin_required(
            &cfg,
            "NIFTY27OCT26FUT",
            "NFO",
            Product::Nrml,
            65,
            dec!(22500),
            Action::Buy,
        );
        assert_eq!(fut, dec!(146250));
        let opt = margin_required(
            &cfg,
            "NIFTY06OCT2622400CE",
            "NFO",
            Product::Nrml,
            65,
            dec!(156.95),
            Action::Sell,
        );
        assert_eq!(opt, dec!(10201.75));
    }

    // Port of test_fund_manager.py::test_unrealized_pnl
    #[test]
    fn test_unrealized_pnl() {
        let mut f = row(dec!(10000000), dec!(0));
        f.realized_pnl = dec!(500);
        let f = set_unrealized(&f, dec!(-200));
        assert_eq!(f.unrealized_pnl, dec!(-200));
        assert_eq!(f.total_pnl, dec!(300));
    }

    // TestFundPrimitivesRejectBadAmounts (test_gtt_manager.py)
    #[test]
    fn test_fund_primitives_reject_bad_amounts() {
        let f = row(dec!(1000), dec!(100));
        assert!(block(&f, dec!(0)).is_err());
        assert!(block(&f, dec!(-1)).is_err());
        assert!(release(&f, dec!(-1), dec!(0), true).is_err());
        assert!(
            release(&f, dec!(100.01), dec!(0), true).is_err(),
            "over-release refused"
        );
        assert!(credit(&f, dec!(0)).is_err());
        assert!(transfer_to_holdings(&f, dec!(100.01), dec!(100.01)).is_err());
        assert!(margin_delta(&f, dec!(-100.01)).is_err());
        assert!(margin_delta(&f, dec!(1000.01)).is_err());
        assert_eq!(margin_delta(&f, dec!(-100)).unwrap().used_margin, dec!(0));
    }

    #[test]
    fn prior_session_release_floors_used_margin_and_skips_today() {
        let f = row(dec!(1000), dec!(50));
        let n = prior_session_release(&f, dec!(80), dec!(-10));
        assert_eq!(n.used_margin, dec!(0));
        assert_eq!(n.available_balance, dec!(1070));
        assert_eq!(n.today_realized_pnl, dec!(0));
        assert_eq!(n.realized_pnl, dec!(-10));
    }
}
