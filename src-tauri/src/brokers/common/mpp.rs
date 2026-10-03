//! Market Price Protection (web `utils/mpp_slab.py`).
//!
//! Where a broker only accepts LIMIT (Kite GTT), a MARKET request is sent as
//! a LIMIT at LTP plus (BUY) or minus (SELL) a slab percentage, rounded to
//! the instrument's tick size.

use super::mapping::Action;

/// Python `round()` (round half to even), so prices match the web exactly.
pub fn py_round(v: f64, ndigits: i32) -> f64 {
    let m = 10f64.powi(ndigits);
    let x = v * m;
    let r = x.round();
    let r = if (x - x.trunc()).abs() == 0.5 {
        // Tie: pick the even neighbour.
        let down = x.trunc();
        if (down as i64) % 2 == 0 {
            down
        } else {
            down + x.signum()
        }
    } else {
        r
    };
    r / m
}

/// `EQ`, `FUT`, `CE` or `PE` from a symbol's suffix.
pub fn instrument_type_from_symbol(symbol: &str) -> &'static str {
    let s = symbol.to_ascii_uppercase();
    if s.ends_with("CE") {
        "CE"
    } else if s.ends_with("PE") {
        "PE"
    } else if s.ends_with("FUT") {
        "FUT"
    } else {
        "EQ"
    }
}

/// Protection percentage for a price.
pub fn mpp_percentage(price: f64, instrument_type: &str) -> f64 {
    if matches!(instrument_type, "CE" | "PE") {
        if price < 10.0 {
            5.0
        } else if price < 100.0 {
            3.0
        } else if price < 500.0 {
            2.0
        } else {
            1.0
        }
    } else if price < 100.0 {
        2.0
    } else if price < 500.0 {
        1.0
    } else {
        0.5
    }
}

pub fn round_to_tick(price: f64, tick_size: Option<f64>) -> f64 {
    match tick_size {
        Some(t) if t > 0.0 => py_round(py_round(price / t, 0) * t, 2),
        _ => py_round(price, 2),
    }
}

/// LIMIT price that protects a MARKET order.
pub fn protected_price(
    price: f64,
    action: Action,
    instrument_type: &str,
    tick_size: Option<f64>,
) -> f64 {
    let m = mpp_percentage(price, instrument_type) / 100.0;
    let p = match action {
        Action::Buy => price * (1.0 + m),
        Action::Sell => price * (1.0 - m),
    };
    round_to_tick(p, tick_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_rounding() {
        assert_eq!(py_round(2.5, 0), 2.0);
        assert_eq!(py_round(3.5, 0), 4.0);
        assert_eq!(py_round(102.0111, 2), 102.01);
    }

    #[test]
    fn slabs_match_web_examples() {
        assert_eq!(mpp_percentage(50.0, "EQ"), 2.0);
        assert_eq!(mpp_percentage(50.0, "CE"), 3.0);
        assert_eq!(mpp_percentage(5.0, "PE"), 5.0);
        assert_eq!(mpp_percentage(600.0, "FUT"), 0.5);
        assert_eq!(protected_price(100.0, Action::Buy, "EQ", None), 101.0);
        assert_eq!(protected_price(5.0, Action::Buy, "CE", Some(0.05)), 5.25);
        assert_eq!(
            protected_price(1000.0, Action::Sell, "EQ", Some(0.05)),
            995.0
        );
        assert_eq!(round_to_tick(102.0111, Some(0.05)), 102.0);
        assert_eq!(instrument_type_from_symbol("NIFTY24DECFUT"), "FUT");
        assert_eq!(instrument_type_from_symbol("SBIN"), "EQ");
        // Same suffix rule as the web: an equity ending in CE reads as CE.
        assert_eq!(instrument_type_from_symbol("RELIANCE"), "CE");
    }
}
