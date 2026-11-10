//! `LiveConfig` (TOML file) + `Secrets` (environment only).
//!
//! Caps and venue settings are **configuration**, never constants in the
//! decider, and the decider never sees them. Secrets never live in TOML.

pub mod llm;

use crate::live::decider::rules::RulesConfig;
use crate::live::risk::RiskLimits;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

/// Symbols this experiment may trade. One instrument per run.
pub const ALLOWED_SYMBOLS: &[&str] = &["BTC-USD", "ETH-USD"];

pub use llm::{
    LlmConfig, ModelPrice, DECISION_MODELS, HELPER_MODELS, MAX_EXPERIMENT_BUDGET_USD,
    MAX_PROMPT_TOKENS,
};

/// Decision-role model (the only role that may propose orders).
pub const DEFAULT_MODEL: &str = "claude-sonnet-5-5";

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Invalid(String),
    MissingSecret(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "config io: {e}"),
            ConfigError::Parse(e) => write!(f, "config parse: {e}"),
            ConfigError::Invalid(m) => write!(f, "invalid config: {m}"),
            ConfigError::MissingSecret(v) => {
                write!(f, "missing secret: set the {v} environment variable")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        ConfigError::Io(e)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        ConfigError::Parse(e)
    }
}

/// Where quotes come from.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum MarketSourceConfig {
    /// Live quotes from the venue (requires `ROBINHOOD_*` secrets, even in paper mode).
    #[default]
    Robinhood,
    /// Seeded random-walk quotes for offline paper runs and tests. Clearly
    /// labelled in the journal; never used when `paper = false`.
    Synthetic {
        #[serde(default = "default_seed")]
        seed: u64,
        #[serde(default = "default_start_price")]
        start_price: f64,
        /// Per-tick log-return volatility (e.g. 0.002 = 20 bps).
        #[serde(default = "default_vol")]
        vol: f64,
        /// Half-spread in basis points.
        #[serde(default = "default_half_spread_bps")]
        half_spread_bps: f64,
    },
}

fn default_seed() -> u64 {
    42
}
fn default_start_price() -> f64 {
    100_000.0
}
fn default_vol() -> f64 {
    0.002
}
fn default_half_spread_bps() -> f64 {
    1.0
}

impl MarketSourceConfig {
    pub fn label(&self) -> &'static str {
        match self {
            MarketSourceConfig::Robinhood => "robinhood",
            MarketSourceConfig::Synthetic { .. } => "synthetic",
        }
    }
}

/// Which decider proposes actions.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DeciderKind {
    /// Deterministic mean-reversion toy (tests and the default). Never a
    /// fallback for Claude: a failed Claude tick holds.
    #[default]
    Rules,
    /// Claude Messages API: Sonnet decides; an optional Haiku helper only
    /// writes non-executable notes.
    Claude,
}

/// Top-level configuration for one run.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LiveConfig {
    /// Trading pair, e.g. `BTC-USD`. Must be in [`ALLOWED_SYMBOLS`].
    pub symbol: String,
    /// Venue base URL.
    pub base_url: String,
    /// Seconds between ticks.
    pub tick_interval_secs: u64,
    /// Decision-role model id; must be in [`DECISION_MODELS`].
    pub model: String,
    /// Optional helper-role model (disabled by default); must be in
    /// [`HELPER_MODELS`]. Helpers never propose orders.
    pub helper_model: Option<String>,
    /// Paper mode: never submit orders; log what would have been sent.
    pub paper: bool,
    /// Markdown journal path.
    pub journal_path: String,
    /// Starting cash for the paper book (paper mode only).
    pub paper_starting_cash_usd: f64,
    /// Fee ratio charged on paper fills (Robinhood's entry API tier is ~0.45%).
    pub paper_fee_ratio: f64,
    /// Quote source.
    pub market: MarketSourceConfig,
    /// Which decider to run.
    pub decider: DeciderKind,
    /// Parameters for the rules decider.
    pub rules: RulesConfig,
    /// Hard caps enforced by `risk::check`.
    pub risk: RiskLimits,
    /// Claude roles, pricing, experiment budget and call bounds.
    pub llm: LlmConfig,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            symbol: "BTC-USD".to_string(),
            base_url: crate::live::robinhood::DEFAULT_BASE_URL.to_string(),
            tick_interval_secs: 900,
            model: DEFAULT_MODEL.to_string(),
            helper_model: None,
            paper: true,
            journal_path: "journal.md".to_string(),
            paper_starting_cash_usd: 10.0,
            paper_fee_ratio: 0.0045,
            market: MarketSourceConfig::Robinhood,
            decider: DeciderKind::Rules,
            rules: RulesConfig::default(),
            risk: RiskLimits::default(),
            llm: LlmConfig::default(),
        }
    }
}

