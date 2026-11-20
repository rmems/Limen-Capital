//! Research execution configuration. Payoff odds must be supplied, not inferred
//! from an uncalibrated confidence score or a percentage price move.

use crate::{kelly::PositionSizer, ExecutionEngine};

#[derive(Debug)]
pub struct ExecutionConfig {
    pub confidence_threshold: f32,
    pub kelly_b: f64,
    pub kelly_scalar: f64,
    /// Per-order quote notional, not asset units or aggregate exposure.
    pub max_order_notional: f64,
}

impl ExecutionConfig {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(format!("{name} must be valid UTF-8")),
        })
    }

    fn from_lookup(
        mut lookup: impl FnMut(&str) -> Result<Option<String>, String>,
    ) -> Result<Self, String> {
        let mut read = |name: &str, default: Option<f64>| number(&mut lookup, name, default);
        let config = Self {
            confidence_threshold: read("LIMEN_CONFIDENCE_THRESHOLD", Some(0.15))? as f32,
            kelly_b: read("LIMEN_KELLY_B", None)?,
            kelly_scalar: read("LIMEN_KELLY_SCALAR", Some(0.25))?,
            max_order_notional: read("LIMEN_MAX_ORDER_NOTIONAL", Some(10.0))?,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        ensure(
            (0.0..=1.0).contains(&self.confidence_threshold),
            "LIMEN_CONFIDENCE_THRESHOLD must be in [0, 1]",
        )?;
        ensure(self.kelly_b > 0.0, "LIMEN_KELLY_B must be > 0")?;
        ensure(
            self.kelly_scalar > 0.0 && self.kelly_scalar <= 1.0,
            "LIMEN_KELLY_SCALAR must be in (0, 1]",
        )?;
        ensure(
            self.max_order_notional > 0.0 && self.max_order_notional <= f32::MAX as f64,
            "LIMEN_MAX_ORDER_NOTIONAL must be in (0, f32::MAX]",
        )
    }

    pub fn engine(&self) -> ExecutionEngine {
        let engine = ExecutionEngine::new(self.confidence_threshold, self.max_order_notional);
        let balance = engine.ghost_wallet.balance_atp as f64;
        engine.with_sizer(PositionSizer::new(balance, self.kelly_b, self.kelly_scalar))
    }
}

fn number(
    lookup: &mut impl FnMut(&str) -> Result<Option<String>, String>,
    name: &str,
    default: Option<f64>,
) -> Result<f64, String> {
    let Some(raw) = lookup(name)? else {
        return default.ok_or_else(|| {
            format!("{name} is required: expected gain / expected loss, not a return percentage")
        });
    };
    let value = raw
        .parse::<f64>()
        .map_err(|_| format!("{name} must be a number"))?;
    ensure(value.is_finite(), &format!("{name} must be finite"))?;
    Ok(value)
}

fn ensure(ok: bool, message: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_validation() {
        let parse = |key: &str, value: Option<&str>| {
            ExecutionConfig::from_lookup(|name| {
                Ok(if name == key {
                    value.map(str::to_owned)
                } else if name == "LIMEN_KELLY_B" {
                    Some("5".into())
                } else {
                    None
                })
            })
        };
        let config = parse("", None).unwrap();
        assert_eq!(config.confidence_threshold, 0.15);
        assert_eq!(config.kelly_b, 5.0);
        assert_eq!(config.kelly_scalar, 0.25);
        assert_eq!(config.max_order_notional, 10.0);
        assert!(parse("LIMEN_KELLY_B", None).is_err());
        for key in [
            "LIMEN_CONFIDENCE_THRESHOLD",
            "LIMEN_KELLY_B",
            "LIMEN_KELLY_SCALAR",
            "LIMEN_MAX_ORDER_NOTIONAL",
        ] {
            for bad in ["", "bad", "NaN", "inf", "-inf", "-1"] {
                assert!(parse(key, Some(bad)).unwrap_err().contains(key));
            }
        }
        for key in ["LIMEN_CONFIDENCE_THRESHOLD", "LIMEN_KELLY_SCALAR"] {
            assert!(parse(key, Some("1.01")).is_err());
            assert!(parse(key, Some("1")).is_ok());
        }
        assert!(parse("LIMEN_CONFIDENCE_THRESHOLD", Some("0")).is_ok());
        for key in [
            "LIMEN_KELLY_B",
            "LIMEN_KELLY_SCALAR",
            "LIMEN_MAX_ORDER_NOTIONAL",
        ] {
            assert!(parse(key, Some("0")).is_err());
        }
    }
}
