//! Limen-Capital Execution Engine — Rust Muscle
//!
//! Deep **Decision** path:
//! ```text
//! TradeSignal → confidence gate → Neutral filter
//!   → fractional Kelly size (PositionSizer)
//!   → per-order notional cap → quantity-aware ATP ghost trade → JSONL
//! ```
//!
//! The policy size is capped once, then applied to the legacy GhostWallet.

pub mod binary_wire;
pub mod confidence;
pub mod config;
pub mod dydx;
mod ghost;
pub mod kelly;
/// Live micro-capital experiment (RM-2436): Robinhood Crypto adapter + decision loop.
/// Feature-gated; **not** part of the research path and not a research result.
#[cfg(feature = "live-robinhood")]
pub mod live;
pub mod universe;
pub mod wire;
pub mod zmq_curve;

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
        self.confidence.is_finite()
            && (0.0..=1.0).contains(&self.confidence)
            && self.confidence >= threshold
    }
}

/// Execution engine: gates + Kelly + metabolic-ledger (one Decision module).
pub struct ExecutionEngine {
    pub confidence_threshold: f32,
    /// Per-order quote notional, not units or total held exposure.
    pub max_order_notional: f64,
    pub ghost_wallet: GhostWallet,
    pub sizer: PositionSizer,
    /// When true, size from Kelly+ATP; signal.quantity only used if false.
    pub use_kelly_sizing: bool,
    pub log_path: String,
    pub step_counter: u64,
    pub confidence_stats: confidence::ConfidenceStats,
}

impl Default for ExecutionEngine {
    fn default() -> Self {
        Self::new(0.15, 10.0)
    }
}

