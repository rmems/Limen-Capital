//! `[llm]` section of [`LiveConfig`](super::LiveConfig): Claude model roles,
//! the price table, the cumulative experiment budget, the credit-expiry
//! cutoff, and per-call bounds.
//!
//! Everything here is deterministic configuration. Neither model sees these
//! values; they bound what the Rust side is willing to dispatch and pay for.

use super::{is_loopback_url, ConfigError};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Models allowed to produce executable `Decision` proposals.
pub const DECISION_MODELS: &[&str] = &["claude-sonnet-5-5"];
/// Models allowed in the optional, non-executable helper role.
pub const HELPER_MODELS: &[&str] = &["claude-haiku-5-5"];
/// Hard ceiling for `llm.max_llm_cost_usd_total` (whole experiment, all roles).
pub const MAX_EXPERIMENT_BUDGET_USD: f64 = 200.0;
/// Prompts at or above this size would hit long-context pricing tiers.
pub const MAX_PROMPT_TOKENS: u64 = 100_000;
pub const MAX_OUTPUT_TOKENS: u32 = 64_000;
pub const DEFAULT_API_BASE_URL: &str = "https://api.anthropic.com";

/// Last calendar date (in the operator's configured offset) on which the
/// experiment's API credits may be used.
pub fn credit_last_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 12).expect("valid date")
}

/// USD per million tokens for one model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    pub cache_read_usd_per_mtok: f64,
    pub cache_write_usd_per_mtok: f64,
}

impl ModelPrice {
    fn validate(&self, model: &str) -> Result<(), ConfigError> {
        for (name, v, strictly_positive) in [
            ("input_usd_per_mtok", self.input_usd_per_mtok, true),
            ("output_usd_per_mtok", self.output_usd_per_mtok, true),
            (
                "cache_read_usd_per_mtok",
                self.cache_read_usd_per_mtok,
                false,
            ),
            (
                "cache_write_usd_per_mtok",
                self.cache_write_usd_per_mtok,
                false,
            ),
        ] {
            let ok = v.is_finite() && if strictly_positive { v > 0.0 } else { v >= 0.0 };
            if !ok {
                return Err(ConfigError::Invalid(format!(
                    "llm.pricing.{model:?}.{name} must be a finite {} number, got {v}",
                    if strictly_positive {
                        "positive"
                    } else {
                        "non-negative"
                    }
                )));
            }
        }
        Ok(())
    }

    pub fn cost_usd(&self, input: u64, output: u64, cache_read: u64, cache_write: u64) -> f64 {
        (input as f64 * self.input_usd_per_mtok
            + output as f64 * self.output_usd_per_mtok
            + cache_read as f64 * self.cache_read_usd_per_mtok
            + cache_write as f64 * self.cache_write_usd_per_mtok)
            / 1e6
    }

