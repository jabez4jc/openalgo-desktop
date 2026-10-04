//! Option-chain analytics: put-call ratio, max pain, gamma exposure, the IV
//! smile's ATM IV and skew, and gamma density with expected-move bands.

use super::black76::{self as b76, Flag};
use super::py_round;
use serde::Serialize;

/// Web `round(pe / ce, 2) if ce > 0 else 0`; `None` is the `0` branch.
pub fn pcr(pe: f64, ce: f64) -> Option<f64> {
    (ce > 0.0).then(|| py_round(pe / ce, 2))
}

/// One strike's open interest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrikeOi {
    pub strike: f64,
    pub ce_oi: f64,
    pub pe_oi: f64,
}

/// Writers' loss if the underlying settles at `strike`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PainRow {
    pub strike: f64,
    pub ce_pain: f64,
    pub pe_pain: f64,
    pub total_pain: f64,
    /// `total_pain` in crores.
    pub total_pain_cr: f64,
}

/// Max pain (web `oi_tracker_service.calculate_max_pain`): for every
/// candidate strike, CE writers lose `(candidate - k) * ce_oi` on strikes
/// below it and PE writers `(k - candidate) * pe_oi` on strikes above. The
/// strike with the least total (rounded, first on ties) is the max-pain
/// strike. `None` when no strike is positive.
pub fn max_pain(chain: &[StrikeOi]) -> Option<(f64, Vec<PainRow>)> {
    let valid: Vec<&StrikeOi> = chain
        .iter()
        .filter(|r| r.strike.is_finite() && r.strike > 0.0)
        .collect();
    if valid.is_empty() {
        return None;
    }
    let rows: Vec<PainRow> = valid
        .iter()
        .map(|c| {
            let (mut ce, mut pe) = (0.0, 0.0);
            for r in &valid {
                if c.strike > r.strike && r.ce_oi > 0.0 {
                    ce += (c.strike - r.strike) * r.ce_oi;
                }
                if c.strike < r.strike && r.pe_oi > 0.0 {
                    pe += (r.strike - c.strike) * r.pe_oi;
                }
            }
            let total = ce + pe;
            PainRow {
                strike: c.strike,
                ce_pain: py_round(ce, 2),
                pe_pain: py_round(pe, 2),
                total_pain: py_round(total, 2),
                total_pain_cr: py_round(total / 10_000_000.0, 2),
            }
        })
        .collect();
    let mut best = &rows[0];
    for r in &rows[1..] {
        if r.total_pain < best.total_pain {
            best = r;
        }
    }
    Some((best.strike, rows))
}

/// One strike of the GEX chain, rounded like the web.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct GexRow {
    pub strike: f64,
    pub ce_oi: f64,
    pub pe_oi: f64,
    pub ce_gamma: f64,
    pub pe_gamma: f64,
    pub ce_gex: f64,
    pub pe_gex: f64,
    pub net_gex: f64,
}

/// GEX = gamma * OI * lot size per side; net = call - put (web
/// `gex_service.get_gex_data`). Gammas arrive as the Greeks service
/// returns them (6 decimals).
pub fn gex_row(
    strike: f64,
    (ce_oi, ce_gamma, ce_lot): (f64, f64, f64),
    (pe_oi, pe_gamma, pe_lot): (f64, f64, f64),
) -> GexRow {
    let ce_gex = ce_gamma * ce_oi * ce_lot;
    let pe_gex = pe_gamma * pe_oi * pe_lot;
    GexRow {
        strike,
        ce_oi,
        pe_oi,
        ce_gamma: py_round(ce_gamma, 6),
        pe_gamma: py_round(pe_gamma, 6),
        ce_gex: py_round(ce_gex, 2),
        pe_gex: py_round(pe_gex, 2),
        net_gex: py_round(ce_gex - pe_gex, 2),
    }
}

