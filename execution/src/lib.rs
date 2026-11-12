//! Limen-Capital Execution Engine — Rust Muscle
//!
//! Deep **Decision** path:
//! ```text
//! TradeSignal → confidence gate → Neutral filter
//!   → fractional Kelly size (PositionSizer)
//!   → max-position gate → metabolic-ledger caller-sized ghost fill → JSONL
//! ```
//!
//! PositionSizer owns confidence→quantity. The ledger applies that quantity
//! (rounded to its legacy f32 units) and explicit simulated friction once.

pub mod binary_wire;
pub mod dydx;
pub mod kelly;
/// Live micro-capital experiment (RM-2436): Robinhood Crypto adapter + decision loop.
/// Feature-gated; **not** part of the research path and not a research result.
#[cfg(feature = "live-robinhood")]
pub mod live;
pub mod universe;
pub mod wire;
pub mod zmq_curve;

use metabolic_ledger::{apply_ghost_fill, GhostFill, Side, METABOLIC_COST};
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
    /// Maximum fill quantity and total long holdings per asset.
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

        if !signal.price.is_finite()
            || signal.price <= 0.0
            || !signal.confidence.is_finite()
            || !(0.0..=1.0).contains(&signal.confidence)
            || !self.max_position_size.is_finite()
            || self.max_position_size <= 0.0
        {
            return ExecutionDecision::RejectedFill {
                reason: "invalid price, confidence, or position limit".into(),
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

        if !quantity.is_finite() || quantity <= 0.0 {
            return ExecutionDecision::RejectedNoEdge {
                reason: "position size is zero".into(),
                latency_ns: latency,
                kelly_fraction: sizing.kelly_fraction,
            };
        }

        // The legacy ghost ledger uses f32. Gate the actual fill as well as
        // policy size so rounding cannot bypass the limit.
        let fill_quantity = quantity as f32;
        let fill_price = signal.price as f32;
        if !fill_quantity.is_finite()
            || fill_quantity <= 0.0
            || !fill_price.is_finite()
            || fill_price <= 0.0
        {
            return ExecutionDecision::RejectedFill {
                reason: "price or quantity is outside ghost ledger precision".into(),
                latency_ns: latency,
            };
        }
        let projected_holdings = if signal.side == TradeSide::Buy {
            // Match the ledger's f32 inventory addition.
            (self.ghost_wallet.balance(&signal.ticker) + fill_quantity) as f64
        } else {
            0.0
        };
        if quantity > self.max_position_size
            || fill_quantity as f64 > self.max_position_size
            || projected_holdings > self.max_position_size
        {
            warn!(
                "Signal rejected: size {} > max {}",
                quantity, self.max_position_size
            );
            return ExecutionDecision::RejectedPositionLimit {
                reason: format!(
                    "Quantity {} or resulting holdings {} exceeds max {}",
                    quantity, projected_holdings, self.max_position_size
                ),
                latency_ns: latency,
            };
        }

        let Some(step) = self.step_counter.checked_add(1) else {
            return ExecutionDecision::RejectedFill {
                reason: "execution step counter exhausted".into(),
                latency_ns: latency,
            };
        };
        self.step_counter = step;
        let reason = format!(
            "snn_conf={:.3};kelly_frac={:.4};policy_qty={:.6};tier={:?}",
            signal.confidence, sizing.kelly_fraction, quantity, sizing.risk_tier
        );

        let trades_before = self.ghost_wallet.trade_count;
        let fill = GhostFill {
            asset: &signal.ticker,
            side: match signal.side {
                TradeSide::Buy => Side::Buy,
                TradeSide::Sell => Side::Sell,
                TradeSide::Neutral => unreachable!("neutral signals return before sizing"),
            },
            price: fill_price,
            quantity: fill_quantity,
            fee: fill_price * fill_quantity * METABOLIC_COST,
            step,
            reason: &reason,
        };
        let record = match apply_ghost_fill(&mut self.ghost_wallet, fill, Some(&self.log_path)) {
            Ok(record) if self.ghost_wallet.trade_count > trades_before => record,
            Ok(_) => {
                return ExecutionDecision::RejectedFill {
                    reason: "ledger did not commit a fill".into(),
                    latency_ns: latency,
                }
            }
            Err(err) => {
                warn!("[{}] Fill rejected: {}", signal.ticker, err);
                return ExecutionDecision::RejectedFill {
                    reason: err.to_string(),
                    latency_ns: latency,
                };
            }
        };
        let quantity = record.quantity as f64;
        let price = record.price_usd as f64;

        info!(
            "[{}] EXECUTE {:?} @ ${:.4} qty≈{:.6} kelly={:.3} conf={:.2} lat={}ns ATP={:.2}",
            signal.ticker,
            signal.side,
            price,
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
            price,
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
    /// No fill committed, including insufficient holdings/funds or log I/O failure.
    RejectedFill {
        reason: String,
        latency_ns: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_engine() -> (tempfile::TempDir, ExecutionEngine) {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("ghost_trades.jsonl");
        let engine = ExecutionEngine::default().with_log_path(log.to_str().unwrap().into());
        (dir, engine)
    }

    fn assert_no_fill(engine: &ExecutionEngine) {
        assert_eq!(engine.ghost_wallet.balance_atp, 500.0);
        assert!(engine.ghost_wallet.balances.is_empty());
        assert_eq!(engine.ghost_wallet.trade_count, 0);
        assert_eq!(engine.step_counter, 0);
        assert!(!std::path::Path::new(&engine.log_path).exists());
    }

    fn ts_now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64
    }

    #[test]
    fn test_confidence_gate() {
        let (_logs, mut engine) = test_engine();
        let signal = TradeSignal {
            ticker: "TESTSYM".into(),
            side: TradeSide::Buy,
            price: 1.0,
            quantity: 1.0,
            confidence: 0.10,
            timestamp_ns: ts_now(),
        };
        assert!(matches!(
            engine.process_signal(signal),
            ExecutionDecision::RejectedLowConfidence { .. }
        ));
        assert_no_fill(&engine);
    }

    #[test]
    fn test_kelly_no_edge() {
        // conf too low for b=0.05 → Kelly 0
        let (_logs, mut engine) = test_engine();
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
        assert_no_fill(&engine);
    }

    #[test]
    fn test_position_limit_without_kelly() {
        let (_logs, mut engine) = test_engine();
        engine.use_kelly_sizing = false;
        let signal = TradeSignal {
            ticker: "ETH".into(),
            side: TradeSide::Buy,
            price: 3500.0,
            quantity: 11.0,
            confidence: 0.95,
            timestamp_ns: ts_now(),
        };
        assert!(matches!(
            engine.process_signal(signal),
            ExecutionDecision::RejectedPositionLimit { .. }
        ));
        assert_no_fill(&engine);
    }

    #[test]
    fn test_default_kelly_position_limit_at_low_price() {
        let (_logs, mut engine) = test_engine();
        let signal = TradeSignal {
            ticker: "TESTSYM".into(),
            side: TradeSide::Buy,
            price: 1.0,
            quantity: 0.0,
            confidence: 0.98,
            timestamp_ns: ts_now(),
        };
        // Policy size ≈72.5 units exceeds the production max of 10.
        assert!(matches!(
            engine.process_signal(signal),
            ExecutionDecision::RejectedPositionLimit { .. }
        ));
        assert_no_fill(&engine);
    }

    #[test]
    fn test_execution_with_kelly() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("ghost_trades.jsonl");
        let mut engine = ExecutionEngine::default().with_log_path(log.to_str().unwrap().into());
        let signal = TradeSignal {
            ticker: "TESTSYM".into(),
            side: TradeSide::Buy,
            price: 10.0,
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
                assert!((quantity - 7.25).abs() < 1e-5);
                assert!((kelly_fraction - 0.145).abs() < 1e-6);
                assert!((engine.ghost_wallet.balance("TESTSYM") - 7.2500052).abs() < 1e-6);
                assert!((engine.ghost_wallet.balance_atp - 427.42743).abs() < 1e-4);
                assert_eq!(engine.ghost_wallet.trade_count, 1);
                assert_eq!(engine.step_counter, 1);
            }
            other => panic!("expected Executed, got {other:?}"),
        }

        let sell = TradeSignal {
            ticker: "TESTSYM".into(),
            side: TradeSide::Sell,
            price: 12.0,
            quantity: 999.0, // ignored by policy sizing
            confidence: 0.98,
            timestamp_ns: ts_now(),
        };
        match engine.process_signal(sell) {
            ExecutionDecision::Executed { side, quantity, .. } => {
                assert_eq!(side, TradeSide::Sell);
                assert!((quantity - 5.1647515).abs() < 1e-5);
            }
            other => panic!("expected Sell execution, got {other:?}"),
        }
        // Pin real ledger deltas, not just the advisory policy quantity.
        assert!((engine.ghost_wallet.balance("TESTSYM") - 2.0852537).abs() < 1e-5);
        assert!((engine.ghost_wallet.balance_atp - 489.34247).abs() < 1e-3);
        assert!((engine.ghost_wallet.cumulative_pnl - 10.215877).abs() < 1e-4);
        assert_eq!(engine.ghost_wallet.trade_count, 2);
        assert_eq!(engine.ghost_wallet.closed_trade_count, 1);
        assert_eq!(engine.step_counter, 2);
        let records: Vec<serde_json::Value> = std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["action"], "buy");
        assert_eq!(records[1]["action"], "sell");
        assert!((records[0]["quantity"].as_f64().unwrap() - 7.2500052).abs() < 1e-5);
        assert!((records[1]["quantity"].as_f64().unwrap() - 5.1647515).abs() < 1e-5);
    }

    #[test]
    fn test_json_side_lowercase() {
        let json = r#"{"ticker":"TESTSYM","side":"buy","price":1.0,"quantity":1.0,"confidence":0.9,"timestamp_ns":0}"#;
        let sig: TradeSignal = serde_json::from_str(json).unwrap();
        assert_eq!(sig.side, TradeSide::Buy);
    }

    #[test]
    fn test_neutral() {
        let (_logs, mut engine) = test_engine();
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
        assert_no_fill(&engine);
    }
}