    /// Upper bound for a call with at most `input_tokens` prompt tokens (each
    /// billed at the dearest input-side rate) and `output_tokens` output tokens.
    pub fn max_charge_usd(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        let in_rate = self
            .input_usd_per_mtok
            .max(self.cache_write_usd_per_mtok)
            .max(self.cache_read_usd_per_mtok);
        (input_tokens as f64 * in_rate + output_tokens as f64 * self.output_usd_per_mtok) / 1e6
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LlmConfig {
    /// Messages API base URL. Plain `http://` only for loopback mocks.
    pub api_base_url: String,
    /// Durable spend ledger (cumulative spend, reservations, day counters, expiry).
    pub ledger_path: String,
    /// Cumulative Claude spend allowed for the whole experiment, all roles,
    /// retries and cache charges included. Required; `0 < x <= 200`.
    pub max_llm_cost_usd_total: Option<f64>,
    /// Provider credit-expiry cutoff, RFC 3339 with an explicit offset, on or
    /// before 2026-11-12 in that offset. Required; no paid call starts at or
    /// after it.
    pub credit_expiry: Option<String>,
    /// Helper-role share of the cumulative budget (required when a helper is
    /// enabled). Carved out of the total, so it never eats the decider's share.
    pub helper_max_cost_usd_total: Option<f64>,
    /// Helper-role share of the daily pacing cap (required when enabled).
    pub helper_max_cost_usd_per_day: Option<f64>,
    /// Per-call prompt bound (request bytes plus API overhead), `<= 100_000`.
    pub max_input_tokens: u64,
    /// `max_tokens` for the decision call. Medium effort thinks inside this
    /// budget, so it is deliberately generous.
    pub decider_max_output_tokens: u32,
    pub helper_max_output_tokens: u32,
    /// Per-attempt HTTP timeout.
    pub request_timeout_secs: u64,
    /// A decision older than this (measured from the quote) is unusable.
    pub decision_deadline_secs: u64,
    pub max_response_bytes: usize,
    /// Extra attempts after a 429/5xx/connect failure. Each one reserves budget.
    pub max_retries: u32,
    /// First backoff; doubles per retry.
    pub retry_backoff_ms: u64,
    /// USD per MTok by model id.
    pub pricing: BTreeMap<String, ModelPrice>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_base_url: DEFAULT_API_BASE_URL.to_string(),
            ledger_path: "llm_budget.json".to_string(),
            max_llm_cost_usd_total: None,
            credit_expiry: None,
            helper_max_cost_usd_total: None,
            helper_max_cost_usd_per_day: None,
            max_input_tokens: 20_000,
            decider_max_output_tokens: 8_192,
            helper_max_output_tokens: 1_024,
            request_timeout_secs: 90,
            decision_deadline_secs: 180,
            max_response_bytes: 512 * 1024,
            max_retries: 1,
            retry_backoff_ms: 1_000,
            pricing: BTreeMap::new(),
        }
    }
}

fn invalid(msg: impl Into<String>) -> ConfigError {
    ConfigError::Invalid(msg.into())
}

fn positive(name: &str, v: Option<f64>) -> Result<f64, ConfigError> {
    match v {
        Some(v) if v.is_finite() && v > 0.0 => Ok(v),
        Some(v) => Err(invalid(format!(
            "{name} must be a positive finite number, got {v}"
        ))),
        None => Err(invalid(format!("{name} is required"))),
    }
}

impl LlmConfig {
    /// Validated cumulative cap.
    pub fn budget_total_usd(&self) -> Result<f64, ConfigError> {
        let total = positive("llm.max_llm_cost_usd_total", self.max_llm_cost_usd_total)?;
        if total > MAX_EXPERIMENT_BUDGET_USD {
            return Err(invalid(format!(
                "llm.max_llm_cost_usd_total {total} exceeds the experiment ceiling of ${MAX_EXPERIMENT_BUDGET_USD}"
            )));
        }
        Ok(total)
    }

