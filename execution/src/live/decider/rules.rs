//! `RulesDecider` — a deterministic mean-reversion toy.
//!
//! Used in tests, as the paper-mode default, and as the fallback when the
//! LLM decider errors. It is intentionally simple and **not** a strategy:
//!
//! * flat and mid is `entry_bps` below the recent-window mean → buy `order_usd`
//! * long and mid is `exit_bps` above average cost → sell the whole position
//! * otherwise hold
//!
//! Sizes are advisory; `risk::check` still applies every cap.

use super::{DecideFuture, Decider, Decision};
use crate::live::market_state::MarketState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RulesConfig {
    /// Buy when mid ≤ window mean × (1 − entry_bps/1e4).
    pub entry_bps: f64,
    /// Sell when mid ≥ avg cost × (1 + exit_bps/1e4).
    pub exit_bps: f64,
    /// Proposed buy notional in USD.
    pub order_usd: f64,
    /// Need at least this many mids in the window before trading.
    pub min_window: usize,
}

impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            entry_bps: 10.0,
            exit_bps: 10.0,
            order_usd: 5.0,
            min_window: 3,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RulesDecider {
    cfg: RulesConfig,
}

impl RulesDecider {
    pub fn new(cfg: RulesConfig) -> Self {
        Self { cfg }
    }

    /// Synchronous core (the async trait method wraps this).
    pub fn decide_sync(&self, state: &MarketState) -> Decision {
        if !state.quote_is_sane() {
            return Decision::hold("bad_quote");
        }
        if state.recent_mids.len() < self.cfg.min_window {
            return Decision::hold(format!(
                "warming_up {}/{}",
                state.recent_mids.len(),
                self.cfg.min_window
            ));
        }
        let mean = state.window_mean();
        let dev_bps = if mean > 0.0 {
            (state.mid / mean - 1.0) * 10_000.0
        } else {
            0.0
        };

        if state.position_qty > 0.0 {
            if let Some(cost) = state.avg_cost_usd {
                let gain_bps = (state.mid / cost - 1.0) * 10_000.0;
                if gain_bps >= self.cfg.exit_bps {
                    // Ask to sell everything; risk clamps to holdings.
                    return Decision::sell(
                        state.position_usd,
                        format!("take_profit {gain_bps:+.1}bps vs cost"),
                    );
                }
                return Decision::hold(format!("long, {gain_bps:+.1}bps vs cost"));
            }
            return Decision::hold("long, no cost basis");
        }

        if dev_bps <= -self.cfg.entry_bps {
            let size = self.cfg.order_usd.min(state.cash_usd);
            return Decision::buy(
                size,
                format!("mean_reversion {dev_bps:+.1}bps vs window mean"),
            );
        }
        Decision::hold(format!("flat, {dev_bps:+.1}bps vs window mean"))
    }
}

impl Decider for RulesDecider {
    fn name(&self) -> &str {
        "rules"
    }

    fn decide<'a>(
        &'a mut self,
        state: &'a MarketState,
        _recent_journal: &'a [String],
    ) -> DecideFuture<'a> {
        let d = self.decide_sync(state);
        Box::pin(async move { Ok(d) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::decider::Action;

    fn st(mids: &[f64], bid: f64, ask: f64, qty: f64, cost: Option<f64>, cash: f64) -> MarketState {
        let prior = mids[..mids.len() - 1].to_vec();
        let account = crate::live::market_state::AccountState {
            position_qty: qty,
            avg_cost_usd: cost,
            cash_usd: cash,
            realized_pnl_usd: 0.0,
        };
        let s = MarketState::new(0, 1, "BTC-USD", bid, ask, account, prior);
        assert_eq!(*s.recent_mids.last().unwrap(), (bid + ask) / 2.0);
        s
    }

    #[test]
    fn warms_up_then_holds_when_flat_and_near_mean() {
        let d = RulesDecider::new(RulesConfig::default());
        let s = st(&[100.0, 100.0], 99.9, 100.1, 0.0, None, 10.0);
        assert!(d.decide_sync(&s).reason.starts_with("warming_up"));
        let s = st(&[100.0, 100.0, 100.0], 99.9, 100.1, 0.0, None, 10.0);
        let dec = d.decide_sync(&s);
        assert_eq!(dec.action, Action::Hold);
        assert!(dec.reason.starts_with("flat"));
    }

    #[test]
    fn buys_on_dip_capped_by_cash() {
        let d = RulesDecider::new(RulesConfig::default());
        // mean of [100,100,99.7] = 99.9; mid 99.7 is -20 bps → buy.
        let s = st(&[100.0, 100.0, 99.7], 99.65, 99.75, 0.0, None, 3.0);
        let dec = d.decide_sync(&s);
        assert_eq!(dec.action, Action::Buy);
        assert_eq!(dec.size_usd, 3.0);
        let s = st(&[100.0, 100.0, 99.7], 99.65, 99.75, 0.0, None, 10.0);
        assert_eq!(d.decide_sync(&s).size_usd, 5.0);
    }

    #[test]
    fn sells_whole_position_at_take_profit() {
        let d = RulesDecider::new(RulesConfig::default());
        let s = st(
            &[100.0, 100.0, 100.2],
            100.15,
            100.25,
            0.05,
            Some(100.0),
            5.0,
        );
        let dec = d.decide_sync(&s);
        assert_eq!(dec.action, Action::Sell);
        assert!((dec.size_usd - s.position_usd).abs() < 1e-9);
        // Below take-profit: hold the long even on a dip (no averaging down).
        let s = st(&[100.0, 100.0, 99.0], 98.95, 99.05, 0.05, Some(100.0), 5.0);
        let dec = d.decide_sync(&s);
        assert_eq!(dec.action, Action::Hold);
        assert!(dec.reason.starts_with("long"));
    }

    #[tokio::test]
    async fn trait_wrapper_is_deterministic() {
        let mut d = RulesDecider::new(RulesConfig::default());
        let s = st(&[100.0, 100.0, 99.7], 99.65, 99.75, 0.0, None, 10.0);
        let a = d.decide(&s, &[]).await.unwrap();
        let b = d.decide(&s, &[]).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(d.name(), "rules");
        assert_eq!(d.llm_cost_today_usd(), 0.0);
    }
}
