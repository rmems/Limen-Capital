//! Capability profiles for `[llm]` models: which roles a model may serve, its
//! context window, accepted effort levels, serialization contracts,
//! tool-choice modes and thinking mode.

use super::super::ConfigError;
use super::invalid;
use serde::{Deserialize, Serialize};

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
pub(super) fn builtin_profile(model: &str) -> Option<ModelProfile> {
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
