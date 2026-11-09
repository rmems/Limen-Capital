//! Decision contract. Deciders are **advisory**: they propose an action and a
//! size; `risk::check` owns execution and may reject or shrink nothing — it
//! only approves or rejects.

pub mod rules;

use crate::live::market_state::MarketState;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::future::Future;
use std::pin::Pin;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Buy,
    Sell,
    Hold,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Buy => "buy",
            Action::Sell => "sell",
            Action::Hold => "hold",
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a decider proposes for one tick.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub action: Action,
    /// Notional in USD; ignored for `Hold`.
    #[serde(default)]
    pub size_usd: f64,
    #[serde(default)]
    pub reason: String,
}

impl Decision {
    pub fn hold(reason: impl Into<String>) -> Self {
        Self {
            action: Action::Hold,
            size_usd: 0.0,
            reason: reason.into(),
        }
    }

    pub fn buy(size_usd: f64, reason: impl Into<String>) -> Self {
        Self {
            action: Action::Buy,
            size_usd,
            reason: reason.into(),
        }
    }

    pub fn sell(size_usd: f64, reason: impl Into<String>) -> Self {
        Self {
            action: Action::Sell,
            size_usd,
            reason: reason.into(),
        }
    }

    pub fn is_hold(&self) -> bool {
        self.action == Action::Hold
    }
}

#[derive(Debug)]
pub enum DeciderError {
    /// Transport / venue failure; the loop falls back to the rules decider.
    Http(String),
    /// The model answered but not in the agreed schema.
    Parse {
        raw: String,
        detail: String,
    },
    /// Per-day LLM budget reached; the loop idles.
    BudgetExhausted {
        spent_usd: f64,
        cap_usd: f64,
    },
    Other(String),
}

impl fmt::Display for DeciderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeciderError::Http(e) => write!(f, "decider http: {e}"),
            DeciderError::Parse { detail, .. } => write!(f, "decider parse_error: {detail}"),
            DeciderError::BudgetExhausted { spent_usd, cap_usd } => write!(
                f,
                "llm budget exhausted: spent ${spent_usd:.4} of ${cap_usd:.2} today"
            ),
            DeciderError::Other(e) => write!(f, "decider: {e}"),
        }
    }
}

impl std::error::Error for DeciderError {}

/// Boxed future so the trait stays object-safe without an `async_trait` dependency.
pub type DecideFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Decision, DeciderError>> + Send + 'a>>;

/// A source of advisory decisions.
pub trait Decider: Send {
    fn name(&self) -> &str;

    /// Propose an action for `state`. `recent_journal` holds the last few
    /// journal rows (markdown) for context.
    fn decide<'a>(
        &'a mut self,
        state: &'a MarketState,
        recent_journal: &'a [String],
    ) -> DecideFuture<'a>;

    /// Estimated LLM spend so far this UTC day, in USD (0 for non-LLM deciders).
    fn llm_cost_today_usd(&self) -> f64 {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_json_roundtrip_and_defaults() {
        let d: Decision =
            serde_json::from_str(r#"{"action":"buy","size_usd":2.5,"reason":"dip"}"#).unwrap();
        assert_eq!(d, Decision::buy(2.5, "dip"));
        let d: Decision = serde_json::from_str(r#"{"action":"hold"}"#).unwrap();
        assert!(d.is_hold());
        assert_eq!(d.size_usd, 0.0);
        assert!(serde_json::from_str::<Decision>(r#"{"action":"short"}"#).is_err());
    }
}
