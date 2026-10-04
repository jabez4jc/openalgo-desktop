//! Volatility surface grid (web `vol_surface_service.py`): a rectangular
//! strike x expiry grid with the OTM convention (calls at and above ATM,
//! puts below). The web does not interpolate missing cells: an IV that
//! cannot be inverted is a gap (`None`) the chart leaves open.

/// The ATM strike and the `count` strikes either side of it.
pub fn strike_window(strikes: &[f64], ltp: f64, count: usize) -> Option<(f64, Vec<f64>)> {
    let atm = super::closest_strike(strikes, ltp)?;
    let idx = strikes.iter().position(|s| *s == atm)?;
    let start = idx.saturating_sub(count);
    let end = (idx + count + 1).min(strikes.len());
    Some((atm, strikes[start..end].to_vec()))
}

/// Strikes every expiry shares, ascending; fewer than three falls back to
/// the first expiry's strikes (the surface then has gaps).
pub fn common_grid(windows: &[Vec<f64>]) -> Vec<f64> {
    let Some(first) = windows.first() else {
        return Vec::new();
    };
    let mut common: Vec<f64> = first
        .iter()
        .copied()
        .filter(|k| windows[1..].iter().all(|w| w.contains(k)))
        .collect();
    common.sort_by(|a, b| a.total_cmp(b));
    common.dedup();
    if common.len() < 3 {
        let mut f = first.clone();
        f.sort_by(|a, b| a.total_cmp(b));
        return f;
    }
    common
}

/// The OTM leg used for a strike: `CE` at or above ATM, `PE` below.
pub fn otm_side(strike: f64, atm: f64) -> &'static str {
    if strike >= atm {
        "CE"
    } else {
        "PE"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_grid_and_sides() {
        let k: Vec<f64> = (0..10).map(|i| 100.0 + 10.0 * i as f64).collect();
        let (atm, w) = strike_window(&k, 141.0, 2).unwrap();
        assert_eq!(atm, 140.0);
        assert_eq!(w, vec![120.0, 130.0, 140.0, 150.0, 160.0]);
        let (_, edge) = strike_window(&k, 0.0, 2).unwrap();
        assert_eq!(edge, vec![100.0, 110.0, 120.0]);
        assert!(strike_window(&[], 1.0, 2).is_none());

        let a = vec![120.0, 130.0, 140.0, 150.0, 160.0];
        let b = vec![130.0, 140.0, 150.0, 160.0, 170.0];
        assert_eq!(
            common_grid(&[a.clone(), b]),
            vec![130.0, 140.0, 150.0, 160.0]
        );
        // Two shared strikes are too few: first expiry's strikes.
        let c = vec![150.0, 160.0, 170.0];
        assert_eq!(common_grid(&[a.clone(), c]), a);
        assert!(common_grid(&[]).is_empty());

        assert_eq!(otm_side(140.0, 140.0), "CE");
        assert_eq!(otm_side(130.0, 140.0), "PE");
    }
}
