//! `[llm]` section of [`LiveConfig`](super::LiveConfig): Claude model roles,
//! capability profiles, the price table, the cumulative experiment budget,
//! the credit-expiry cutoff, and per-call bounds.
//!
//! Everything here is deterministic configuration. Neither model sees these
//! values; they bound what the Rust side is willing to dispatch and pay for.
//!
//! Model selection is profile-driven, not name-driven: a model may hold the
//! decision or helper role when its capability [`ModelProfile`] (built-in for
//! the maintained 5.5 defaults, or operator-supplied via
//! `[llm.profiles."<model>"]`) allows that role and a complete price entry
//! exists. Role permissions never come from the model name.

use super::{is_loopback_url, ConfigError};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Role names used inside [`ModelProfile::roles`].
pub const ROLE_DECISION: &str = "decision";
pub const ROLE_HELPER: &str = "helper";
/// Serialization contracts a profile may declare.
pub const CONTRACT_JSON_SCHEMA: &str = "json_schema";
pub const CONTRACT_STRICT_TOOL: &str = "strict_tool";
/// `tool_choice` modes a profile may declare. The decision role always sends
/// `auto` (Sonnet 5.5 rejects forced tool use), so decision profiles must
/// list it.
pub const TOOL_CHOICE_AUTO: &str = "auto";
pub const TOOL_CHOICE_NONE: &str = "none";
pub const TOOL_CHOICE_ANY: &str = "any";
pub const TOOL_CHOICE_TOOL: &str = "tool";
/// Thinking modes. `adaptive` sends no `thinking` field (the model default);
/// `disabled` sends `{"type": "disabled"}`. Manual token budgets are never
/// sent: the 5.5 family rejects them.
pub const THINKING_ADAPTIVE: &str = "adaptive";
pub const THINKING_DISABLED: &str = "disabled";

/// Hard ceiling for `llm.max_llm_cost_usd_total` (whole experiment, all roles).
pub const MAX_EXPERIMENT_BUDGET_USD: f64 = 200.0;
pub const MAX_OUTPUT_TOKENS: u32 = 64_000;
/// Bound on the operator-configured decision-model fallback list.
pub const MAX_FALLBACK_MODELS: usize = 4;
/// Bound on bounded serialization-correction attempts per model call.
pub const MAX_CORRECTIONS: u32 = 3;
pub const DEFAULT_API_BASE_URL: &str = "https://api.anthropic.com";

/// Last calendar date (in the operator's configured offset) on which the
/// experiment's API credits may be used.
pub fn credit_last_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 12).expect("valid date")
}

/// Capability profile for one model: which roles it may serve, its context
/// window, accepted effort levels, serialization contracts, tool-choice modes
/// and thinking mode. Operator-supplied profiles must be complete; a missing
/// or incompatible profile fails validation before any paid call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct ModelProfile {
    /// Roles this model may serve: any of `"decision"`, `"helper"`.
    pub roles: Vec<String>,
    /// Context window in tokens. `llm.max_input_tokens` must fit inside the
    /// smallest window among the configured role models.
    pub context_tokens: u64,
    /// Effort levels the model accepts (e.g. `low`, `medium`, `high`). This is
    /// a per-model capability list, not a global ceiling.
    pub efforts: Vec<String>,
    /// Effort used when the role configuration leaves it unset.
    pub default_effort: String,
    /// Serialization contracts the model supports: `"json_schema"` and/or
    /// `"strict_tool"`.
    pub response_contracts: Vec<String>,
    /// `tool_choice` modes the model accepts: `"auto"`, `"none"`, `"any"`,
    /// `"tool"`. Decision-role profiles must include `"auto"`.
    pub tool_choice: Vec<String>,
    /// Thinking mode: `"adaptive"` or `"disabled"`.
    pub thinking: String,
}

fn one_of(value: &str, allowed: &[&str]) -> bool {
    allowed.contains(&value)
}

impl ModelProfile {
    /// Structural checks every profile must pass, whatever its role.
    pub fn validate(&self, model: &str) -> Result<(), ConfigError> {
        self.validate_roles_and_window(model)?;
        self.validate_capabilities(model)
    }

