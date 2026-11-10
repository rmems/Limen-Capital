//! `LiveConfig` (TOML file) + `Secrets` (environment only).
//!
//! Caps and venue settings are **configuration**, never constants in the
//! decider, and the decider never sees them. Secrets never live in TOML.

use crate::live::decider::rules::RulesConfig;
use crate::live::risk::RiskLimits;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

/// Symbols this experiment may trade. One instrument per run.
pub const ALLOWED_SYMBOLS: &[&str] = &["AAPL", "SPY"];

pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";

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
    /// Live quotes from the venue (requires `APCA_*` secrets, even in paper mode).
    #[default]
    Alpaca,
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
    200.0
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
            MarketSourceConfig::Alpaca => "alpaca",
            MarketSourceConfig::Synthetic { .. } => "synthetic",
        }
    }
}

/// Which decider proposes actions.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DeciderKind {
    /// Deterministic mean-reversion toy (tests, paper default, LLM fallback).
    #[default]
    Rules,
    /// Claude API decision loop (later step).
    Claude,
}

/// Top-level configuration for one run.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LiveConfig {
    /// Trading pair, e.g. `AAPL`. Must be in [`ALLOWED_SYMBOLS`].
    pub symbol: String,
    /// Seconds between ticks.
    pub tick_interval_secs: u64,
    /// LLM model id for the decider.
    pub model: String,
    /// Paper mode: never submit orders; log what would have been sent.
    pub paper: bool,
    /// Markdown journal path.
    pub journal_path: String,
    /// Starting cash for the paper book (paper mode only).
    pub paper_starting_cash_usd: f64,
    /// Fee ratio charged on paper fills (configurable simulated cost).
    pub paper_fee_ratio: f64,
    /// Quote source.
    pub market: MarketSourceConfig,
    /// Which decider to run.
    pub decider: DeciderKind,
    /// Parameters for the rules decider.
    pub rules: RulesConfig,
    /// Hard caps enforced by `risk::check`.
    pub risk: RiskLimits,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            symbol: "AAPL".to_string(),
            tick_interval_secs: 900,
            model: DEFAULT_MODEL.to_string(),
            paper: true,
            journal_path: "journal.md".to_string(),
            paper_starting_cash_usd: 10.0,
            paper_fee_ratio: 0.0045,
            market: MarketSourceConfig::Alpaca,
            decider: DeciderKind::Rules,
            rules: RulesConfig::default(),
            risk: RiskLimits::default(),
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

    pub fn validate(&self) -> Result<(), ConfigError> {
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
        self.validate_market()?;
        self.validate_paper_book()?;
        self.validate_rules()?;
        if self.model.trim().is_empty() {
            return Err(ConfigError::Invalid("model must not be empty".into()));
        }
        self.risk.validate().map_err(ConfigError::Invalid)
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

/// Secrets, read from the environment only.
#[derive(Clone, Default)]
pub struct Secrets {
    pub alpaca_key_id: Option<String>,
    pub alpaca_secret_key: Option<String>,
    pub anthropic_api_key: Option<String>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("alpaca_key_id", &self.alpaca_key_id.is_some())
            .field("alpaca_secret_key", &self.alpaca_secret_key.is_some())
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
            alpaca_key_id: env_nonempty("APCA_API_KEY_ID"),
            alpaca_secret_key: env_nonempty("APCA_API_SECRET_KEY"),
            anthropic_api_key: env_nonempty("ANTHROPIC_API_KEY"),
        }
    }

    /// `(api_key, secret_key)` or which variable is missing.
    pub fn alpaca(&self) -> Result<(&str, &str), ConfigError> {
        let k = self
            .alpaca_key_id
            .as_deref()
            .ok_or(ConfigError::MissingSecret("APCA_API_KEY_ID"))?;
        let p = self
            .alpaca_secret_key
            .as_deref()
            .ok_or(ConfigError::MissingSecret("APCA_API_SECRET_KEY"))?;
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
        assert_eq!(c.symbol, "AAPL");
        assert!(c.paper);
        assert_eq!(c.tick_interval_secs, 900);
        assert_eq!(c.model, DEFAULT_MODEL);
        assert_eq!(c.market, MarketSourceConfig::Alpaca);
        assert_eq!(c.decider, DeciderKind::Rules);
        assert_eq!(c.paper_fee_ratio, 0.0045);
        c.validate().unwrap();
    }

    #[test]
    fn parses_toml_with_synthetic_market() {
        let c = LiveConfig::from_toml_str(
            r#"
symbol = "SPY"
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
        assert_eq!(c.symbol, "SPY");
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
        let c =
            LiveConfig::from_toml_str("decider = \"claude\"\n[rules]\nentry_bps = 25.0").unwrap();
        assert_eq!(c.decider, DeciderKind::Claude);
        assert_eq!(c.rules.entry_bps, 25.0);
        assert!(LiveConfig::from_toml_str("decider = \"random\"").is_err());
    }

    #[test]
    fn rejects_unknown_symbol_and_unknown_fields() {
        assert!(LiveConfig::from_toml_str(r#"symbol = "DOGE-USD""#).is_err());
        assert!(LiveConfig::from_toml_str(r#"max_leverage = 10"#).is_err());
        assert!(LiveConfig::from_toml_str(r#"tick_interval_secs = 0"#).is_err());
    }

    #[test]
    fn live_mode_forbids_synthetic_quotes_and_old_venue_fields() {
        let err = LiveConfig::from_toml_str("paper = false\n[market]\nsource = \"synthetic\"")
            .unwrap_err();
        assert!(err.to_string().contains("synthetic"));
        assert!(LiveConfig::from_toml_str("base_url = \"https://example.com\"").is_err());
        assert!(LiveConfig::from_toml_str("api_key = \"not-allowed\"").is_err());
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
            alpaca_key_id: Some("alpaca-mock-SECRET-VALUE".into()),
            ..Default::default()
        };
        assert!(matches!(
            s.alpaca(),
            Err(ConfigError::MissingSecret("APCA_API_SECRET_KEY"))
        ));
        assert!(matches!(
            s.anthropic(),
            Err(ConfigError::MissingSecret("ANTHROPIC_API_KEY"))
        ));
        assert!(!format!("{s:?}").contains("SECRET-VALUE"));
    }
}