    /// Validated credit-expiry cutoff.
    pub fn credit_expiry_utc(&self) -> Result<DateTime<Utc>, ConfigError> {
        let raw = self
            .credit_expiry
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                invalid(
                    "llm.credit_expiry is required (RFC 3339 with an explicit offset, e.g. \"2026-11-12T23:59:59-08:00\")",
                )
            })?;
        let ts = DateTime::parse_from_rfc3339(raw).map_err(|e| {
            invalid(format!(
                "llm.credit_expiry {raw:?} must be RFC 3339 with an explicit offset: {e}"
            ))
        })?;
        if ts.date_naive() > credit_last_date() {
            return Err(invalid(format!(
                "llm.credit_expiry {raw:?} is after {}: the credits expire on that date",
                credit_last_date()
            )));
        }
        Ok(ts.with_timezone(&Utc))
    }

    pub fn helper_caps(&self) -> Result<(f64, f64), ConfigError> {
        Ok((
            positive(
                "llm.helper_max_cost_usd_total",
                self.helper_max_cost_usd_total,
            )?,
            positive(
                "llm.helper_max_cost_usd_per_day",
                self.helper_max_cost_usd_per_day,
            )?,
        ))
    }

    pub fn price(&self, model: &str) -> Result<&ModelPrice, ConfigError> {
        let p = self
            .pricing
            .get(model)
            .ok_or_else(|| invalid(format!("llm.pricing has no entry for {model:?}")))?;
        p.validate(model)?;
        Ok(p)
    }

    /// Checks for `decider = "claude"`. `per_day_usd` is the daily pacing
    /// sub-cap (`risk.max_llm_cost_usd_per_day`).
    pub(super) fn validate(
        &self,
        model: &str,
        helper_model: Option<&str>,
        per_day_usd: f64,
    ) -> Result<(), ConfigError> {
        self.validate_endpoint()?;
        let total = self.budget_total_usd()?;
        if per_day_usd >= total {
            return Err(invalid(format!(
                "risk.max_llm_cost_usd_per_day ({per_day_usd}) is a pacing sub-cap and must be smaller than llm.max_llm_cost_usd_total ({total})"
            )));
        }
        self.credit_expiry_utc()?;
        self.validate_bounds()?;
        self.price(model)?;
        if let Some(helper) = helper_model {
            self.price(helper)?;
            let (h_total, h_day) = self.helper_caps()?;
            if h_total >= total {
                return Err(invalid(
                    "llm.helper_max_cost_usd_total must be smaller than llm.max_llm_cost_usd_total",
                ));
            }
            if h_day >= per_day_usd {
                return Err(invalid(
                    "llm.helper_max_cost_usd_per_day must be smaller than risk.max_llm_cost_usd_per_day",
                ));
            }
        }
        if self.ledger_path.trim().is_empty() {
            return Err(invalid("llm.ledger_path must not be empty"));
        }
        Ok(())
    }

    fn validate_endpoint(&self) -> Result<(), ConfigError> {
        let url = self.api_base_url.as_str();
        if url.starts_with("https://") {
            return Ok(());
        }
        if url.starts_with("http://") && is_loopback_url(url) {
            return Ok(());
        }
        Err(invalid(format!(
            "llm.api_base_url must be https:// (plain http:// only for loopback mocks), got {url:?}"
        )))
    }

    fn validate_bounds(&self) -> Result<(), ConfigError> {
        if !(1..=MAX_PROMPT_TOKENS).contains(&self.max_input_tokens) {
            return Err(invalid(format!(
                "llm.max_input_tokens must be in 1..={MAX_PROMPT_TOKENS}"
            )));
        }
        for (name, v) in [
            (
                "llm.decider_max_output_tokens",
                self.decider_max_output_tokens,
            ),
            (
                "llm.helper_max_output_tokens",
                self.helper_max_output_tokens,
            ),
        ] {
            if !(1..=MAX_OUTPUT_TOKENS).contains(&v) {
                return Err(invalid(format!(
                    "{name} must be in 1..={MAX_OUTPUT_TOKENS}"
                )));
            }
        }
        if !(1..=600).contains(&self.request_timeout_secs) {
            return Err(invalid("llm.request_timeout_secs must be in 1..=600"));
        }
        if !(1..=3_600).contains(&self.decision_deadline_secs) {
            return Err(invalid("llm.decision_deadline_secs must be in 1..=3600"));
        }
        if !(1_024..=8 * 1024 * 1024).contains(&self.max_response_bytes) {
            return Err(invalid("llm.max_response_bytes must be in 1 KiB..=8 MiB"));
        }
        if self.max_retries > 3 {
            return Err(invalid("llm.max_retries must be <= 3"));
        }
        if self.retry_backoff_ms > 30_000 {
            return Err(invalid("llm.retry_backoff_ms must be <= 30000"));
        }
        Ok(())
    }
}
