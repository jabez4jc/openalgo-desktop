//! Black-76 option pricing and Greeks (web `option_greeks_service.py`, which
//! uses `opengreeks.black76`, a clone of `py_vollib.black`). Shared by the
//! `/api/v1` option services and every options tool.
//!
//! Hand-written: the normal CDF is Hart's double-precision algorithm (West
//! 2005); the implied-volatility solver is a bracketed bisection on
//! `[1e-6, 5.0]` with the library's two pre-checks. Rates are decimals, time
//! in years, theta per calendar day, vega and rho per 1 percentage point.

use std::f64::consts::PI;

/// Standard normal CDF, about 1e-15 absolute accuracy.
pub fn norm_cdf(x: f64) -> f64 {
    let xabs = x.abs();
    let c = if xabs > 37.0 {
        0.0
    } else {
        let e = (-xabs * xabs / 2.0).exp();
        if xabs < 7.071_067_811_865_47 {
            let mut b = 3.526_249_659_989_11e-02 * xabs + 0.700_383_064_443_688;
            b = b * xabs + 6.373_962_203_531_65;
            b = b * xabs + 33.912_866_078_383;
            b = b * xabs + 112.079_291_497_871;
            b = b * xabs + 221.213_596_169_931;
            b = b * xabs + 220.206_867_912_376;
            let num = e * b;
            let mut d = 8.838_834_764_831_84e-02 * xabs + 1.755_667_163_182_64;
            d = d * xabs + 16.064_177_579_207;
            d = d * xabs + 86.780_732_202_946_1;
            d = d * xabs + 296.564_248_779_674;
            d = d * xabs + 637.333_633_378_831;
            d = d * xabs + 793.826_512_519_948;
            d = d * xabs + 440.413_735_824_752;
            num / d
        } else {
            let mut b = xabs + 0.65;
            b = xabs + 4.0 / b;
            b = xabs + 3.0 / b;
            b = xabs + 2.0 / b;
            b = xabs + 1.0 / b;
            e / b / 2.506_628_274_631
        }
    };
    if x > 0.0 {
        1.0 - c
    } else {
        c
    }
}

/// Standard normal PDF.
pub fn norm_pdf(x: f64) -> f64 {
    (-x * x / 2.0).exp() / (2.0 * PI).sqrt()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Call,
    Put,
}

impl Flag {
    pub fn from_type(t: &str) -> Self {
        if t.eq_ignore_ascii_case("PE") {
            Flag::Put
        } else {
            Flag::Call
        }
    }
}

fn d1d2(f: f64, k: f64, t: f64, sigma: f64) -> (f64, f64) {
    let st = sigma * t.sqrt();
    let d1 = ((f / k).ln() + sigma * sigma * t / 2.0) / st;
    (d1, d1 - st)
}

/// Black-76 price.
pub fn price(flag: Flag, f: f64, k: f64, t: f64, r: f64, sigma: f64) -> f64 {
    let (d1, d2) = d1d2(f, k, t, sigma);
    let df = (-r * t).exp();
    match flag {
        Flag::Call => df * (f * norm_cdf(d1) - k * norm_cdf(d2)),
        Flag::Put => df * (k * norm_cdf(-d2) - f * norm_cdf(-d1)),
    }
}

/// Greeks of one option.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Greeks {
    pub delta: f64,
    pub gamma: f64,
    pub theta: f64,
    pub vega: f64,
    pub rho: f64,
}

pub fn greeks(flag: Flag, f: f64, k: f64, t: f64, r: f64, sigma: f64) -> Greeks {
    let (d1, d2) = d1d2(f, k, t, sigma);
    let df = (-r * t).exp();
    let sqrt_t = t.sqrt();
    let delta = match flag {
        Flag::Call => df * norm_cdf(d1),
        Flag::Put => -df * norm_cdf(-d1),
    };
    let gamma = df * norm_pdf(d1) / (f * sigma * sqrt_t);
    let vega = f * df * norm_pdf(d1) * sqrt_t * 0.01;
    let decay = -f * df * norm_pdf(d1) * sigma / (2.0 * sqrt_t);
    let theta = match flag {
        Flag::Call => decay + r * f * df * norm_cdf(d1) - r * k * df * norm_cdf(d2),
        Flag::Put => decay - r * f * df * norm_cdf(-d1) + r * k * df * norm_cdf(-d2),
    } / 365.0;
    let rho = -t * price(flag, f, k, t, r, sigma) * 0.01;
    Greeks {
        delta,
        gamma,
        theta,
        vega,
        rho,
    }
}

/// Black-76 gamma alone (web `black76.gamma`).
pub fn gamma(f: f64, k: f64, t: f64, r: f64, sigma: f64) -> f64 {
    let (d1, _) = d1d2(f, k, t, sigma);
    (-r * t).exp() * norm_pdf(d1) / (f * sigma * t.sqrt())
}

pub const IV_LOW: f64 = 1e-6;
pub const IV_HIGH: f64 = 5.0;

