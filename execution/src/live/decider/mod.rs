//! Decision contract. Deciders are **advisory**: they propose an action and a
//! size; `risk::check` owns execution and only approves or rejects.
//!
//! A decider error never falls back to another decider or model: the loop
//! journals the error and holds that tick. Protective handling (stale-order
//! cancels, the daily-loss halt) runs in the loop independent of any decider.

pub mod budget;
pub mod claude;
pub mod helper;
pub mod llm;
pub mod rules;

use crate::live::market_state::MarketState;
use budget::{BudgetError, Role};
use llm::Usage;
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
    /// Transport failure or API error status; the loop holds this tick.
    Http(String),
    /// No response within the per-attempt timeout.
    Timeout {
        after_ms: u64,
    },
    /// The model answered but not with exactly one valid decision tool call.
    Parse {
        raw: String,
        detail: String,
    },
    /// The model refused the request (`stop_reason: "refusal"`). Never
    /// corrected or retried through a fallback model.
    Refusal(String),
    /// The decision would act on a quote older than the freshness deadline.
    Stale {
        detail: String,
    },
    /// Required decision context is missing or cannot be assembled within
    /// configured limits; no paid call was made.
    Context(String),
    /// A 200 response without usable `usage`; paid calls halt.
    MissingUsage(String),
    /// No configured price for the model or tier; paid calls halt.
    Pricing(String),
    /// Prompt above `llm.max_input_tokens`; nothing was sent.
    PromptTooLarge {
        estimated_tokens: u64,
        limit: u64,
    },
    /// Budget, expiry, halt, or ledger persistence refused the call.
    Budget(BudgetError),
    Other(String),
}

impl DeciderError {
    /// Short stable tag for the journal.
    pub fn kind(&self) -> &'static str {
        match self {
            DeciderError::Http(_) => "http",
            DeciderError::Timeout { .. } => "timeout",
            DeciderError::Parse { .. } => "parse",
            DeciderError::Refusal(_) => "refusal",
            DeciderError::Stale { .. } => "stale",
            DeciderError::Context(_) => "context_error",
            _ => self.tail_kind(),
        }
    }

    fn tail_kind(&self) -> &'static str {
        match self {
            DeciderError::MissingUsage(_) => "missing_usage",
            DeciderError::Pricing(_) => "missing_pricing",
            DeciderError::PromptTooLarge { .. } => "prompt_too_large",
            DeciderError::Budget(BudgetError::Expired { .. }) => "expired",
            DeciderError::Budget(_) => "budget",
            DeciderError::Other(_) => "other",
            _ => unreachable!("kind() handled the other variants"),
        }
    }
}

impl fmt::Display for DeciderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeciderError::Http(e) => write!(f, "decider http: {e}"),
            DeciderError::Timeout { after_ms } => write!(f, "decider timeout after {after_ms} ms"),
            DeciderError::Parse { detail, .. } => write!(f, "decider parse_error: {detail}"),
            DeciderError::Refusal(m) => write!(f, "decider refusal: {m}"),
            DeciderError::Stale { detail } => write!(f, "decision stale: {detail}"),
            DeciderError::Context(m) => write!(f, "decision context: {m}"),
            DeciderError::MissingUsage(m) => write!(f, "missing usage: {m}"),
            DeciderError::Pricing(m) => write!(f, "missing pricing: {m}"),
            _ => self.fmt_tail(f),
        }
    }
}

impl DeciderError {
    fn fmt_tail(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeciderError::PromptTooLarge {
                estimated_tokens,
                limit,
            } => write!(f, "prompt ~{estimated_tokens} tokens > limit {limit}"),
            DeciderError::Budget(e) => write!(f, "llm budget: {e}"),
            DeciderError::Other(e) => write!(f, "decider: {e}"),
            _ => unreachable!("fmt() handled the other variants"),
        }
    }
}

impl std::error::Error for DeciderError {}

/// Audit record for one logical LLM call (retries folded in). Carries no
/// secrets and no thinking content.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmCallReport {
    pub role: Role,
    pub model: String,
    pub attempts: u32,
    pub usage: Usage,
    pub cost_usd: f64,
    pub latency_ms: u64,
    pub outcome: String,
    /// Sanitized helper note (helper role only).
    pub note: Option<String>,
}

impl LlmCallReport {
    pub fn new(role: Role, model: &str) -> Self {
        Self {
            role,
            model: model.to_string(),
            attempts: 0,
            usage: Usage::default(),
            cost_usd: 0.0,
            latency_ms: 0,
            outcome: String::new(),
            note: None,
        }
    }

    pub fn summary(&self) -> String {
        let u = &self.usage;
        let mut s = format!(
            "{} {} {} in={} out={} cache_r={} cache_w={} ${:.6} {}ms x{}",
            self.role,
            self.model,
            self.outcome,
            u.input_tokens,
            u.output_tokens,
            u.cache_read_input_tokens,
            u.cache_creation_input_tokens,
            self.cost_usd,
            self.latency_ms,
            self.attempts
        );
        if let Some(note) = &self.note {
            s.push_str(&format!(" note=\"{note}\""));
        }
        s
    }
}

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

    /// LLM spend (committed plus reserved) this UTC day, in USD (0 for non-LLM deciders).
    fn llm_cost_today_usd(&self) -> f64 {
        0.0
    }

    /// LLM call audit records produced since the last call.
    fn take_llm_reports(&mut self) -> Vec<LlmCallReport> {
        Vec::new()
    }

    /// Graceful shutdown: let in-flight background work (e.g. a helper call)
    /// finish so its reservation settles and its report is audited instead of
    /// being aborted. Default: nothing to flush.
    fn shutdown<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
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