impl LiveConfig {
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        let cfg: LiveConfig = toml::from_str(s)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let s = std::fs::read_to_string(path)?;
        Self::from_toml_str(&s)
    }

    /// Asset code for the configured pair (`BTC` for `BTC-USD`).
    pub fn asset_code(&self) -> &str {
        self.symbol.split('-').next().unwrap_or(&self.symbol)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.validate_symbol_and_cadence()?;
        self.validate_base_url()?;
        self.validate_market()?;
        self.validate_paper_book()?;
        self.validate_rules()?;
        self.validate_models()?;
        self.risk.validate().map_err(ConfigError::Invalid)?;
        self.validate_decider_kind()
    }

    fn validate_decider_kind(&self) -> Result<(), ConfigError> {
        if self.decider == DeciderKind::Claude {
            self.validate_llm()?;
        }
        Ok(())
    }

    fn validate_symbol_and_cadence(&self) -> Result<(), ConfigError> {
        if !ALLOWED_SYMBOLS.contains(&self.symbol.as_str()) {
            return Err(ConfigError::Invalid(format!(
                "symbol {:?} not allowed; one of {:?}",
                self.symbol, ALLOWED_SYMBOLS
            )));
        }
        if self.tick_interval_secs == 0 {
            return Err(ConfigError::Invalid(
                "tick_interval_secs must be > 0".into(),
            ));
        }
        Ok(())
    }

    /// `[llm]` checks needed before any Claude call can be dispatched.
    pub fn validate_llm(&self) -> Result<(), ConfigError> {
        self.validate_models()?;
        self.llm.validate(
            &self.model,
            self.helper_model.as_deref(),
            self.risk.max_llm_cost_usd_per_day,
        )
    }

    fn validate_models(&self) -> Result<(), ConfigError> {
        if !DECISION_MODELS.contains(&self.model.as_str()) {
            let why = if HELPER_MODELS.contains(&self.model.as_str()) {
                "is a helper model and cannot hold the decision role"
            } else {
                "is not an allowed model"
            };
            return Err(ConfigError::Invalid(format!(
                "model {:?} {why}; the decision role allows only {DECISION_MODELS:?}",
                self.model
            )));
        }
        if let Some(helper) = &self.helper_model {
            if !HELPER_MODELS.contains(&helper.as_str()) {
                let why = if DECISION_MODELS.contains(&helper.as_str()) {
                    "is a decision model and cannot hold the helper role"
                } else {
                    "is not an allowed model"
                };
                return Err(ConfigError::Invalid(format!(
                    "helper_model {helper:?} {why}; the helper role allows only {HELPER_MODELS:?}"
                )));
            }
            if self.decider != DeciderKind::Claude {
                return Err(ConfigError::Invalid(
                    "helper_model requires decider = \"claude\"".into(),
                ));
            }
        }
        Ok(())
    }

    fn validate_base_url(&self) -> Result<(), ConfigError> {
        if !(self.base_url.starts_with("https://") || self.base_url.starts_with("http://")) {
            return Err(ConfigError::Invalid(format!(
                "base_url must start with http(s)://, got {:?}",
                self.base_url
            )));
        }
        // Signed headers carry the API key, so plain HTTP is only ever allowed
        // towards a loopback mock (tests / offline demos), never in live mode.
        if self.base_url.starts_with("http://") {
            if !self.paper {
                return Err(ConfigError::Invalid(
                    "live mode requires an https:// base_url".into(),
                ));
            }
            if !is_loopback_url(&self.base_url) {
                return Err(ConfigError::Invalid(format!(
                    "plain http:// base_url is only allowed for loopback mocks, got {:?}",
                    self.base_url
                )));
            }
        }
        Ok(())
    }

    fn validate_market(&self) -> Result<(), ConfigError> {
        if let MarketSourceConfig::Synthetic {
            start_price,
            vol,
            half_spread_bps,
            ..
        } = &self.market
        {
            validate_synthetic(*start_price, *vol, *half_spread_bps)?;
        }
        if !self.paper && matches!(self.market, MarketSourceConfig::Synthetic { .. }) {
            return Err(ConfigError::Invalid(
                "synthetic market source is only allowed in paper mode".into(),
            ));
        }
        Ok(())
    }

    fn validate_rules(&self) -> Result<(), ConfigError> {
        if !(self.rules.order_usd.is_finite() && self.rules.order_usd > 0.0) {
            return Err(ConfigError::Invalid(
                "rules.order_usd must be positive".into(),
            ));
        }
        Ok(())
    }

    fn validate_paper_book(&self) -> Result<(), ConfigError> {
        if !(self.paper_starting_cash_usd.is_finite() && self.paper_starting_cash_usd > 0.0) {
            return Err(ConfigError::Invalid(
                "paper_starting_cash_usd must be positive".into(),
            ));
        }
        if !(self.paper_fee_ratio.is_finite() && (0.0..0.1).contains(&self.paper_fee_ratio)) {
            return Err(ConfigError::Invalid(
                "paper_fee_ratio must be in [0, 0.1)".into(),
            ));
        }
        Ok(())
    }
}