/// Chain totals: (ce OI, pe OI, ce GEX, pe GEX, net GEX), GEX sums of the
/// rounded rows, rounded again.
pub fn gex_totals(rows: &[GexRow]) -> (f64, f64, f64, f64, f64) {
    let sum = |f: fn(&GexRow) -> f64| rows.iter().map(f).sum::<f64>();
    (
        sum(|r| r.ce_oi),
        sum(|r| r.pe_oi),
        py_round(sum(|r| r.ce_gex), 2),
        py_round(sum(|r| r.pe_gex), 2),
        py_round(sum(|r| r.net_gex), 2),
    )
}

/// ATM IV: the mean of call and put IV at the ATM strike, or whichever
/// exists (web `iv_smile_service`).
pub fn atm_iv(ce: Option<f64>, pe: Option<f64>) -> Option<f64> {
    match (ce, pe) {
        (Some(c), Some(p)) => Some(py_round((c + p) / 2.0, 2)),
        (Some(c), None) => Some(c),
        (None, p) => p,
    }
}

/// 25-delta skew proxy: put IV of the strike below ATM nearest to
/// ATM - 5%, minus call IV of the strike above ATM nearest to ATM + 5%.
/// Rows are `(strike, ce_iv, pe_iv)` in chain order; ties keep chain order
/// (Python's stable sort).
/// `(strike, ce_iv, pe_iv)`.
pub type SmileRow = (f64, Option<f64>, Option<f64>);

pub fn skew(rows: &[SmileRow], atm: f64) -> Option<f64> {
    if atm == 0.0 || rows.is_empty() {
        return None;
    }
    let d = atm * 0.05;
    let nearest = |target: f64, pick: &dyn Fn(&SmileRow) -> Option<f64>| {
        let mut order: Vec<&SmileRow> = rows.iter().collect();
        order.sort_by(|a, b| (a.0 - target).abs().total_cmp(&(b.0 - target).abs()));
        order.into_iter().find_map(pick)
    };
    let put = nearest(atm - d, &|r| if r.0 < atm { r.2 } else { None })?;
    let call = nearest(atm + d, &|r| if r.0 > atm { r.1 } else { None })?;
    Some(py_round(put - call, 2))
}

/// Intraday gamma horizon: one calendar day in years.
pub const INTRADAY_T_YEARS: f64 = 1.0 / 365.0;
/// IV used when no strike's IV can be inverted.
pub const FALLBACK_IV: f64 = 0.15;

/// One strike's chain inputs for gamma density.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DensityInput {
    pub strike: f64,
    pub ce_oi: f64,
    pub pe_oi: f64,
    pub ce_ltp: f64,
    pub pe_ltp: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DensityRow {
    pub strike: f64,
    pub ce_oi: f64,
    pub pe_oi: f64,
    /// Strike IV in percent (2 dp), `None` when neither side inverts.
    pub iv: Option<f64>,
    pub density_intraday: f64,
    pub density_expiry: f64,
}

/// A 1 and 2 sigma expected-move band around spot.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Band {
    pub sigma_move: f64,
    pub one_sigma_low: f64,
    pub one_sigma_high: f64,
    pub two_sigma_low: f64,
    pub two_sigma_high: f64,
}

