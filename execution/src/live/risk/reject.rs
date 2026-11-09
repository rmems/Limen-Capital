//! Typed rejection reasons and their stable journal presentation.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum RiskReject {
    DailyLossBreached {
        loss_usd: f64,
        cap_usd: f64,
    },
    LlmBudgetExhausted {
        spent_usd: f64,
        cap_usd: f64,
    },
    BadQuote,
    PairNotTradable,
    InvalidSize {
        size_usd: f64,
    },
    OrderTooLarge {
        size_usd: f64,
        cap_usd: f64,
    },
    TooManyOpenOrders {
        open: usize,
        max: usize,
    },
    MinTickInterval {
        since_secs: i64,
        min_secs: u64,
    },
    PositionCapExceeded {
        position_usd: f64,
        size_usd: f64,
        cap_usd: f64,
    },
    InsufficientCash {
        needed_usd: f64,
        cash_usd: f64,
    },
    InsufficientHoldings {
        qty: f64,
        held: f64,
    },
    RoundsToZero,
    BelowMinOrderAmount {
        notional_usd: f64,
        min_usd: f64,
    },
    AboveMaxOrderSize {
        qty: f64,
        max_qty: f64,
    },
}

impl RiskReject {
    /// Short stable tag for the journal.
    pub fn tag(&self) -> &'static str {
        match self {
            RiskReject::DailyLossBreached { .. } => "daily_loss_breached",
            RiskReject::LlmBudgetExhausted { .. } => "llm_budget_exhausted",
            RiskReject::BadQuote => "bad_quote",
            RiskReject::PairNotTradable => "pair_not_tradable",
            RiskReject::InvalidSize { .. } => "invalid_size",
            RiskReject::OrderTooLarge { .. } => "order_too_large",
            RiskReject::TooManyOpenOrders { .. } => "too_many_open_orders",
            _ => self.order_tag(),
        }
    }

    fn order_tag(&self) -> &'static str {
        match self {
            RiskReject::MinTickInterval { .. } => "min_tick_interval",
            RiskReject::PositionCapExceeded { .. } => "position_cap_exceeded",
            RiskReject::InsufficientCash { .. } => "insufficient_cash",
            RiskReject::InsufficientHoldings { .. } => "insufficient_holdings",
            RiskReject::RoundsToZero => "rounds_to_zero",
            RiskReject::BelowMinOrderAmount { .. } => "below_min_order_amount",
            RiskReject::AboveMaxOrderSize { .. } => "above_max_order_size",
            _ => unreachable!("preflight rejects handled by tag"),
        }
    }

    /// Rejects that halt the loop (for the UTC day) rather than skip a tick.
    pub fn halts_day(&self) -> bool {
        matches!(self, RiskReject::DailyLossBreached { .. })
    }
}

impl fmt::Display for RiskReject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiskReject::DailyLossBreached { loss_usd, cap_usd } => {
                write!(f, "daily loss ${loss_usd:.4} ≥ cap ${cap_usd:.2}")
            }
            RiskReject::LlmBudgetExhausted { spent_usd, cap_usd } => {
                write!(f, "llm spend ${spent_usd:.4} ≥ cap ${cap_usd:.2}")
            }
            RiskReject::BadQuote => write!(f, "quote not finite/positive/uncrossed"),
            RiskReject::PairNotTradable => write!(f, "pair not api-tradable"),
            RiskReject::InvalidSize { size_usd } => write!(f, "size ${size_usd} invalid"),
            RiskReject::OrderTooLarge { size_usd, cap_usd } => {
                write!(f, "size ${size_usd:.4} > max_order_usd ${cap_usd:.2}")
            }
            RiskReject::TooManyOpenOrders { open, max } => {
                write!(f, "{open} open order(s) ≥ max_open_orders {max}")
            }
            _ => self.fmt_order(f),
        }
    }
}

impl RiskReject {
    fn fmt_order(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiskReject::MinTickInterval {
                since_secs,
                min_secs,
            } => {
                write!(f, "{since_secs}s since last order < min {min_secs}s")
            }
            RiskReject::PositionCapExceeded {
                position_usd,
                size_usd,
                cap_usd,
            } => write!(
                f,
                "position ${position_usd:.4} + ${size_usd:.4} > max_position_usd ${cap_usd:.2}"
            ),
            RiskReject::InsufficientCash {
                needed_usd,
                cash_usd,
            } => write!(f, "need ${needed_usd:.4} > cash ${cash_usd:.4}"),
            RiskReject::InsufficientHoldings { qty, held } => {
                write!(f, "sell {qty} > held {held} (no shorts)")
            }
            RiskReject::RoundsToZero => write!(f, "quantity rounds to zero at pair increment"),
            RiskReject::BelowMinOrderAmount {
                notional_usd,
                min_usd,
            } => write!(f, "notional ${notional_usd:.4} < pair min ${min_usd:.2}"),
            RiskReject::AboveMaxOrderSize { qty, max_qty } => {
                write!(f, "qty {qty} > pair max {max_qty}")
            }
            _ => unreachable!("preflight rejects handled by Display"),
        }
    }
}
