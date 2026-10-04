//! Dynamic ATM straddle (web `straddle_chart_service.py`) and the intraday
//! short-straddle simulation with N-point adjustments (web
//! `custom_straddle_service.py`).

use super::py_round;
use super::series::{ist_date, Bar};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// Close-by-timestamp of one strike's call and put.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StrikeLegs {
    pub ce: BTreeMap<i64, f64>,
    pub pe: BTreeMap<i64, f64>,
}

/// Option history per strike.
#[derive(Debug, Clone, Default)]
pub struct StrikeMap(HashMap<u64, StrikeLegs>);

impl StrikeMap {
    pub fn insert(&mut self, strike: f64, legs: StrikeLegs) {
        self.0.insert(strike.to_bits(), legs);
    }

    pub fn get(&self, strike: f64) -> Option<&StrikeLegs> {
        self.0.get(&strike.to_bits())
    }

    fn ce(&self, strike: f64, t: i64) -> Option<f64> {
        self.get(strike).and_then(|l| l.ce.get(&t).copied())
    }

    fn pe(&self, strike: f64, t: i64) -> Option<f64> {
        self.get(strike).and_then(|l| l.pe.get(&t).copied())
    }
}

/// The ATM strike of every bar (closest available strike to its close).
pub fn atm_per_bar(underlying: &[Bar], strikes: &[f64]) -> Vec<Option<f64>> {
    underlying
        .iter()
        .map(|b| super::closest_strike(strikes, b.close))
        .collect()
}