/// Implied volatility, or the library's error text.
pub fn implied_volatility(
    option_price: f64,
    f: f64,
    k: f64,
    r: f64,
    t: f64,
    flag: Flag,
) -> Result<f64, &'static str> {
    let df = (-r * t).exp();
    let intrinsic = match flag {
        Flag::Call => df * (f - k).max(0.0),
        Flag::Put => df * (k - f).max(0.0),
    };
    if option_price < intrinsic {
        return Err("price is below intrinsic value");
    }
    let max = match flag {
        Flag::Call => f * df,
        Flag::Put => k * df,
    };
    if option_price > max {
        return Err("price exceeds theoretical maximum");
    }
    if option_price <= 0.0 {
        return Ok(IV_LOW);
    }
    let p_hi = price(flag, f, k, t, r, IV_HIGH);
    if option_price >= p_hi {
        return Ok(IV_HIGH - 4e-15);
    }
    if option_price <= price(flag, f, k, t, r, IV_LOW) {
        return Ok(IV_LOW);
    }
    let (mut lo, mut hi) = (IV_LOW, IV_HIGH);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if mid <= lo || mid >= hi {
            break;
        }
        if price(flag, f, k, t, r, mid) < option_price {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let sigma = 0.5 * (lo + hi);
    if sigma.is_finite() {
        Ok(sigma)
    } else {
        Err("IV solver failed to converge")
    }
}

/// Python `round(x, n)` (correctly rounded on the binary value).
pub fn py_round(x: f64, n: usize) -> f64 {
    format!("{:.*}", n, x).parse::<f64>().unwrap_or(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T3: f64 = 3.0 / 365.0;

    fn rounded(
        flag: Flag,
        f: f64,
        k: f64,
        px: f64,
        t: f64,
        r: f64,
    ) -> (f64, f64, f64, f64, f64, f64) {
        let iv = implied_volatility(px, f, k, r, t, flag).unwrap();
        let g = greeks(flag, f, k, t, r, iv);
        (
            py_round(iv * 100.0, 2),
            py_round(g.delta, 4),
            py_round(g.gamma, 6),
            py_round(g.theta, 4),
            py_round(g.vega, 4),
            py_round(g.rho, 6),
        )
    }

    /// Reference values computed with the web's `opengreeks.black76` (the
    /// library behind `services/option_greeks_service.py`), rounded as the
    /// service rounds them.
    #[test]
    fn matches_the_web_formula() {
        assert_eq!(
            rounded(Flag::Call, 22421.95, 22400.0, 156.95, T3, 0.0),
            (17.98, 0.5272, 0.001089, -24.241, 8.0907, -0.0129)
        );
        assert_eq!(
            rounded(Flag::Put, 22421.95, 22500.0, 180.0, T3, 0.0),
            (16.92, -0.5866, 0.001132, -22.3299, 7.9177, -0.014795)
        );
        assert_eq!(
            rounded(Flag::Call, 22421.95, 21000.0, 1450.0, T3, 0.0),
            (48.29, 0.9356, 0.000128, -20.6048, 2.5603, -0.119178)
        );
        assert_eq!(
            rounded(Flag::Call, 5850.0, 5900.0, 95.5, 12.0 / 365.0, 0.0),
            (27.96, 0.4433, 0.001331, -4.8809, 4.1889, -0.031397)
        );
        assert_eq!(
            rounded(Flag::Call, 22421.95, 22400.0, 156.95, T3, 0.065),
            (17.99, 0.5269, 0.001088, -24.2141, 8.0864, -0.0129)
        );
    }

    #[test]
    fn raw_values_agree_to_high_precision() {
        let iv = implied_volatility(156.95, 22421.95, 22400.0, 0.0, T3, Flag::Call).unwrap();
        assert!((iv - 0.179_769_085_169_233_4).abs() < 1e-9);
        let g = greeks(Flag::Call, 22421.95, 22400.0, T3, 0.0, iv);
        assert!((g.delta - 0.527_204_606_065_814_8).abs() < 1e-9);
        assert!((g.theta - -24.240_974_139_031_493).abs() < 1e-7);
    }

    #[test]
    fn fixture_forward_case() {
        // optiongreeks/nifty_option_default: F 22453.35, t = 280528 s.
        let t = 280_528.0 / 86_400.0 / 365.0;
        let r = rounded(Flag::Call, 22453.35, 22400.0, 156.95, t, 0.0);
        assert_eq!(
            (r.0, r.1, r.2, r.4, r.5),
            (15.23, 0.5686, 0.001219, 8.3232, -0.013961)
        );
    }

    #[test]
    fn solver_edges() {
        assert_eq!(
            implied_volatility(1.0, 100.0, 50.0, 0.0, 0.1, Flag::Call),
            Err("price is below intrinsic value")
        );
        assert_eq!(
            implied_volatility(150.0, 100.0, 50.0, 0.0, 0.1, Flag::Call),
            Err("price exceeds theoretical maximum")
        );
        assert_eq!(
            implied_volatility(0.0, 100.0, 120.0, 0.0, 0.1, Flag::Call),
            Ok(IV_LOW)
        );
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-15);
        assert!((norm_cdf(1.96) - 0.975_002_104_851_780).abs() < 1e-12);
    }
}
