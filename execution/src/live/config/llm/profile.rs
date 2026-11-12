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

const KNOWN_ROLES: &[&str] = &[ROLE_DECISION, ROLE_HELPER];
const KNOWN_CONTRACTS: &[&str] = &[CONTRACT_JSON_SCHEMA, CONTRACT_STRICT_TOOL];
const KNOWN_TOOL_CHOICES: &[&str] = &[
    TOOL_CHOICE_AUTO,
    TOOL_CHOICE_NONE,
    TOOL_CHOICE_ANY,
    TOOL_CHOICE_TOOL,
];
const KNOWN_THINKING: &[&str] = &[THINKING_ADAPTIVE, THINKING_DISABLED];

/// `values` is a non-empty list whose entries all come from `known`.
fn subset_ok(values: &[String], known: &[&str]) -> bool {
    !values.is_empty() && values.iter().all(|v| known.contains(&v.as_str()))
}

/// Prefix a sub-check failure with the model it describes.
fn for_model(model: &str, e: ConfigError) -> ConfigError {
    match e {
        ConfigError::Invalid(m) => ConfigError::Invalid(format!("profile for {model:?}: {m}")),
        other => other,
    }
}

impl ModelProfile {
    /// Structural checks every profile must pass, whatever its role.
    pub fn validate(&self, model: &str) -> Result<(), ConfigError> {
        self.check_shape()
            .and_then(|()| self.check_capabilities())
            .map_err(|e| for_model(model, e))
    }

    fn check_shape(&self) -> Result<(), ConfigError> {
        if !subset_ok(&self.roles, KNOWN_ROLES) {
            return Err(invalid(
                "roles must be a non-empty subset of [\"decision\", \"helper\"]",
            ));
        }
        if self.context_tokens < 4_096 {
            return Err(invalid(format!(
                "context_tokens must be >= 4096, got {}",
                self.context_tokens
            )));
        }
        Ok(())
    }

    fn check_capabilities(&self) -> Result<(), ConfigError> {
        if self.efforts.is_empty() || self.efforts.iter().any(|e| e.is_empty()) {
            return Err(invalid("efforts must be a non-empty list of effort levels"));
        }
        if !self.efforts.iter().any(|e| e == &self.default_effort) {
            return Err(invalid(format!(
                "default_effort {:?} is not in efforts {:?}",
                self.default_effort, self.efforts
            )));
        }
        for (values, known, msg) in [
            (
                self.response_contracts.as_slice(),
                KNOWN_CONTRACTS,
                "response_contracts must be a non-empty subset of [\"json_schema\", \"strict_tool\"]",
            ),
            (
                self.tool_choice.as_slice(),
                KNOWN_TOOL_CHOICES,
                "tool_choice must be a non-empty subset of [\"auto\", \"none\", \"any\", \"tool\"]",
            ),
        ] {
            if !subset_ok(values, known) {
                return Err(invalid(msg));
            }
        }
        if !KNOWN_THINKING.contains(&self.thinking.as_str()) {
            return Err(invalid(format!(
                "thinking must be \"adaptive\" or \"disabled\", got {:?}",
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
    // claude-sonnet-5-5: decision role only. Rejects forced tool use
    // (`any`/`tool`) and `thinking: disabled`; adaptive thinking only.
    // claude-haiku-5.5: helper profile. Accepts forced tool choice but the
    // helper contract is plain text, so it is not required; a decision-role
    // Haiku profile must be operator-supplied.
    let (roles, default_effort, tool_choice): (Vec<String>, &str, Vec<String>) = match model {
        "claude-sonnet-5-5" => (
            vec![ROLE_DECISION.into()],
            "high",
            vec![TOOL_CHOICE_AUTO.into(), TOOL_CHOICE_NONE.into()],
        ),
        "claude-haiku-5-5" => (
            vec![ROLE_HELPER.into()],
            "medium",
            vec![
                TOOL_CHOICE_AUTO.into(),
                TOOL_CHOICE_NONE.into(),
                TOOL_CHOICE_ANY.into(),
                TOOL_CHOICE_TOOL.into(),
            ],
        ),
        _ => return None,
    };
    Some(ModelProfile {
        roles,
        context_tokens: 200_000,
        efforts: ["low", "medium", "high"].map(String::from).to_vec(),
        default_effort: default_effort.into(),
        response_contracts: [CONTRACT_JSON_SCHEMA, CONTRACT_STRICT_TOOL]
            .map(String::from)
            .to_vec(),
        tool_choice,
        thinking: THINKING_ADAPTIVE.into(),
    })
}
