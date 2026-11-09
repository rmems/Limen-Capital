//! Venue price and quantity grids.

/// Number of decimals needed to represent multiples of an increment exactly
/// (`0.01` → 2, `1e-8` → 8, `0.25` → 2, `0.5` → 1, `1` → 0).
pub fn decimals_for_increment(inc: f64) -> usize {
    if !(inc.is_finite() && inc > 0.0) {
        return 8;
    }
    for d in 0..=18usize {
        let scaled = inc * 10f64.powi(d as i32);
        let r = scaled.round();
        // `scaled` must be a positive integer: a tiny increment scaled by a
        // small power rounds to 0, which must not count as representable.
        if r >= 1.0 && (scaled - r).abs() < 1e-6 {
            return d;
        }
    }
    18
}

/// Round `x` down to a multiple of `inc` (with a tiny epsilon so exact
/// multiples are not knocked down by float noise).
pub fn round_down_to_increment(x: f64, inc: f64) -> f64 {
    if !(inc.is_finite() && inc > 0.0) || !x.is_finite() {
        return x;
    }
    let steps = (x / inc + 1e-9).floor();
    let d = decimals_for_increment(inc);
    let p = 10f64.powi(d as i32);
    ((steps * inc) * p).round() / p
}

/// Round `x` up to a multiple of `inc`.
pub fn round_up_to_increment(x: f64, inc: f64) -> f64 {
    if !(inc.is_finite() && inc > 0.0) || !x.is_finite() {
        return x;
    }
    let steps = (x / inc - 1e-9).ceil();
    let d = decimals_for_increment(inc);
    let p = 10f64.powi(d as i32);
    ((steps * inc) * p).round() / p
}

/// Format with exactly the increment's decimals (what the venue expects).
pub fn format_at_increment(x: f64, inc: f64) -> String {
    format!("{:.*}", decimals_for_increment(inc), x)
}
