//! Kelly Criterion Position Sizing for HFT
//!
//! Kelly Fraction = (p·b − q) / b  with q = 1 − p, b = expected payoff ratio.
//!
//! SNN "confidence" is **not** a calibrated win probability. Prefer fractional
//! Kelly (default 0.25×) and realistic `b` (e.g. 0.02–0.10 for short-horizon moves).
//! With b = 0.01, positive Kelly only appears when p > ~0.990.

use tracing::info;

/// Kelly criterion calculator for position sizing
pub struct KellyCriterion {
    /// Expected payoff ratio (price move / entry price)
    /// Example: 0.02 means expecting 2% move
    pub expected_payoff_ratio: f64,
}

impl Default for KellyCriterion {
    fn default() -> Self {
        Self {
            expected_payoff_ratio: 0.01, // 1% expected move (conservative)
        }
    }
}

impl KellyCriterion {
    pub fn new(expected_payoff_ratio: f64) -> Self {
        Self {
            expected_payoff_ratio,
        }
    }

    /// Calculate Kelly fraction given SNN confidence
    ///
    /// # Arguments
    /// - `win_probability`: SNN confidence (0.0 - 1.0)
    /// - `loss_probability`: 1.0 - win_probability (auto-calculated)
    /// - `odds`: expected_payoff_ratio (b parameter)
    ///
    /// # Formula
    /// Kelly Fraction = (p*b - q) / b
    /// where:
    ///   p = win_probability
    ///   q = loss_probability (1 - p)
    ///   b = odds (payoff_ratio)
    ///
    /// # Returns
    /// Kelly fraction in `0.0..=1.0` (invalid inputs fail closed to `0.0`).
    pub fn calculate_fraction(&self, win_probability: f32) -> f64 {
        let p = win_probability as f64;
        let b = self.expected_payoff_ratio;

        // Fail closed on non-probability inputs and non-positive odds.
        if !p.is_finite() || !(0.0..=1.0).contains(&p) || !b.is_finite() || b <= 0.0 {
            return 0.0;
        }

        let q = 1.0 - p;
        // Kelly Formula: F = (p*b - q) / b
        let kelly_frac = (p * b - q) / b;

        if !kelly_frac.is_finite() {
            return 0.0;
        }

        kelly_frac.clamp(0.0, 1.0)
    }

    /// Position size given Kelly fraction and account balance
    pub fn position_size(&self, kelly_fraction: f64, account_balance: f64, price: f64) -> f64 {
        // position_size = (kelly_fraction * account_balance) / price
        (kelly_fraction * account_balance) / price
    }

    /// Risk-adjusted Kelly (fractional Kelly)
    /// Most professionals use 0.5x or 0.25x Kelly for safety
    pub fn fractional_kelly(kelly_frac: f64, fraction: f64) -> f64 {
        (kelly_frac * fraction).min(1.0)
    }
}

/// Decision tree for position sizing based on SNN confidence.
pub struct PositionSizer {
    kelly: KellyCriterion,
    account_balance: f64,
    /// Fractional Kelly scalar (default 0.25 for research safety).
    kelly_fraction_scalar: f64,
}

impl PositionSizer {
    pub fn new(
        account_balance: f64,
        expected_payoff_ratio: f64,
        kelly_fraction_scalar: f64,
    ) -> Self {
        Self {
            kelly: KellyCriterion::new(expected_payoff_ratio),
            account_balance: account_balance.max(0.0),
            kelly_fraction_scalar,
        }
    }

    pub fn set_account_balance(&mut self, balance: f64) {
        self.account_balance = balance.max(0.0);
    }

    pub fn account_balance(&self) -> f64 {
        self.account_balance
    }

    pub fn expected_payoff_ratio(&self) -> f64 {
        self.kelly.expected_payoff_ratio
    }