    fn validate_roles_and_window(&self, model: &str) -> Result<(), ConfigError> {
        if self.roles.is_empty()
            || !self
                .roles
                .iter()
                .all(|r| one_of(r, &[ROLE_DECISION, ROLE_HELPER]))
        {
            return Err(invalid(format!(
                "profile for {model:?}: roles must be a non-empty subset of [\"decision\", \"helper\"]"
            )));
        }
        if self.context_tokens < 4_096 {
            return Err(invalid(format!(
                "profile for {model:?}: context_tokens must be >= 4096, got {}",
                self.context_tokens
            )));
        }
        Ok(())
    }

    fn validate_capabilities(&self, model: &str) -> Result<(), ConfigError> {
        self.validate_efforts(model)?;
        self.validate_contracts(model)?;
        self.validate_tool_choice(model)?;
        self.validate_thinking(model)
    }

    fn validate_efforts(&self, model: &str) -> Result<(), ConfigError> {
        if self.efforts.is_empty() || self.efforts.iter().any(|e| e.is_empty()) {
            return Err(invalid(format!(
                "profile for {model:?}: efforts must be a non-empty list of effort levels"
            )));
        }
        if !self.efforts.iter().any(|e| e == &self.default_effort) {
            return Err(invalid(format!(
                "profile for {model:?}: default_effort {:?} is not in efforts {:?}",
                self.default_effort, self.efforts
            )));
        }
        Ok(())
    }

    fn validate_contracts(&self, model: &str) -> Result<(), ConfigError> {
        if self.response_contracts.is_empty()
            || !self
                .response_contracts
                .iter()
                .all(|c| one_of(c, &[CONTRACT_JSON_SCHEMA, CONTRACT_STRICT_TOOL]))
        {
            return Err(invalid(format!(
                "profile for {model:?}: response_contracts must be a non-empty subset of [\"json_schema\", \"strict_tool\"]"
            )));
        }
        Ok(())
    }

    fn validate_tool_choice(&self, model: &str) -> Result<(), ConfigError> {
        if self.tool_choice.is_empty()
            || !self.tool_choice.iter().all(|t| {
                one_of(
                    t,
                    &[
                        TOOL_CHOICE_AUTO,
                        TOOL_CHOICE_NONE,
                        TOOL_CHOICE_ANY,
                        TOOL_CHOICE_TOOL,
                    ],
                )
            })
        {
            return Err(invalid(format!(
                "profile for {model:?}: tool_choice must be a non-empty subset of [\"auto\", \"none\", \"any\", \"tool\"]"
            )));
        }
        Ok(())
    }

    fn validate_thinking(&self, model: &str) -> Result<(), ConfigError> {
        if !one_of(&self.thinking, &[THINKING_ADAPTIVE, THINKING_DISABLED]) {
            return Err(invalid(format!(
                "profile for {model:?}: thinking must be \"adaptive\" or \"disabled\", got {:?}",
                self.thinking
            )));
        }
        Ok(())
    }