fn validate_synthetic(start_price: f64, vol: f64, half_spread_bps: f64) -> Result<(), ConfigError> {
    if !(start_price.is_finite() && start_price > 0.0) {
        return Err(ConfigError::Invalid(
            "market.start_price must be positive".into(),
        ));
    }
    if !(vol.is_finite() && vol >= 0.0) {
        return Err(ConfigError::Invalid("market.vol must be >= 0".into()));
    }
    if !(half_spread_bps.is_finite() && half_spread_bps >= 0.0) {
        return Err(ConfigError::Invalid(
            "market.half_spread_bps must be >= 0".into(),
        ));
    }
    Ok(())
}

/// `http://` host is a loopback address (`127.x.x.x`, `localhost`, `[::1]`).
fn is_loopback_url(url: &str) -> bool {
    let rest = url.trim_start_matches("http://");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Userinfo (`user:pass@host`) would make the text before `@` look like the host.
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        authority
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(authority)
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Secrets, read from the environment only.
#[derive(Clone, Default)]
pub struct Secrets {
    pub robinhood_api_key: Option<String>,
    pub robinhood_private_key: Option<String>,
    pub anthropic_api_key: Option<String>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("robinhood_api_key", &self.robinhood_api_key.is_some())
            .field(
                "robinhood_private_key",
                &self.robinhood_private_key.is_some(),
            )
            .field("anthropic_api_key", &self.anthropic_api_key.is_some())
            .finish()
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl Secrets {
    pub fn from_env() -> Self {
        Self {
            robinhood_api_key: env_nonempty("ROBINHOOD_API_KEY"),
            robinhood_private_key: env_nonempty("ROBINHOOD_PRIVATE_KEY"),
            anthropic_api_key: env_nonempty("ANTHROPIC_API_KEY"),
        }
    }

    /// `(api_key, private_key_b64)` or which variable is missing.
    pub fn robinhood(&self) -> Result<(&str, &str), ConfigError> {
        let k = self
            .robinhood_api_key
            .as_deref()
            .ok_or(ConfigError::MissingSecret("ROBINHOOD_API_KEY"))?;
        let p = self
            .robinhood_private_key
            .as_deref()
            .ok_or(ConfigError::MissingSecret("ROBINHOOD_PRIVATE_KEY"))?;
        Ok((k, p))
    }

    pub fn anthropic(&self) -> Result<&str, ConfigError> {
        self.anthropic_api_key
            .as_deref()
            .ok_or(ConfigError::MissingSecret("ANTHROPIC_API_KEY"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_ten_dollar_paper_run() {
        let c = LiveConfig::default();
        assert_eq!(c.symbol, "BTC-USD");
        assert!(c.paper);
        assert_eq!(c.tick_interval_secs, 900);
        assert_eq!(c.model, DEFAULT_MODEL);
        assert_eq!(c.market, MarketSourceConfig::Robinhood);
        assert_eq!(c.decider, DeciderKind::Rules);
        assert_eq!(c.paper_fee_ratio, 0.0045);
        assert_eq!(c.asset_code(), "BTC");
        c.validate().unwrap();
    }

    #[test]
    fn parses_toml_with_synthetic_market() {
        let c = LiveConfig::from_toml_str(
            r#"
symbol = "ETH-USD"
tick_interval_secs = 1
paper = true
journal_path = "/tmp/j.md"

[market]
source = "synthetic"
seed = 7
start_price = 3000.0
"#,
        )
        .unwrap();
        assert_eq!(c.symbol, "ETH-USD");
        assert_eq!(c.asset_code(), "ETH");
        match c.market {
            MarketSourceConfig::Synthetic {
                seed,
                start_price,
                vol,
                ..
            } => {
                assert_eq!(seed, 7);
                assert_eq!(start_price, 3000.0);
                assert_eq!(vol, 0.002);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn risk_section_parses_and_validates() {
        let c = LiveConfig::from_toml_str(
            r#"
[risk]
max_order_usd = 1.0
max_daily_loss_usd = 2.0
"#,
        )
        .unwrap();
        assert_eq!(c.risk.max_order_usd, 1.0);
        assert_eq!(c.risk.max_daily_loss_usd, 2.0);
        assert_eq!(c.risk.max_position_usd, 10.0);
        assert!(LiveConfig::from_toml_str("[risk]\nmax_order_usd = 50.0").is_err());
        let c = LiveConfig::from_toml_str("[rules]\nentry_bps = 25.0").unwrap();
        assert_eq!(c.rules.entry_bps, 25.0);
        // Claude without its budget/expiry/pricing config fails closed.
        let err = LiveConfig::from_toml_str("decider = \"claude\"").unwrap_err();
        assert!(err.to_string().contains("llm."), "{err}");
        assert!(LiveConfig::from_toml_str("decider = \"random\"").is_err());
    }

    #[test]
    fn rejects_unknown_symbol_and_unknown_fields() {
        assert!(LiveConfig::from_toml_str(r#"symbol = "DOGE-USD""#).is_err());
        assert!(LiveConfig::from_toml_str(r#"max_leverage = 10"#).is_err());
        assert!(LiveConfig::from_toml_str(r#"tick_interval_secs = 0"#).is_err());
    }

    #[test]
    fn live_mode_forbids_synthetic_quotes_and_plain_http() {
        let err = LiveConfig::from_toml_str(
            r#"
paper = false
[market]
source = "synthetic"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("synthetic"));
        let err = LiveConfig::from_toml_str(
            r#"
paper = false
base_url = "http://localhost:9"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("https"));
    }

    #[test]
    fn plain_http_only_for_loopback_mocks() {
        for ok in [
            "http://127.0.0.1:18765",
            "http://localhost:9/",
            "http://[::1]:8080",
            "http://127.5.6.7",
        ] {
            assert!(is_loopback_url(ok), "{ok}");
            LiveConfig::from_toml_str(&format!("base_url = {ok:?}")).unwrap();
        }
        for bad in [
            "http://trading.robinhood.com",
            "http://127.0.0.1.evil.example",
            "http://10.0.0.5",
            "http://127.0.0.1:80@evil.example",
            "http://localhost@evil.example",
        ] {
            assert!(!is_loopback_url(bad), "{bad}");
            let err = LiveConfig::from_toml_str(&format!("base_url = {bad:?}")).unwrap_err();
            assert!(err.to_string().contains("loopback"), "{bad}: {err}");
        }
    }

    #[test]
    fn synthetic_params_are_validated() {
        let err = LiveConfig::from_toml_str("[market]\nsource = \"synthetic\"\nstart_price = 0.0")
            .unwrap_err();
        assert!(err.to_string().contains("start_price"));
        assert!(LiveConfig::from_toml_str("[market]\nsource = \"synthetic\"\nvol = -0.1").is_err());
        assert!(LiveConfig::from_toml_str(
            "[market]\nsource = \"synthetic\"\nhalf_spread_bps = -1.0"
        )
        .is_err());
        LiveConfig::from_toml_str("[market]\nsource = \"synthetic\"\nvol = 0.0").unwrap();
    }

    #[test]
    fn secrets_report_missing_variable_by_name() {
        let s = Secrets {
            robinhood_api_key: Some("rh-api-SECRET-VALUE".into()),
            ..Default::default()
        };
        assert!(matches!(
            s.robinhood(),
            Err(ConfigError::MissingSecret("ROBINHOOD_PRIVATE_KEY"))
        ));
        assert!(matches!(
            s.anthropic(),
            Err(ConfigError::MissingSecret("ANTHROPIC_API_KEY"))
        ));
        assert!(!format!("{s:?}").contains("SECRET-VALUE"));
    }

    fn claude_toml(extra_top: &str, extra_llm: &str) -> String {
        format!(
            r#"
decider = "claude"
{extra_top}
[llm]
max_llm_cost_usd_total = 200.0
credit_expiry = "2026-11-12T23:59:59-08:00"
{extra_llm}
[llm.pricing."claude-sonnet-5-5"]
input_usd_per_mtok = 2.0
output_usd_per_mtok = 10.0
cache_read_usd_per_mtok = 0.2
cache_write_usd_per_mtok = 2.5
[llm.pricing."claude-haiku-5-5"]
input_usd_per_mtok = 0.1
output_usd_per_mtok = 0.5
cache_read_usd_per_mtok = 0.01
cache_write_usd_per_mtok = 0.125
"#
        )
    }

    fn err_of(toml: &str) -> String {
        LiveConfig::from_toml_str(toml).unwrap_err().to_string()
    }

    #[test]
    fn example_config_is_valid_and_seeded() {
        let c =
            LiveConfig::from_toml_str(include_str!("../../../config/live.example.toml")).unwrap();
        assert_eq!(c.decider, DeciderKind::Claude);
        assert_eq!(c.model, "claude-sonnet-5-5");
        assert_eq!(c.helper_model, None);
        assert_eq!(c.llm.max_llm_cost_usd_total, Some(200.0));
        assert_eq!(c.llm.pricing["claude-sonnet-5-5"].input_usd_per_mtok, 2.0);
        assert_eq!(c.llm.pricing["claude-haiku-5-5"].output_usd_per_mtok, 0.5);
    }

    #[test]
    fn model_roles_are_allow_listed() {
        LiveConfig::from_toml_str(&claude_toml("", "")).unwrap();
        let e = err_of(&claude_toml("model = \"claude-opus-9\"", ""));
        assert!(e.contains("not an allowed model"), "{e}");
        let e = err_of(&claude_toml("model = \"claude-haiku-5-5\"", ""));
        assert!(e.contains("helper model"), "{e}");
        let e = err_of(&claude_toml("helper_model = \"claude-sonnet-5-5\"", ""));
        assert!(e.contains("decision model"), "{e}");
        let e = err_of(&claude_toml("helper_model = \"gpt-x\"", ""));
        assert!(e.contains("helper_model"), "{e}");
        // Rules decider: model ids are still checked, and helpers need claude.
        assert!(LiveConfig::from_toml_str("model = \"claude-haiku-5-5\"").is_err());
        let e = err_of("helper_model = \"claude-haiku-5-5\"");
        assert!(e.contains("requires decider"), "{e}");
    }

    #[test]
    fn cumulative_budget_is_required_positive_and_at_most_200() {
        let base = claude_toml("", "");
        let e = err_of(&base.replace("max_llm_cost_usd_total = 200.0\n", ""));
        assert!(e.contains("max_llm_cost_usd_total is required"), "{e}");
        for bad in ["0.0", "-1.0", "200.01", "1000.0", "nan"] {
            let e = err_of(&base.replace("= 200.0", &format!("= {bad}")));
            assert!(e.contains("max_llm_cost_usd_total"), "{bad}: {e}");
        }
        // The daily pacing cap must be strictly smaller than the total.
        let e = err_of(&claude_toml("[risk]\nmax_llm_cost_usd_per_day = 200.0", ""));
        assert!(e.contains("pacing sub-cap"), "{e}");
        LiveConfig::from_toml_str(&claude_toml("[risk]\nmax_llm_cost_usd_per_day = 199.0", ""))
            .unwrap();
    }

    #[test]
    fn credit_expiry_must_be_explicit_and_not_after_2026_11_12() {
        let base = claude_toml("", "");
        let with = |v: &str| base.replace("\"2026-11-12T23:59:59-08:00\"", v);
        let e = err_of(&base.replace("credit_expiry = \"2026-11-12T23:59:59-08:00\"\n", ""));
        assert!(e.contains("credit_expiry is required"), "{e}");
        for bad in [
            "\"2026-11-12T23:59:59\"",
            "\"tomorrow\"",
            "\"\"",
            "\"2026-11-13T00:00:00Z\"",
            "\"2026-11-13T00:30:00+14:00\"",
        ] {
            let e = err_of(&with(bad));
            assert!(e.contains("credit_expiry"), "{bad}: {e}");
        }
        for ok in [
            "\"2026-11-12T23:59:59Z\"",
            "\"2026-11-12T23:59:59+09:00\"",
            "\"2026-11-11T12:00:00Z\"",
        ] {
            LiveConfig::from_toml_str(&with(ok)).unwrap();
        }
    }

    #[test]
    fn pricing_endpoint_and_bounds_fail_closed() {
        let base = claude_toml("", "");
        let no_sonnet = base.replace(
            "[llm.pricing.\"claude-sonnet-5-5\"]",
            "[llm.pricing.\"other\"]",
        );
        assert!(err_of(&no_sonnet).contains("no entry for \"claude-sonnet-5-5\""));
        let e = err_of(&base.replace("input_usd_per_mtok = 2.0", "input_usd_per_mtok = 0.0"));
        assert!(e.contains("input_usd_per_mtok"), "{e}");
        let e = err_of(&base.replace("cache_write_usd_per_mtok = 2.5\n", ""));
        assert!(e.contains("cache_write_usd_per_mtok"), "{e}");
        let e = err_of(&claude_toml(
            "",
            "api_base_url = \"http://api.anthropic.com\"",
        ));
        assert!(e.contains("loopback"), "{e}");
        LiveConfig::from_toml_str(&claude_toml("", "api_base_url = \"http://127.0.0.1:9\""))
            .unwrap();
        for (k, v) in [
            ("max_input_tokens", "100001"),
            ("max_input_tokens", "0"),
            ("decider_max_output_tokens", "0"),
            ("request_timeout_secs", "0"),
            ("decision_deadline_secs", "0"),
            ("max_retries", "9"),
            ("max_response_bytes", "10"),
        ] {
            let e = err_of(&claude_toml("", &format!("{k} = {v}")));
            assert!(e.contains(k), "{k}={v}: {e}");
        }
    }

    #[test]
    fn helper_needs_a_smaller_carved_out_share() {
        let h = "helper_model = \"claude-haiku-5-5\"\n[risk]\nmax_llm_cost_usd_per_day = 10.0";
        let e = err_of(&claude_toml(h, ""));
        assert!(e.contains("helper_max_cost_usd_total is required"), "{e}");
        let e = err_of(&claude_toml(
            h,
            "helper_max_cost_usd_total = 200.0\nhelper_max_cost_usd_per_day = 1.0",
        ));
        assert!(e.contains("smaller than llm.max_llm_cost_usd_total"), "{e}");
        let e = err_of(&claude_toml(
            h,
            "helper_max_cost_usd_total = 5.0\nhelper_max_cost_usd_per_day = 10.0",
        ));
        assert!(
            e.contains("smaller than risk.max_llm_cost_usd_per_day"),
            "{e}"
        );
        LiveConfig::from_toml_str(&claude_toml(
            h,
            "helper_max_cost_usd_total = 5.0\nhelper_max_cost_usd_per_day = 1.0",
        ))
        .unwrap();
    }
}
