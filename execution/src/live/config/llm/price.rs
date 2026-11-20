//! Per-model token pricing for `[llm.pricing]`: four rate dimensions plus an
//! optional long-context tier.

use super::super::ConfigError;
use super::invalid;
use serde::{Deserialize, Serialize};

/// Token counts for one billed call, in the four price dimensions.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenCounts {
    /// Prompt-side tokens: everything billed at an input-family rate.
    pub fn prompt_tokens(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
}

/// Long-context price tier: once the prompt reaches `threshold_tokens`, these
/// rates replace the base rates for the whole call.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LongContextPrice {
    pub threshold_tokens: u64,
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    pub cache_read_usd_per_mtok: f64,
    pub cache_write_usd_per_mtok: f64,
}

/// USD per million tokens for one model, optionally with a long-context tier.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    pub cache_read_usd_per_mtok: f64,
    pub cache_write_usd_per_mtok: f64,
    /// Rates applied when `prompt_tokens >= threshold_tokens`. Absent means a
    /// single tier covers the model's whole context window.
    #[serde(default)]
    pub long_context: Option<LongContextPrice>,
}

/// The four per-MTok rates selected for a given prompt size.
#[derive(Debug, Clone, Copy)]
struct Rates {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

impl ModelPrice {
    fn base_rates(&self) -> Rates {
        Rates {
            input: self.input_usd_per_mtok,
            output: self.output_usd_per_mtok,
            cache_read: self.cache_read_usd_per_mtok,
            cache_write: self.cache_write_usd_per_mtok,
        }
    }

    fn rates_for(&self, prompt_tokens: u64) -> Rates {
        match &self.long_context {
            Some(lc) if prompt_tokens >= lc.threshold_tokens => Rates {
                input: lc.input_usd_per_mtok,
                output: lc.output_usd_per_mtok,
                cache_read: lc.cache_read_usd_per_mtok,
                cache_write: lc.cache_write_usd_per_mtok,
            },
            _ => self.base_rates(),
        }
    }

    pub(super) fn validate(&self, model: &str) -> Result<(), ConfigError> {
        self.validate_rate_set(model, "", self.base_rates())?;
        if let Some(lc) = &self.long_context {
            if lc.threshold_tokens == 0 {
                return Err(invalid(format!(
                    "llm.pricing.{model:?}.long_context.threshold_tokens must be > 0"
                )));
            }
            self.validate_rate_set(
                model,
                "long_context.",
                Rates {
                    input: lc.input_usd_per_mtok,
                    output: lc.output_usd_per_mtok,
                    cache_read: lc.cache_read_usd_per_mtok,
                    cache_write: lc.cache_write_usd_per_mtok,
                },
            )?;
        }
        Ok(())
    }

    fn validate_rate_set(&self, model: &str, prefix: &str, r: Rates) -> Result<(), ConfigError> {
        for (name, v, strictly_positive) in [
            ("input_usd_per_mtok", r.input, true),
            ("output_usd_per_mtok", r.output, true),
            ("cache_read_usd_per_mtok", r.cache_read, false),
            ("cache_write_usd_per_mtok", r.cache_write, false),
        ] {
            let ok = rate_ok(v, strictly_positive);
            if !ok {
                return Err(invalid(format!(
                    "llm.pricing.{model:?}.{prefix}{name} must be a finite {} number, got {v}",
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

    /// Actual charge for `tokens` at the tier their prompt size selects.
    pub fn cost_usd(&self, tokens: &TokenCounts) -> f64 {
        let r = self.rates_for(tokens.prompt_tokens());
        (tokens.input as f64 * r.input
            + tokens.output as f64 * r.output
            + tokens.cache_read as f64 * r.cache_read
            + tokens.cache_write as f64 * r.cache_write)
            / 1e6
    }

    /// Upper bound for a call with at most `input_tokens` prompt tokens (each
    /// billed at the dearest input-side rate across tiers) and `output_tokens`
    /// output tokens (dearest output rate across tiers).
    pub fn max_charge_usd(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        let base = self.base_rates();
        let worst = match &self.long_context {
            Some(lc) => Rates {
                input: base.input.max(lc.input_usd_per_mtok),
                output: base.output.max(lc.output_usd_per_mtok),
                cache_read: base.cache_read.max(lc.cache_read_usd_per_mtok),
                cache_write: base.cache_write.max(lc.cache_write_usd_per_mtok),
            },
            None => base,
        };
        let in_rate = worst.input.max(worst.cache_write).max(worst.cache_read);
        (input_tokens as f64 * in_rate + output_tokens as f64 * worst.output) / 1e6
    }
}

fn rate_ok(v: f64, strictly_positive: bool) -> bool {
    v.is_finite() && if strictly_positive { v > 0.0 } else { v >= 0.0 }
}