impl ExecutionEngine {
    /// Library convenience defaults: symmetric gain/loss (b=1), quarter Kelly.
    /// The listener requires an explicit LIMEN_KELLY_B via ExecutionConfig.
    pub fn new(confidence_threshold: f32, max_order_notional: f64) -> Self {
        let wallet = GhostWallet::new();
        let account = wallet.balance_atp as f64;
        Self {
            confidence_threshold,
            max_order_notional,
            ghost_wallet: wallet,
            sizer: PositionSizer::new(account, 1.0, 0.25),
            use_kelly_sizing: true,
            log_path: "ghost_trades.jsonl".to_string(),
            step_counter: 0,
            confidence_stats: confidence::ConfidenceStats::default(),
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
        let confidence = signal.confidence;
        let decision = self.decide(signal);
        self.confidence_stats.record(confidence, &decision);
        if self.confidence_stats.count.is_multiple_of(100) {
            self.confidence_stats.log();
        }
        decision
    }

    fn decide(&mut self, signal: TradeSignal) -> ExecutionDecision {
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

        if !signal.price.is_finite()
            || signal.price <= 0.0
            || !(signal.price as f32).is_finite()
            || (signal.price as f32) <= 0.0
            || !self.max_order_notional.is_finite()
            || self.max_order_notional <= 0.0
        {
            return ExecutionDecision::RejectedPositionLimit {
                reason: "invalid price or notional limit".into(),
                latency_ns: latency,
            };
        }

        // Size: fractional Kelly from ATP bankroll (policy)
        self.sync_sizer_balance();
        let mut sizing = self
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

        if !quantity.is_finite() || quantity <= 0.0 {
            return ExecutionDecision::RejectedNoEdge {
                reason: "position size is zero".into(),
                latency_ns: latency,
                kelly_fraction: sizing.kelly_fraction,
            };
        }

        // Cap in quote units, then convert back to asset units. High edge must
        // not turn a valid signal into a rejection.
        let quantity = quantity.min(self.max_order_notional / signal.price);

        // Apply this size to GhostWallet, without the ledger helper re-sizing it.
        self.step_counter += 1;
        let reason = format!(
            "snn_conf={:.3};kelly_frac={:.4};policy_qty={:.6};tier={:?}",
            signal.confidence, sizing.kelly_fraction, quantity, sizing.risk_tier
        );

        let Some(quantity) = ghost::execute_sized(
            &mut self.ghost_wallet,
            &signal,
            quantity,
            self.step_counter,
            &reason,
            &self.log_path,
        ) else {
            return ExecutionDecision::RejectedPositionLimit {
                reason: "insufficient ATP, inventory, or below ghost minimum".into(),
                latency_ns: latency,
            };
        };
        sizing.position_units = quantity;
        sizing.account_risk_percent = if self.sizer.account_balance() > 0.0 {
            quantity * signal.price / self.sizer.account_balance() * 100.0
        } else {
            0.0
        };

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

impl Drop for ExecutionEngine {
    fn drop(&mut self) {
        // Includes short runs (<100 signals) and the final partial interval.
        self.confidence_stats.log();
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
            ticker: "TESTSYM".into(),
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
        let mut engine =
            ExecutionEngine::new(0.10, 10.0).with_sizer(PositionSizer::new(500.0, 0.05, 0.25));
        let signal = TradeSignal {
            ticker: "TESTSYM".into(),
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
        let dir = tempfile::tempdir().unwrap();
        let mut engine = ExecutionEngine::new(0.80, 5.0)
            .with_kelly_sizing(false)
            .with_log_path(
                dir.path()
                    .join("trades.jsonl")
                    .to_string_lossy()
                    .into_owned(),
            );
        let signal = TradeSignal {
            ticker: "ETH".into(),
            side: TradeSide::Buy,
            price: 3500.0,
            quantity: 10.0,
            confidence: 0.95,
            timestamp_ns: ts_now(),
        };
        match engine.process_signal(signal) {
            ExecutionDecision::Executed {
                quantity,
                price,
                sizing,
                ..
            } => {
                assert!(quantity * price <= 5.0);
                assert!(quantity * price > 4.9);
                assert_eq!(quantity, sizing.position_units);
                assert_eq!(quantity, engine.ghost_wallet.balance("ETH") as f64);
            }
            other => panic!("expected capped execution, got {other:?}"),
        }
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
            ticker: "TESTSYM".into(),
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
                assert!(engine.ghost_wallet.balance("TESTSYM") > 0.0);
            }
            other => panic!("expected Executed, got {other:?}"),
        }
    }

    #[test]
    fn test_json_side_lowercase() {
        let json = r#"{"ticker":"TESTSYM","side":"buy","price":1.0,"quantity":1.0,"confidence":0.9,"timestamp_ns":0}"#;
        let sig: TradeSignal = serde_json::from_str(json).unwrap();
        assert_eq!(sig.side, TradeSide::Buy);
    }

    fn signal(price: f64, side: TradeSide, confidence: f32) -> TradeSignal {
        TradeSignal {
            ticker: "TESTSYM".into(),
            side,
            price,
            quantity: 100_000.0,
            confidence,
            timestamp_ns: ts_now(),
        }
    }

    #[test]
    fn cap_applies_to_wallet_audit_and_decision_at_different_prices_and_edges() {
        for price in [0.01, 1.0, 3500.0] {
            for confidence in [0.2, 0.4, 0.98, 1.0] {
                let dir = tempfile::tempdir().unwrap();
                let log = dir.path().join("fills.jsonl");
                let mut engine = ExecutionEngine::new(0.15, 5.0)
                    .with_sizer(PositionSizer::new(500.0, 5.0, 0.25))
                    .with_log_path(log.to_string_lossy().into_owned());
                for side in [
                    TradeSide::Buy,
                    TradeSide::Sell,
                    TradeSide::Buy,
                    TradeSide::Sell,
                ] {
                    let before_qty = engine.ghost_wallet.balance("TESTSYM") as f64;
                    let before_atp = engine.ghost_wallet.balance_atp as f64;
                    match engine.process_signal(signal(price, side, confidence)) {
                        ExecutionDecision::Executed {
                            quantity, sizing, ..
                        } => {
                            assert!(quantity > 0.0 && quantity * price <= 5.0);
                            assert_eq!(quantity, sizing.position_units);
                            let delta =
                                (engine.ghost_wallet.balance("TESTSYM") as f64 - before_qty).abs();
                            assert!((delta - quantity).abs() < quantity * 1e-6);
                            if side == TradeSide::Buy {
                                assert!(before_atp - engine.ghost_wallet.balance_atp as f64 <= 5.0);
                            }
                        }
                        other => panic!("expected capped execution, got {other:?}"),
                    }
                }
                let fills = std::fs::read_to_string(log).unwrap();
                assert_eq!(fills.lines().count(), 4);
                for line in fills.lines() {
                    let fill: serde_json::Value = serde_json::from_str(line).unwrap();
                    assert!(fill["trade_value_usdt"].as_f64().unwrap() <= 5.0);
                    assert!(fill["quantity"].as_f64().unwrap() * price <= 5.0);
                }
                assert_eq!(engine.ghost_wallet.trade_count, 4);
                assert_eq!(engine.ghost_wallet.closed_trade_count, 2);
                let realized: f32 = engine.ghost_wallet.realized_pnls.values().sum();
                assert!((realized - engine.ghost_wallet.cumulative_pnl).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn invalid_sizes_prices_and_unfillable_orders_never_report_executed() {
        let mut engine = ExecutionEngine::default();
        for price in [
            0.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::MAX,
            f64::MIN_POSITIVE,
        ] {
            assert!(matches!(
                engine.process_signal(signal(price, TradeSide::Buy, 1.0)),
                ExecutionDecision::RejectedPositionLimit { .. }
            ));
        }
        for confidence in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            assert!(matches!(
                engine.process_signal(signal(1.0, TradeSide::Buy, confidence)),
                ExecutionDecision::RejectedLowConfidence { .. }
            ));
        }
        assert!(matches!(
            engine.process_signal(signal(1.0, TradeSide::Sell, 1.0)),
            ExecutionDecision::RejectedPositionLimit { .. }
        ));
        engine.ghost_wallet.balance_atp = 0.0;
        assert!(matches!(
            engine.process_signal(signal(1.0, TradeSide::Buy, 1.0)),
            ExecutionDecision::RejectedNoEdge { .. }
        ));
        assert_eq!(engine.ghost_wallet.trade_count, 0);
        assert_eq!(engine.confidence_stats.executed, 0);
    }

    #[test]
    fn test_neutral() {
        let mut engine = ExecutionEngine::new(0.1, 10.0);
        let signal = TradeSignal {
            ticker: "TESTSYM".into(),
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
