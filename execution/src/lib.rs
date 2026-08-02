//! Limen-Capital Execution Engine — Rust Muscle
//!
//! Deep **Decision** path:
//! ```text
//! TradeSignal → confidence gate → Neutral filter
//!   → fractional Kelly size (PositionSizer)
//!   → max-position gate → metabolic-ledger ATP ghost trade → JSONL
//! ```
//!
//! Note: `metabolic-ledger` sizes spend from wallet ATP × ledger Kelly;
//! this module's PositionSizer is the **policy surface** for confidence→size
//! and soft limits, logged on every fill.

pub mod binary_wire;
pub mod dydx;
pub mod kelly;
pub mod wire;

pub use metabolic_ledger::{execute_buy, execute_sell, GhostWallet, MarketPrices};

use crate::kelly::{PositionSizer, PositionSizingDecision};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Trade signal from Julia strategy (JSON path / binary map).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeSignal {
    pub ticker: String,
    pub side: TradeSide,
    pub price: f64,
    /// Advisory quantity; ignored when `use_kelly_sizing` (default true).
    pub quantity: f64,
    /// SNN confidence (0.0 - 1.0) — treated as soft win-prob with fractional Kelly.
    pub confidence: f32,
    /// Unix nanoseconds for end-to-end latency tracking
    pub timestamp_ns: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TradeSide {
    Buy,
    Sell,
    Neutral,
}

impl TradeSignal {
    pub fn latency_ns(&self) -> i64 {
        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        now_ns - self.timestamp_ns
    }

    pub fn passes_gate(&self, threshold: f32) -> bool {
        self.confidence >= threshold
    }
}

/// Execution engine: gates + Kelly + metabolic-ledger (one Decision module).
pub struct ExecutionEngine {
    pub confidence_threshold: f32,
    pub max_position_size: f64,
    pub ghost_wallet: GhostWallet,
    pub sizer: PositionSizer,
    /// When true, size from Kelly+ATP; signal.quantity only used if false.
    pub use_kelly_sizing: bool,
    pub log_path: String,
    pub step_counter: u64,
}

impl Default for ExecutionEngine {
    fn default() -> Self {
        Self::new(0.15, 10.0)
    }
}

impl ExecutionEngine {
    /// Default research-friendly thresholds; Kelly uses b=0.05, 0.25× fractional.
    pub fn new(confidence_threshold: f32, max_position_size: f64) -> Self {
        let wallet = GhostWallet::new();
        let account = wallet.balance_atp as f64;
        Self {
            confidence_threshold,
            max_position_size,
            ghost_wallet: wallet,
            sizer: PositionSizer::new(account, 0.05, 0.25),
            use_kelly_sizing: true,
            log_path: "ghost_trades.jsonl".to_string(),
            step_counter: 0,
        }
    }

    pub fn with_log_path(mut self, log_path: String) -> Self {
        self.log_path = log_path;
        self
    }

    pub fn with_kelly_sizing(mut self, enabled: bool) -> Self {
        self.use_kelly_sizing = enabled;
        self
    }

    pub fn with_sizer(mut self, sizer: PositionSizer) -> Self {
        self.sizer = sizer;
        self
    }

    fn sync_sizer_balance(&mut self) {
        self.sizer
            .set_account_balance(self.ghost_wallet.balance_atp as f64);
    }

    /// Full decision path: gates → size → ghost execute.
    pub fn process_signal(&mut self, signal: TradeSignal) -> ExecutionDecision {
        let latency = signal.latency_ns();

        // Gate 1: Confidence
        if !signal.passes_gate(self.confidence_threshold) {
            warn!(
                "Signal rejected: confidence {} < threshold {}",
                signal.confidence, self.confidence_threshold
            );
            return ExecutionDecision::RejectedLowConfidence {
                reason: format!(
                    "Confidence {} below threshold {}",
                    signal.confidence, self.confidence_threshold
                ),
                latency_ns: latency,
            };
        }

        // Gate 2: Neutral
        if signal.side == TradeSide::Neutral {
            info!(
                "[{}] Neutral (confidence: {}, latency: {}ns)",
                signal.ticker, signal.confidence, latency
            );
            return ExecutionDecision::Neutral {
                latency_ns: latency,
            };
        }

        // Size: fractional Kelly from ATP bankroll (policy)
        self.sync_sizer_balance();
        let sizing = self
            .sizer
            .size_position(signal.confidence, signal.price, &signal.ticker);

        let quantity = if self.use_kelly_sizing {
            sizing.position_units
        } else {
            signal.quantity
        };

        if self.use_kelly_sizing && sizing.kelly_fraction <= 0.0 {
            warn!(
                "[{}] No edge under Kelly (conf={}, b={})",
                signal.ticker,
                signal.confidence,
                self.sizer.expected_payoff_ratio()
            );
            return ExecutionDecision::RejectedNoEdge {
                reason: format!(
                    "Kelly fraction 0 for confidence {} (need higher conf or larger b)",
                    signal.confidence
                ),
                latency_ns: latency,
                kelly_fraction: sizing.kelly_fraction,
            };
        }

        if quantity <= 0.0 {
            return ExecutionDecision::RejectedNoEdge {
                reason: "position size is zero".into(),
                latency_ns: latency,
                kelly_fraction: sizing.kelly_fraction,
            };
        }

        // Gate 3: Soft max position (on policy size)
        if quantity > self.max_position_size {
            warn!(
                "Signal rejected: size {} > max {}",
                quantity, self.max_position_size
            );
            return ExecutionDecision::RejectedPositionLimit {
                reason: format!(
                    "Quantity {} exceeds max {}",
                    quantity, self.max_position_size
                ),
                latency_ns: latency,
            };
        }

        // Execute via metabolic-ledger (ATP energy commitment inside ledger)
        self.step_counter += 1;
        let reason = format!(
            "snn_conf={:.3};kelly_frac={:.4};policy_qty={:.6};tier={:?}",
            signal.confidence, sizing.kelly_fraction, quantity, sizing.risk_tier
        );

        match signal.side {
            TradeSide::Buy => {
                execute_buy(
                    &mut self.ghost_wallet,
                    &signal.ticker,
                    signal.price as f32,
                    self.step_counter,
                    &reason,
                    Some(&self.log_path),
                );
            }
            TradeSide::Sell => {
                execute_sell(
                    &mut self.ghost_wallet,
                    &signal.ticker,
                    signal.price as f32,
                    self.step_counter,
                    &reason,
                    Some(&self.log_path),
                );
            }
            TradeSide::Neutral => {}
        }

        info!(
            "[{}] EXECUTE {:?} @ ${:.4} qty≈{:.6} kelly={:.3} conf={:.2} lat={}ns ATP={:.2}",
            signal.ticker,
            signal.side,
            signal.price,
            quantity,
            sizing.kelly_fraction,
            signal.confidence,
            latency,
            self.ghost_wallet.balance_atp
        );

        ExecutionDecision::Executed {
            ticker: signal.ticker,
            side: signal.side,
            quantity,
            price: signal.price,
            confidence: signal.confidence,
            kelly_fraction: sizing.kelly_fraction,
            latency_ns: latency,
            sizing,
        }
    }
}

#[derive(Debug)]
pub enum ExecutionDecision {
    Executed {
        ticker: String,
        side: TradeSide,
        quantity: f64,
        price: f64,
        confidence: f32,
        kelly_fraction: f64,
        latency_ns: i64,
        sizing: PositionSizingDecision,
    },
    Neutral {
        latency_ns: i64,
    },
    RejectedLowConfidence {
        reason: String,
        latency_ns: i64,
    },
    RejectedPositionLimit {
        reason: String,
        latency_ns: i64,
    },
    RejectedNoEdge {
        reason: String,
        latency_ns: i64,
        kelly_fraction: f64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts_now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64
    }

    #[test]
    fn test_confidence_gate() {
        let mut engine = ExecutionEngine::new(0.90, 10.0);
        let signal = TradeSignal {
            ticker: "DNX".into(),
            side: TradeSide::Buy,
            price: 1.0,
            quantity: 1.0,
            confidence: 0.85,
            timestamp_ns: ts_now(),
        };
        assert!(matches!(
            engine.process_signal(signal),
            ExecutionDecision::RejectedLowConfidence { .. }
        ));
    }

    #[test]
    fn test_kelly_no_edge() {
        // conf too low for b=0.05 → Kelly 0
        let mut engine = ExecutionEngine::new(0.10, 10.0);
        let signal = TradeSignal {
            ticker: "DNX".into(),
            side: TradeSide::Buy,
            price: 1.0,
            quantity: 1.0,
            confidence: 0.50,
            timestamp_ns: ts_now(),
        };
        assert!(matches!(
            engine.process_signal(signal),
            ExecutionDecision::RejectedNoEdge { .. }
        ));
    }

    #[test]
    fn test_position_limit_without_kelly() {
        let mut engine = ExecutionEngine::new(0.80, 5.0).with_kelly_sizing(false);
        let signal = TradeSignal {
            ticker: "ETH".into(),
            side: TradeSide::Buy,
            price: 3500.0,
            quantity: 10.0,
            confidence: 0.95,
            timestamp_ns: ts_now(),
        };
        assert!(matches!(
            engine.process_signal(signal),
            ExecutionDecision::RejectedPositionLimit { .. }
        ));
    }

    #[test]
    fn test_execution_with_kelly() {
        let mut engine = ExecutionEngine::new(0.80, 100.0).with_log_path(
            std::env::temp_dir()
                .join("limen_capital_ghost_c6.jsonl")
                .to_string_lossy()
                .into_owned(),
        );
        let signal = TradeSignal {
            ticker: "DNX".into(),
            side: TradeSide::Buy,
            price: 1.0,
            quantity: 0.0, // ignored under Kelly
            confidence: 0.98,
            timestamp_ns: ts_now(),
        };

        let decision = engine.process_signal(signal);
        match decision {
            ExecutionDecision::Executed {
                quantity,
                kelly_fraction,
                ..
            } => {
                assert!(quantity > 0.0);
                assert!(kelly_fraction > 0.0);
                assert!(engine.ghost_wallet.balance("DNX") > 0.0);
            }
            other => panic!("expected Executed, got {other:?}"),
        }
    }

    #[test]
    fn test_json_side_lowercase() {
        let json = r#"{"ticker":"DNX","side":"buy","price":1.0,"quantity":1.0,"confidence":0.9,"timestamp_ns":0}"#;
        let sig: TradeSignal = serde_json::from_str(json).unwrap();
        assert_eq!(sig.side, TradeSide::Buy);
    }

    #[test]
    fn test_neutral() {
        let mut engine = ExecutionEngine::new(0.1, 10.0);
        let signal = TradeSignal {
            ticker: "DNX".into(),
            side: TradeSide::Neutral,
            price: 1.0,
            quantity: 0.0,
            confidence: 0.99,
            timestamp_ns: ts_now(),
        };
        assert!(matches!(
            engine.process_signal(signal),
            ExecutionDecision::Neutral { .. }
        ));
    }
}