impl Band {
    pub fn around(spot: f64, sigma: f64) -> Self {
        Band {
            sigma_move: py_round(sigma, 2),
            one_sigma_low: py_round(spot - sigma, 2),
            one_sigma_high: py_round(spot + sigma, 2),
            two_sigma_low: py_round(spot - 2.0 * sigma, 2),
            two_sigma_high: py_round(spot + 2.0 * sigma, 2),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GammaDensity {
    pub rows: Vec<DensityRow>,
    /// Decimal ATM IV actually used.
    pub atm_iv: f64,
    /// True when no strike inverted and [`FALLBACK_IV`] was used.
    pub fallback_iv: bool,
    pub peak_intraday_strike: Option<f64>,
    pub peak_expiry_strike: Option<f64>,
    pub intraday_band: Band,
    pub expiry_band: Band,
}

/// Web `_safe_iv`: a finite IV in `(0, 5]`, else `None`.
pub fn safe_iv(price: f64, f: f64, k: f64, r: f64, t: f64, flag: Flag) -> Option<f64> {
    if price <= 0.0 || f <= 0.0 || k <= 0.0 || t <= 0.0 {
        return None;
    }
    b76::implied_volatility(price, f, k, r, t, flag)
        .ok()
        .filter(|iv| iv.is_finite() && *iv > 0.0 && *iv <= 5.0)
}

/// Web `_safe_gamma`: a finite, non-negative gamma, else 0.
pub fn safe_gamma(f: f64, k: f64, t: f64, r: f64, sigma: f64) -> f64 {
    if sigma <= 0.0 || f <= 0.0 || k <= 0.0 || t <= 0.0 {
        return 0.0;
    }
    let g = b76::gamma(f, k, t, r, sigma);
    if g.is_finite() && g >= 0.0 {
        g
    } else {
        0.0
    }
}

/// Gamma density (web `gamma_density_service.calculate_gamma_density`):
/// per strike, gamma x OI summed over both legs at the intraday and the
/// to-expiry horizon, plus expected-move bands from the ATM IV.
///
/// `forward` prices the options; `spot` centres the bands. `t_years` is 0
/// once the contract has expired.
pub fn gamma_density(
    chain: &[DensityInput],
    spot: f64,
    forward: f64,
    atm_strike: Option<f64>,
    t_years: f64,
    r: f64,
) -> GammaDensity {
    let t_intraday = if t_years > 0.0 {
        t_years.min(INTRADAY_T_YEARS)
    } else {
        INTRADAY_T_YEARS
    };
    struct Leg {
        input: DensityInput,
        ce_iv: Option<f64>,
        pe_iv: Option<f64>,
        strike_iv: Option<f64>,
    }
    let mut legs = Vec::new();
    let mut valid = Vec::new();
    let mut atm_iv = None;
    for c in chain {
        if !c.strike.is_finite() || c.strike <= 0.0 {
            continue;
        }
        let ce_iv = safe_iv(c.ce_ltp, forward, c.strike, r, t_years, Flag::Call);
        let pe_iv = safe_iv(c.pe_ltp, forward, c.strike, r, t_years, Flag::Put);
        let sides: Vec<f64> = [ce_iv, pe_iv].into_iter().flatten().collect();
        let strike_iv = (!sides.is_empty()).then(|| sides.iter().sum::<f64>() / sides.len() as f64);
        if let Some(v) = strike_iv {
            valid.push(v);
            if atm_strike == Some(c.strike) {
                atm_iv = Some(v);
            }
        }
        legs.push(Leg {
            input: *c,
            ce_iv,
            pe_iv,
            strike_iv,
        });
    }
    let mut fallback_iv = false;
    let atm_iv = match atm_iv {
        Some(v) => v,
        None if !valid.is_empty() => {
            valid.sort_by(|a, b| a.total_cmp(b));
            valid[valid.len() / 2]
        }
        None => {
            fallback_iv = true;
            FALLBACK_IV
        }
    };
    let (mut max_i, mut max_e) = (0.0, 0.0);
    let (mut peak_i, mut peak_e) = (None, None);
    let rows = legs
        .iter()
        .map(|l| {
            let k = l.input.strike;
            let ce_sigma = l.ce_iv.unwrap_or(atm_iv);
            let pe_sigma = l.pe_iv.unwrap_or(atm_iv);
            // Black-76 gamma is the same for a call and a put.
            let e = safe_gamma(forward, k, t_years, r, ce_sigma) * l.input.ce_oi
                + safe_gamma(forward, k, t_years, r, pe_sigma) * l.input.pe_oi;
            let i = safe_gamma(forward, k, t_intraday, r, ce_sigma) * l.input.ce_oi
                + safe_gamma(forward, k, t_intraday, r, pe_sigma) * l.input.pe_oi;
            if e > max_e {
                max_e = e;
                peak_e = Some(k);
            }
            if i > max_i {
                max_i = i;
                peak_i = Some(k);
            }
            DensityRow {
                strike: k,
                ce_oi: l.input.ce_oi,
                pe_oi: l.input.pe_oi,
                iv: l.strike_iv.map(|v| py_round(v * 100.0, 2)),
                density_intraday: i,
                density_expiry: e,
            }
        })
        .collect();
    let sigma_i = spot * atm_iv * INTRADAY_T_YEARS.sqrt();
    let sigma_e = spot * atm_iv * t_years.max(1e-9).sqrt();
    GammaDensity {
        rows,
        atm_iv,
        fallback_iv,
        peak_intraday_strike: peak_i,
        peak_expiry_strike: peak_e,
        intraday_band: Band::around(spot, sigma_i),
        expiry_band: Band::around(spot, sigma_e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oi(strike: f64, ce_oi: f64, pe_oi: f64) -> StrikeOi {
        StrikeOi {
            strike,
            ce_oi,
            pe_oi,
        }
    }

    /// Worked by hand from web `services/oi_tracker_service.py:288-318`.
    /// Strikes 100/110/120, CE OI 10/20/30, PE OI 30/20/10.
    ///   at 100: ce 0;                pe (110-100)*20 + (120-100)*10 = 400
    ///   at 110: ce (110-100)*10=100; pe (120-110)*10 = 100      -> 200
    ///   at 120: ce 20*10 + 10*20=400; pe 0                      -> 400
    #[test]
    fn max_pain_matches_the_hand_computed_web_case() {
        let (k, rows) = max_pain(&[
            oi(100.0, 10.0, 30.0),
            oi(110.0, 20.0, 20.0),
            oi(120.0, 30.0, 10.0),
        ])
        .unwrap();
        assert_eq!(k, 110.0);
        let totals: Vec<f64> = rows.iter().map(|r| r.total_pain).collect();
        assert_eq!(totals, vec![400.0, 200.0, 400.0]);
        assert_eq!(rows[1].ce_pain, 100.0);
        assert_eq!(rows[1].pe_pain, 100.0);
        assert_eq!(rows[0].total_pain_cr, 0.0);
    }

    #[test]
    fn max_pain_ties_go_to_the_first_strike_and_bad_strikes_are_dropped() {
        let (k, rows) =
            max_pain(&[oi(0.0, 5.0, 5.0), oi(100.0, 0.0, 0.0), oi(110.0, 0.0, 0.0)]).unwrap();
        assert_eq!(k, 100.0);
        assert_eq!(rows.len(), 2);
        assert!(max_pain(&[oi(0.0, 1.0, 1.0)]).is_none());
        // Crores: 2.5e7 of pain is 2.5 Cr.
        let (_, rows) = max_pain(&[oi(100.0, 250_000.0, 0.0), oi(200.0, 0.0, 0.0)]).unwrap();
        assert_eq!(rows[1].total_pain_cr, 2.5);
    }

    #[test]
    fn pcr_rounds_and_guards_zero() {
        assert_eq!(pcr(150.0, 100.0), Some(1.5));
        assert_eq!(pcr(2.0, 3.0), Some(0.67));
        assert_eq!(pcr(5.0, 0.0), None);
    }

    /// Web `services/gex_service.py:98-127`: gex = gamma * oi * lot.
    #[test]
    fn gex_rows_and_totals() {
        let a = gex_row(100.0, (1000.0, 0.001089, 75.0), (500.0, 0.001132, 75.0));
        assert_eq!(a.ce_gex, 81.67); // 81.675 is stored just below, as in Python
        assert_eq!(a.pe_gex, 42.45); // 0.001132 * 500 * 75 = 42.45
        assert_eq!(a.net_gex, 39.23); // 81.675 - 42.45 = 39.225
        let b = gex_row(110.0, (0.0, 0.0, 75.0), (200.0, 0.0005, 75.0));
        assert_eq!(b.pe_gex, 7.5);
        let t = gex_totals(&[a, b]);
        assert_eq!(t, (1000.0, 700.0, 81.67, 49.95, 31.73));
    }

    /// Web `services/iv_smile_service.py:126-162`.
    #[test]
    fn smile_atm_iv_and_skew() {
        assert_eq!(atm_iv(Some(15.0), Some(16.01)), Some(15.51));
        assert_eq!(atm_iv(None, Some(16.0)), Some(16.0));
        assert_eq!(atm_iv(None, None), None);
        // ATM 1000: put target 950, call target 1050.
        let rows = [
            (940.0, None, Some(20.0)),
            (950.0, Some(1.0), None), // no put IV: skipped for the put side
            (960.0, None, Some(18.0)),
            (1000.0, Some(15.0), Some(15.0)),
            (1050.0, Some(12.5), None),
            (1060.0, Some(12.0), None),
        ];
        // Nearest to 950 with a put IV below ATM: 940 and 960 tie at 10,
        // chain order keeps 940 first.
        assert_eq!(skew(&rows, 1000.0), Some(7.5));
        assert_eq!(skew(&rows, 0.0), None);
        assert_eq!(skew(&rows[3..], 1000.0), None);
    }

    #[test]
    fn gamma_density_peaks_at_the_money_and_bands_scale_with_time() {
        let f = 22_400.0;
        let t = 3.0 / 365.0;
        let mk = |k: f64| {
            let sigma = 0.18;
            DensityInput {
                strike: k,
                ce_oi: 1000.0,
                pe_oi: 1000.0,
                ce_ltp: b76::price(Flag::Call, f, k, t, 0.0, sigma),
                pe_ltp: b76::price(Flag::Put, f, k, t, 0.0, sigma),
            }
        };
        let chain: Vec<DensityInput> = (0..9).map(|i| mk(22_000.0 + 100.0 * i as f64)).collect();
        let g = gamma_density(&chain, f, f, Some(22_400.0), t, 0.0);
        assert!(!g.fallback_iv);
        assert!((g.atm_iv - 0.18).abs() < 1e-6);
        assert_eq!(g.peak_expiry_strike, Some(22_400.0));
        assert_eq!(g.peak_intraday_strike, Some(22_400.0));
        assert_eq!(g.rows[4].iv, Some(18.0));
        // 1-day sigma: 22400 * 0.18 * sqrt(1/365) = 211.0449..
        assert_eq!(g.intraday_band.sigma_move, 211.04);
        assert_eq!(g.intraday_band.one_sigma_low, 22_188.96);
        // to expiry (3 days) is sqrt(3) wider.
        assert_eq!(g.expiry_band.sigma_move, 365.54);
        // Intraday horizon is shorter, so ATM gamma is sharper.
        assert!(g.rows[4].density_intraday > g.rows[4].density_expiry);
    }

    #[test]
    fn gamma_density_falls_back_when_nothing_inverts() {
        let chain = [DensityInput {
            strike: 100.0,
            ce_oi: 10.0,
            pe_oi: 10.0,
            ce_ltp: 0.0,
            pe_ltp: 0.0,
        }];
        let g = gamma_density(&chain, 100.0, 100.0, Some(100.0), 0.0, 0.0);
        assert!(g.fallback_iv);
        assert_eq!(g.atm_iv, FALLBACK_IV);
        // Expired: the to-expiry gamma is zero, the intraday one is not.
        assert_eq!(g.rows[0].density_expiry, 0.0);
        assert!(g.rows[0].density_intraday > 0.0);
        assert_eq!(g.peak_expiry_strike, None);
        assert_eq!(g.rows[0].iv, None);
    }
}