    /// Size from SNN confidence (soft win-prob) and current price.
    pub fn size_position(
        &self,
        snn_confidence: f32,
        current_price: f64,
        ticker: &str,
    ) -> PositionSizingDecision {
        let price = current_price.max(1e-12);
        let raw_kelly = self.kelly.calculate_fraction(snn_confidence);
        let kelly_frac = KellyCriterion::fractional_kelly(raw_kelly, self.kelly_fraction_scalar);
        let position_units = self
            .kelly
            .position_size(kelly_frac, self.account_balance, price);
        let risk_tier = self.risk_tier(snn_confidence);

        info!(
            "[{}] Kelly sizing: conf={:.2} raw={:.3} frac={:.3} qty={:.6} (tier: {:?})",
            ticker, snn_confidence, raw_kelly, kelly_frac, position_units, risk_tier
        );

        PositionSizingDecision {
            position_units,
            kelly_fraction: kelly_frac,
            raw_kelly,
            snn_confidence,
            risk_tier,
            account_risk_percent: if self.account_balance > 0.0 {
                (position_units * price) / self.account_balance * 100.0
            } else {
                0.0
            },
        }
    }

    fn risk_tier(&self, confidence: f32) -> RiskTier {
        match confidence {
            c if c >= 0.95 => RiskTier::Aggressive,
            c if c >= 0.85 => RiskTier::Moderate,
            c if c >= 0.70 => RiskTier::Conservative,
            _ => RiskTier::Minimal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskTier {
    Aggressive,   // 0.95+
    Moderate,     // 0.85 - 0.94
    Conservative, // 0.70 - 0.84
    Minimal,      // < 0.70
}

#[derive(Debug, Clone)]
pub struct PositionSizingDecision {
    pub position_units: f64,
    /// After fractional Kelly scalar.
    pub kelly_fraction: f64,
    /// Full Kelly before fractional scalar.
    pub raw_kelly: f64,
    pub snn_confidence: f32,
    pub risk_tier: RiskTier,
    pub account_risk_percent: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kelly_high_confidence_with_realistic_b() {
        // b=0.05 → breakeven p ≈ 0.952; p=0.98 yields positive fraction
        let kelly = KellyCriterion::new(0.05);
        let frac = kelly.calculate_fraction(0.98);
        assert!(
            frac > 0.5,
            "high p with b=0.05 should be substantial: got {frac}"
        );
    }

    #[test]
    fn test_kelly_zero_when_edge_negative() {
        // b=0.01, p=0.95 → F = (0.0095 - 0.05)/0.01 < 0 → clamp 0
        let kelly = KellyCriterion::new(0.01);
        let frac = kelly.calculate_fraction(0.95);
        assert_eq!(frac, 0.0);
    }

    #[test]
    fn test_kelly_low_confidence() {
        let kelly = KellyCriterion::new(0.05);
        let frac = kelly.calculate_fraction(0.55);
        assert_eq!(frac, 0.0);
    }

    #[test]
    fn test_fractional_kelly() {
        let kelly_frac = 0.6;
        let quarter = KellyCriterion::fractional_kelly(kelly_frac, 0.25);
        assert!((quarter - 0.15).abs() < 1e-9);
    }

    #[test]
    fn test_position_sizer() {
        // 0.25× fractional Kelly, b=0.05, p=0.98 → positive size
        let sizer = PositionSizer::new(100_000.0, 0.05, 0.25);
        let decision = sizer.size_position(0.98, 65_000.0, "BTC-USD");
        assert!(decision.position_units > 0.0);
        assert!(decision.raw_kelly > decision.kelly_fraction);
        assert_eq!(decision.snn_confidence, 0.98);
        assert_eq!(decision.risk_tier, RiskTier::Aggressive);
    }

    #[test]
    fn test_set_account_balance() {
        let mut sizer = PositionSizer::new(100.0, 0.05, 0.25);
        sizer.set_account_balance(500.0);
        assert_eq!(sizer.account_balance(), 500.0);
    }

    #[test]
    fn test_kelly_rejects_negative_payoff_ratio() {
        // Without validation, b=-1 and p=0.5 clamped to max (1.0); must fail closed.
        let kelly = KellyCriterion::new(-1.0);
        assert_eq!(kelly.calculate_fraction(0.5), 0.0);
    }

    #[test]
    fn test_kelly_rejects_out_of_range_probability() {
        let kelly = KellyCriterion::new(0.05);
        assert_eq!(kelly.calculate_fraction(-0.1), 0.0);
        assert_eq!(kelly.calculate_fraction(1.5), 0.0);
        assert_eq!(kelly.calculate_fraction(f32::NAN), 0.0);
    }
}