/// The distinct ATM strikes, ascending.
pub fn unique_strikes(atms: &[Option<f64>]) -> Vec<f64> {
    let mut v: Vec<f64> = atms.iter().flatten().copied().collect();
    v.sort_by(|a, b| a.total_cmp(b));
    v.dedup();
    v
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct StraddlePoint {
    pub time: i64,
    pub spot: f64,
    pub atm_strike: f64,
    pub ce_price: f64,
    pub pe_price: f64,
    pub straddle: f64,
    pub synthetic_future: f64,
}

/// Web `get_straddle_chart_data` merge: at every underlying bar, the ATM
/// call and put closes of that bar's ATM strike; straddle = CE + PE,
/// synthetic future = strike + CE - PE. Bars missing either leg are
/// dropped.
pub fn straddle_series(
    underlying: &[Bar],
    atms: &[Option<f64>],
    data: &StrikeMap,
) -> Vec<StraddlePoint> {
    underlying
        .iter()
        .zip(atms)
        .filter_map(|(b, atm)| {
            let k = (*atm)?;
            let ce = data.ce(k, b.time)?;
            let pe = data.pe(k, b.time)?;
            Some(StraddlePoint {
                time: b.time,
                spot: py_round(b.close, 2),
                atm_strike: k,
                ce_price: py_round(ce, 2),
                pe_price: py_round(pe, 2),
                straddle: py_round(ce + pe, 2),
                synthetic_future: py_round(k + ce - pe, 2),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SimPoint {
    pub time: i64,
    pub pnl: f64,
    pub spot: f64,
    pub atm_strike: f64,
    pub entry_strike: f64,
    pub ce_price: f64,
    pub pe_price: f64,
    pub straddle: f64,
    pub synthetic_future: f64,
    pub adjustments: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Trade {
    pub time: i64,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_strike: Option<f64>,
    pub strike: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_ce: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_pe: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_straddle: Option<f64>,
    pub ce_price: f64,
    pub pe_price: f64,
    pub straddle: f64,
    pub spot: f64,
    pub leg_pnl: f64,
    pub cumulative_pnl: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Summary {
    pub total_pnl: f64,
    pub total_adjustments: i64,
    pub max_pnl: f64,
    pub min_pnl: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Simulation {
    pub pnl_series: Vec<SimPoint>,
    pub trades: Vec<Trade>,
    pub summary: Summary,
}

fn trade(kind: &'static str, time: i64, strike: f64, spot: f64) -> Trade {
    Trade {
        time,
        kind,
        old_strike: None,
        strike,
        exit_ce: None,
        exit_pe: None,
        exit_straddle: None,
        ce_price: 0.0,
        pe_price: 0.0,
        straddle: 0.0,
        spot: py_round(spot, 2),
        leg_pnl: 0.0,
        cumulative_pnl: 0.0,
    }
}

/// Web `get_custom_straddle_simulation`, over the last `days` IST trading
/// dates present in `underlying` (sorted by time):
///
/// - ENTRY at the first bar of a day whose ATM call and put both priced:
///   sell both at that strike.
/// - ADJUSTMENT when the ATM strike is `adjustment_points` or more away
///   from the entry strike and all four prices exist: buy back the old
///   pair (realising its P&L), sell the new ATM pair.
/// - EXIT at the day's last bar; the day's P&L is realised.
///
/// P&L per pair is `(entry_ce - ce) + (entry_pe - pe)` times `quantity`.
pub fn simulate(
    underlying: &[Bar],
    atms: &[Option<f64>],
    data: &StrikeMap,
    days: i64,
    adjustment_points: f64,
    quantity: f64,
) -> Simulation {
    let mut by_day: BTreeMap<chrono::NaiveDate, Vec<(Bar, Option<f64>)>> = BTreeMap::new();
    for (b, atm) in underlying.iter().zip(atms) {
        if let Some(d) = ist_date(b.time) {
            by_day.entry(d).or_default().push((*b, *atm));
        }
    }
    let skip = by_day.len().saturating_sub(days.max(1) as usize);
    let mut cumulative = 0.0;
    let mut total_adjustments = 0i64;
    let mut series = Vec::new();
    let mut trades = Vec::new();
    for (_, candles) in by_day.into_iter().skip(skip) {
        let mut entry: Option<(f64, f64, f64)> = None; // strike, ce, pe
        let mut day_realized = 0.0;
        let mut day_adjustments = 0i64;
        let mut last_unrealized = 0.0;
        let n = candles.len();
        for (i, (b, atm)) in candles.iter().enumerate() {
            let t = b.time;
            let spot = b.close;
            let Some(atm) = *atm else { continue };
            if data.get(atm).is_none() {
                continue;
            }
            let is_last = i == n - 1;
            match entry {
                None => {
                    let (Some(ce), Some(pe)) = (data.ce(atm, t), data.pe(atm, t)) else {
                        continue;
                    };
                    entry = Some((atm, ce, pe));
                    let mut tr = trade("ENTRY", t, atm, spot);
                    tr.ce_price = py_round(ce, 2);
                    tr.pe_price = py_round(pe, 2);
                    tr.straddle = py_round(ce + pe, 2);
                    tr.cumulative_pnl = py_round(cumulative, 2);
                    trades.push(tr);
                }
                Some((k, e_ce, e_pe)) => {
                    if (atm - k).abs() >= adjustment_points {
                        if let (Some(o_ce), Some(o_pe), Some(n_ce), Some(n_pe)) = (
                            data.ce(k, t),
                            data.pe(k, t),
                            data.ce(atm, t),
                            data.pe(atm, t),
                        ) {
                            let leg = ((e_ce - o_ce) + (e_pe - o_pe)) * quantity;
                            day_realized += leg;
                            day_adjustments += 1;
                            let mut tr = trade("ADJUSTMENT", t, atm, spot);
                            tr.old_strike = Some(k);
                            tr.exit_ce = Some(py_round(o_ce, 2));
                            tr.exit_pe = Some(py_round(o_pe, 2));
                            tr.exit_straddle = Some(py_round(o_ce + o_pe, 2));
                            tr.ce_price = py_round(n_ce, 2);
                            tr.pe_price = py_round(n_pe, 2);
                            tr.straddle = py_round(n_ce + n_pe, 2);
                            tr.leg_pnl = py_round(leg, 2);
                            tr.cumulative_pnl = py_round(cumulative + day_realized, 2);
                            trades.push(tr);
                            entry = Some((atm, n_ce, n_pe));
                        }
                    }
                }
            }
            let Some((k, e_ce, e_pe)) = entry else {
                continue;
            };
            let unrealized = match (data.ce(k, t), data.pe(k, t)) {
                (Some(c), Some(p)) => {
                    last_unrealized = ((e_ce - c) + (e_pe - p)) * quantity;
                    last_unrealized
                }
                _ => last_unrealized,
            };
            let total = cumulative + day_realized + unrealized;
            let atm_ce = data.ce(atm, t).unwrap_or(0.0);
            let atm_pe = data.pe(atm, t).unwrap_or(0.0);
            let synthetic = if atm_ce != 0.0 && atm_pe != 0.0 {
                py_round(atm + atm_ce - atm_pe, 2)
            } else {
                py_round(spot, 2)
            };
            series.push(SimPoint {
                time: t,
                pnl: py_round(total, 2),
                spot: py_round(spot, 2),
                atm_strike: atm,
                entry_strike: k,
                ce_price: py_round(atm_ce, 2),
                pe_price: py_round(atm_pe, 2),
                straddle: py_round(atm_ce + atm_pe, 2),
                synthetic_future: synthetic,
                adjustments: total_adjustments + day_adjustments,
            });
            if is_last {
                let (x_ce, x_pe) = (data.ce(k, t), data.pe(k, t));
                let leg = match (x_ce, x_pe) {
                    (Some(c), Some(p)) => ((e_ce - c) + (e_pe - p)) * quantity,
                    _ => last_unrealized,
                };
                let (c, p) = (x_ce.unwrap_or(0.0), x_pe.unwrap_or(0.0));
                let mut tr = trade("EXIT", t, k, spot);
                tr.ce_price = py_round(c, 2);
                tr.pe_price = py_round(p, 2);
                tr.straddle = py_round(c + p, 2);
                tr.leg_pnl = py_round(leg, 2);
                tr.cumulative_pnl = py_round(cumulative + day_realized + leg, 2);
                trades.push(tr);
            }
        }
        if let Some((k, e_ce, e_pe)) = entry {
            let last_t = candles[n - 1].0.time;
            let final_leg = match (data.ce(k, last_t), data.pe(k, last_t)) {
                (Some(c), Some(p)) => ((e_ce - c) + (e_pe - p)) * quantity,
                _ => last_unrealized,
            };
            cumulative += day_realized + final_leg;
        }
        total_adjustments += day_adjustments;
    }
    let pnls = series.iter().map(|p| p.pnl);
    let max_pnl = pnls.clone().fold(f64::NEG_INFINITY, f64::max);
    let min_pnl = pnls.fold(f64::INFINITY, f64::min);
    let summary = Summary {
        total_pnl: py_round(cumulative, 2),
        total_adjustments,
        max_pnl: if series.is_empty() { 0.0 } else { max_pnl },
        min_pnl: if series.is_empty() { 0.0 } else { min_pnl },
    };
    Simulation {
        pnl_series: series,
        trades,
        summary,
    }
}

/// Web `_calculate_days_to_expiry`: whole days from `now` to the expiry at
/// 15:30 IST (floored), never negative.
pub fn days_to_expiry(expiry_ts: i64, now_ts: i64) -> i64 {
    (expiry_ts - now_ts).div_euclid(86_400).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono_tz::Asia::Kolkata;

    fn ts(d: u32, h: u32, mi: u32) -> i64 {
        Kolkata
            .with_ymd_and_hms(2026, 10, d, h, mi, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    fn bar(time: i64, close: f64) -> Bar {
        Bar {
            time,
            close,
            oi: 0.0,
        }
    }

    fn legs(rows: &[(i64, f64, f64)]) -> StrikeLegs {
        StrikeLegs {
            ce: rows.iter().map(|(t, c, _)| (*t, *c)).collect(),
            pe: rows.iter().map(|(t, _, p)| (*t, *p)).collect(),
        }
    }

    #[test]
    fn straddle_follows_the_atm_strike() {
        let (t1, t2, t3) = (ts(5, 9, 15), ts(5, 9, 16), ts(5, 9, 17));
        let u = [bar(t1, 101.0), bar(t2, 112.0), bar(t3, 104.0)];
        let strikes = [100.0, 110.0];
        let atms = atm_per_bar(&u, &strikes);
        assert_eq!(atms, vec![Some(100.0), Some(110.0), Some(100.0)]);
        assert_eq!(unique_strikes(&atms), vec![100.0, 110.0]);
        let mut m = StrikeMap::default();
        m.insert(100.0, legs(&[(t1, 5.0, 4.0), (t3, 6.0, 2.5)]));
        m.insert(110.0, legs(&[(t1, 1.0, 9.0)])); // no t2 bar: dropped
        let s = straddle_series(&u, &atms, &m);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].straddle, 9.0);
        assert_eq!(s[0].synthetic_future, 101.0);
        assert_eq!(s[1].time, t3);
        assert_eq!(s[1].synthetic_future, 103.5);
    }

    /// Worked by hand from web `services/custom_straddle_service.py:196-361`.
    /// Quantity 10, adjust at 10 points.
    ///   09:15 spot 101 -> ATM 100: sell 100 straddle at 5 + 4.
    ///   09:16 spot 112 -> ATM 110: buy back 100 at 7 + 3 -> (5-7 + 4-3)*10
    ///         = -10 realised; sell 110 at 6 + 6.
    ///   09:17 spot 111 -> ATM 110, last bar: 110 at 5 + 6 -> unrealised
    ///         (6-5 + 6-6)*10 = +10; exit, day total -10 + 10 = 0.
    #[test]
    fn simulation_matches_the_hand_computed_web_case() {
        let (t1, t2, t3) = (ts(5, 9, 15), ts(5, 9, 16), ts(5, 9, 17));
        let u = [bar(t1, 101.0), bar(t2, 112.0), bar(t3, 111.0)];
        let atms = atm_per_bar(&u, &[100.0, 110.0]);
        let mut m = StrikeMap::default();
        m.insert(
            100.0,
            legs(&[(t1, 5.0, 4.0), (t2, 7.0, 3.0), (t3, 8.0, 2.0)]),
        );
        m.insert(
            110.0,
            legs(&[(t1, 2.0, 9.0), (t2, 6.0, 6.0), (t3, 5.0, 6.0)]),
        );
        let sim = simulate(&u, &atms, &m, 1, 10.0, 10.0);
        let kinds: Vec<&str> = sim.trades.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec!["ENTRY", "ADJUSTMENT", "EXIT"]);
        let adj = sim.trades[1];
        assert_eq!(adj.old_strike, Some(100.0));
        assert_eq!(adj.leg_pnl, -10.0);
        assert_eq!(adj.exit_straddle, Some(10.0));
        assert_eq!(adj.straddle, 12.0);
        let exit = sim.trades[2];
        assert_eq!(exit.leg_pnl, 10.0);
        assert_eq!(exit.cumulative_pnl, 0.0);
        let pnl: Vec<f64> = sim.pnl_series.iter().map(|p| p.pnl).collect();
        // t1: entry, 0; t2: realised -10, new pair flat; t3: -10 + 10.
        assert_eq!(pnl, vec![0.0, -10.0, 0.0]);
        assert_eq!(sim.pnl_series[1].adjustments, 1);
        assert_eq!(sim.pnl_series[2].entry_strike, 110.0);
        assert_eq!(
            sim.summary,
            Summary {
                total_pnl: 0.0,
                total_adjustments: 1,
                max_pnl: 0.0,
                min_pnl: -10.0
            }
        );
        // Serialized like the web's dicts: `type`, optional exit fields.
        let v = serde_json::to_value(sim.trades[0]).unwrap();
        assert_eq!(v["type"], "ENTRY");
        assert!(v.get("old_strike").is_none());
    }

    #[test]
    fn simulation_carries_pnl_across_days_and_limits_to_n_days() {
        let mk = |d: u32| {
            let (a, b) = (ts(d, 9, 15), ts(d, 15, 29));
            (a, b)
        };
        let (a1, b1) = mk(5);
        let (a2, b2) = mk(6);
        let u = [
            bar(a1, 100.0),
            bar(b1, 100.0),
            bar(a2, 100.0),
            bar(b2, 100.0),
        ];
        let atms = atm_per_bar(&u, &[100.0]);
        let mut m = StrikeMap::default();
        // Each day the straddle decays from 10 to 6: +4 * qty.
        m.insert(
            100.0,
            legs(&[
                (a1, 5.0, 5.0),
                (b1, 3.0, 3.0),
                (a2, 5.0, 5.0),
                (b2, 3.0, 3.0),
            ]),
        );
        let two = simulate(&u, &atms, &m, 2, 50.0, 2.0);
        assert_eq!(two.summary.total_pnl, 16.0);
        assert_eq!(two.pnl_series.last().unwrap().pnl, 16.0);
        let one = simulate(&u, &atms, &m, 1, 50.0, 2.0);
        assert_eq!(one.summary.total_pnl, 8.0);
        assert_eq!(one.pnl_series.len(), 2);
    }

    #[test]
    fn days_to_expiry_floors_and_clamps() {
        let exp = ts(8, 15, 30);
        assert_eq!(days_to_expiry(exp, ts(5, 10, 0)), 3);
        assert_eq!(days_to_expiry(exp, ts(5, 16, 0)), 2);
        assert_eq!(days_to_expiry(exp, ts(9, 10, 0)), 0);
    }
}