    /// Whether the profile lists `role` in [`ModelProfile::roles`].
    pub fn allows(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

/// Maintained capability profiles for the default 5.5 family. These are the
/// documented contracts: Sonnet 5.5 rejects forced tool use and manual
/// thinking budgets; both use adaptive thinking. An operator profile for the
/// same model id replaces the built-in entry wholesale.
fn builtin_profile(model: &str) -> Option<ModelProfile> {
    let profile = match model {
        // claude-sonnet-5-5: decision role only. Rejects forced tool use
        // (`any`/`tool`) and `thinking: disabled`; adaptive thinking only.
        "claude-sonnet-5-5" => ModelProfile {
            roles: vec![ROLE_DECISION.to_string()],
            context_tokens: 200_000,
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            default_effort: "high".into(),
            response_contracts: vec![CONTRACT_JSON_SCHEMA.into(), CONTRACT_STRICT_TOOL.into()],
            tool_choice: vec![TOOL_CHOICE_AUTO.into(), TOOL_CHOICE_NONE.into()],
            thinking: THINKING_ADAPTIVE.into(),
        },
        // claude-haiku-5.5 helper profile: accepts forced tool choice but the
        // helper contract is plain text, so it is not required. A decision-role
        // Haiku profile must be operator-supplied.
        "claude-haiku-5-5" => ModelProfile {
            roles: vec![ROLE_HELPER.to_string()],
            context_tokens: 200_000,
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            default_effort: "medium".into(),
            response_contracts: vec![CONTRACT_JSON_SCHEMA.into(), CONTRACT_STRICT_TOOL.into()],
            tool_choice: vec![
                TOOL_CHOICE_AUTO.into(),
                TOOL_CHOICE_NONE.into(),
                TOOL_CHOICE_ANY.into(),
                TOOL_CHOICE_TOOL.into(),
            ],
            thinking: THINKING_ADAPTIVE.into(),
        },
        _ => return None,
    };
    Some(profile)
}

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

    fn validate(&self, model: &str) -> Result<(), ConfigError> {
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
    /// Explicit operator pacing: daily Claude spend sub-cap, required for
    /// `decider = "claude"`. Must be `0 < x < max_llm_cost_usd_total`; it is
    /// the single pacing value the ledger and the loop gate share.
    pub max_llm_cost_usd_per_day: Option<f64>,
    /// Provider credit-expiry cutoff, RFC 3339 with an explicit offset, on or
    /// before 2026-11-12 in that offset. Required; no paid call starts at or
    /// after it.
    pub credit_expiry: Option<String>,
    /// Helper-role share of the cumulative budget (required when a helper is
    /// enabled). Carved out of the total, so it never eats the decider's share.
    pub helper_max_cost_usd_total: Option<f64>,
    /// Helper-role share of the daily pacing cap (required when enabled).
    pub helper_max_cost_usd_per_day: Option<f64>,
    /// Optional ordered decision-model fallback candidates, disabled by
    /// default (empty). Each must have a decision-capable profile and a price;
    /// every attempt is separately reserved and risk-gated.
    pub decision_fallback_models: Vec<String>,
    /// `output_config.effort` for the decision role. Must be supported by every
    /// decision-role model profile; unset uses each profile's default (Sonnet:
    /// `high`).
    pub decider_effort: Option<String>,
    /// `output_config.effort` for the helper role; unset uses the helper
    /// profile's default.
    pub helper_effort: Option<String>,
    /// Bounded serialization-correction attempts after a schema/parse
    /// failure, per model call. `0` disables correction.
    pub max_corrections: u32,
    /// Per-call prompt bound (request bytes plus API overhead). Must fit the
    /// smallest context window among the configured role models; there is no
    /// blanket ceiling — context and pricing tiers are per model.
    pub max_input_tokens: u64,
    /// `max_tokens` for the decision call. Effort thinks inside this budget,
    /// so it is deliberately generous.
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
    /// USD per MTok (plus optional long-context tier) by model id.
    pub pricing: BTreeMap<String, ModelPrice>,
    /// Operator-supplied capability profiles by model id. Entries replace the
    /// built-in profile for that model wholesale; they must be complete.
    pub profiles: BTreeMap<String, ModelProfile>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_base_url: DEFAULT_API_BASE_URL.to_string(),
            ledger_path: "llm_budget.json".to_string(),
            max_llm_cost_usd_total: None,
            max_llm_cost_usd_per_day: None,
            credit_expiry: None,
            helper_max_cost_usd_total: None,
            helper_max_cost_usd_per_day: None,
            decision_fallback_models: Vec::new(),
            decider_effort: None,
            helper_effort: None,
            max_corrections: 1,
            max_input_tokens: 20_000,
            decider_max_output_tokens: 8_192,
            helper_max_output_tokens: 1_024,
            request_timeout_secs: 90,
            decision_deadline_secs: 180,
            max_response_bytes: 512 * 1024,
            max_retries: 1,
            retry_backoff_ms: 1_000,
            pricing: BTreeMap::new(),
            profiles: BTreeMap::new(),
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

    /// Validated explicit daily pacing cap.
    pub fn pacing_per_day_usd(&self) -> Result<f64, ConfigError> {
        positive(
            "llm.max_llm_cost_usd_per_day",
            self.max_llm_cost_usd_per_day,
        )
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

    /// Effective capability profile for `model`: the operator-supplied
    /// `[llm.profiles]` entry when present, else the maintained built-in.
    /// A model with neither fails with a capability diagnostic before any
    /// paid call.
    pub fn profile_for(&self, model: &str) -> Result<ModelProfile, ConfigError> {
        let p = match self.profiles.get(model) {
            Some(p) => p.clone(),
            None => builtin_profile(model).ok_or_else(|| {
                invalid(format!(
                    "no capability profile for {model:?}; add [llm.profiles.{model}] with roles, context_tokens, efforts, response_contracts, tool_choice and thinking, or use a maintained model"
                ))
            })?,
        };
        p.validate(model)?;
        Ok(p)
    }

    /// The profile for `model` restricted to `role`, with the decision-role
    /// requirements (a serialization contract and `tool_choice: auto`, which
    /// Sonnet 5.5 mandates and other profiles must declare support for).
    pub fn role_profile(&self, model: &str, role: &str) -> Result<ModelProfile, ConfigError> {
        let p = self.profile_for(model)?;
        if !p.allows(role) {
            return Err(invalid(format!(
                "{model:?} capability profile does not allow the {role} role"
            )));
        }
        if role == ROLE_DECISION {
            if !p.tool_choice.iter().any(|t| t == TOOL_CHOICE_AUTO) {
                return Err(invalid(format!(
                    "{model:?} capability profile must allow tool_choice \"auto\" for the decision role"
                )));
            }
            // The decision request always uses the strict decision tool, so a
            // profile that cannot honor that contract is incompatible.
            if !p
                .response_contracts
                .iter()
                .any(|c| c == CONTRACT_STRICT_TOOL)
            {
                return Err(invalid(format!(
                    "{model:?} capability profile must declare the \"strict_tool\" response contract for the decision role"
                )));
            }
        }
        Ok(p)
    }

    /// Resolve the effort for `model` in `role`: the configured value must be
    /// supported by the model's profile; unset uses the profile default.
    pub fn role_effort(
        &self,
        configured: Option<&str>,
        model: &str,
    ) -> Result<String, ConfigError> {
        let p = self.profile_for(model)?;
        match configured {
            Some(e) => {
                if p.efforts.iter().any(|x| x == e) {
                    Ok(e.to_string())
                } else {
                    Err(invalid(format!(
                        "effort {e:?} is not supported by {model:?}; its profile allows {:?}",
                        p.efforts
                    )))
                }
            }
            None => Ok(p.default_effort.clone()),
        }
    }

    /// Checks for `decider = "claude"`. Model roles, profiles, prices, the
    /// cumulative cap, its explicit pacing sub-cap, helper shares, fallback
    /// candidates, efforts and per-call bounds are all validated before any
    /// paid call can be dispatched.
    pub(super) fn validate(
        &self,
        model: &str,
        helper_model: Option<&str>,
        fallbacks: &[String],
    ) -> Result<(), ConfigError> {
        self.validate_roles(model, helper_model, fallbacks)?;
        self.validate_endpoint()?;
        self.credit_expiry_utc()?;
        self.validate_bounds()?;
        self.validate_context_fit(model, helper_model, fallbacks)?;
        if self.ledger_path.trim().is_empty() {
            return Err(invalid("llm.ledger_path must not be empty"));
        }
        Ok(())
    }

    /// Prices, profiles, roles, efforts, the cumulative cap, its explicit
    /// pacing sub-cap, fallback candidates, and the helper's carved-out
    /// shares.
    fn validate_roles(
        &self,
        model: &str,
        helper_model: Option<&str>,
        fallbacks: &[String],
    ) -> Result<(), ConfigError> {
        let (total, day) = self.validate_budget_shape()?;
        if fallbacks.len() > MAX_FALLBACK_MODELS {
            return Err(invalid(format!(
                "llm.decision_fallback_models allows at most {MAX_FALLBACK_MODELS} candidates, got {}",
                fallbacks.len()
            )));
        }
        self.check_decision_candidate(model)?;
        for fb in fallbacks {
            self.check_decision_candidate(fb)?;
        }
        self.validate_decision_efforts(model, fallbacks)?;
        if let Some(helper) = helper_model {
            self.validate_helper(helper, total, day)?;
        }
        Ok(())
    }

    /// The cumulative cap and its explicit pacing sub-cap.
    fn validate_budget_shape(&self) -> Result<(f64, f64), ConfigError> {
        let total = self.budget_total_usd()?;
        let day = self.pacing_per_day_usd()?;
        if day >= total {
            return Err(invalid(format!(
                "llm.max_llm_cost_usd_per_day ({day}) is a pacing sub-cap and must be smaller than llm.max_llm_cost_usd_total ({total})"
            )));
        }
        Ok((total, day))
    }

    /// One configured effort must be supported by every decision-role model
    /// that could serve the tick.
    fn validate_decision_efforts(
        &self,
        model: &str,
        fallbacks: &[String],
    ) -> Result<(), ConfigError> {
        if let Some(effort) = self.decider_effort.as_deref() {
            for m in std::iter::once(model).chain(fallbacks.iter().map(String::as_str)) {
                self.role_effort(Some(effort), m)?;
            }
        }
        Ok(())
    }

    fn validate_helper(&self, helper: &str, total: f64, day: f64) -> Result<(), ConfigError> {
        self.role_profile(helper, ROLE_HELPER)?;
        self.price(helper)?;
        if let Some(effort) = self.helper_effort.as_deref() {
            self.role_effort(Some(effort), helper)?;
        }
        self.validate_helper_share(total, day)
    }

    /// A decision-role candidate needs a decision-capable profile, a complete
    /// price, and a resolvable effort.
    fn check_decision_candidate(&self, model: &str) -> Result<(), ConfigError> {
        self.role_profile(model, ROLE_DECISION)?;
        self.price(model)?;
        self.role_effort(self.decider_effort.as_deref(), model)?;
        Ok(())
    }

    fn validate_helper_share(&self, total: f64, per_day_usd: f64) -> Result<(), ConfigError> {
        let (h_total, h_day) = self.helper_caps()?;
        if h_total >= total {
            return Err(invalid(
                "llm.helper_max_cost_usd_total must be smaller than llm.max_llm_cost_usd_total",
            ));
        }
        if h_day >= per_day_usd {
            return Err(invalid(
                "llm.helper_max_cost_usd_per_day must be smaller than llm.max_llm_cost_usd_per_day",
            ));
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

    /// `max_input_tokens` must fit inside the smallest context window among
    /// the configured role models — context limits are per model, never a
    /// blanket ceiling.
    fn validate_context_fit(
        &self,
        model: &str,
        helper_model: Option<&str>,
        fallbacks: &[String],
    ) -> Result<(), ConfigError> {
        let mut smallest = self.profile_for(model)?.context_tokens;
        let mut smallest_model = model.to_string();
        for m in fallbacks.iter().map(String::as_str).chain(helper_model) {
            let ctx = self.profile_for(m)?.context_tokens;
            if ctx < smallest {
                smallest = ctx;
                smallest_model = m.to_string();
            }
        }
        if self.max_input_tokens > smallest {
            return Err(invalid(format!(
                "llm.max_input_tokens {} exceeds the context window of {smallest_model:?} ({smallest} tokens)",
                self.max_input_tokens
            )));
        }
        Ok(())
    }

    fn validate_bounds(&self) -> Result<(), ConfigError> {
        if self.max_input_tokens == 0 {
            return Err(invalid("llm.max_input_tokens must be >= 1"));
        }
        // (name, value, min, max) for every numeric per-call bound.
        let bounds: [(&str, u64, u64, u64); 6] = [
            (
                "llm.decider_max_output_tokens",
                u64::from(self.decider_max_output_tokens),
                1,
                u64::from(MAX_OUTPUT_TOKENS),
            ),
            (
                "llm.helper_max_output_tokens",
                u64::from(self.helper_max_output_tokens),
                1,
                u64::from(MAX_OUTPUT_TOKENS),
            ),
            (
                "llm.request_timeout_secs",
                self.request_timeout_secs,
                1,
                600,
            ),
            (
                "llm.decision_deadline_secs",
                self.decision_deadline_secs,
                1,
                3_600,
            ),
            (
                "llm.max_response_bytes",
                self.max_response_bytes as u64,
                1_024,
                8 * 1024 * 1024,
            ),
            ("llm.retry_backoff_ms", self.retry_backoff_ms, 0, 30_000),
        ];
        for (name, value, lo, hi) in bounds {
            if !(lo..=hi).contains(&value) {
                return Err(invalid(format!("{name} must be in {lo}..={hi}")));
            }
        }
        if self.max_retries > 3 {
            return Err(invalid("llm.max_retries must be <= 3"));
        }
        if self.max_corrections > MAX_CORRECTIONS {
            return Err(invalid(format!(
                "llm.max_corrections must be <= {MAX_CORRECTIONS}"
            )));
        }
        Ok(())
    }
}
