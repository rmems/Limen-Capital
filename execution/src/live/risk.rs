//! Hard limits, enforced in code, **before** any HTTP call to the venue.
//!
//! The decider never sees or edits these caps. `check` is a pure function of
//! (decision, state, pair rules, limits, timing inputs) and either approves a
//! fully-specified limit order or rejects with a typed reason.
//!
//! Defaults are the $10 run from RM-2436:
//!
//! | cap                          | default |
//! |------------------------------|---------|
//! | `max_position_usd`           | 10.0    |
//! | `max_order_usd`              | 5.0 (buys; sells are bounded by holdings and `max_position_usd`) |
//! | `max_daily_loss_usd`         | 4.0 → loop stops for the UTC day |
//! | `max_llm_cost_usd_per_day`   | 1.0 → decider disabled, loop idles |
//! | `min_tick_interval_secs`     | 900     |
//! | `max_open_orders`            | 1 (stale unfilled limit cancelled after `stale_order_ticks` = 2) |
//!
//! Buy limit price = bid (post-only intent); sell limit price = ask.
//! No shorts: sells exceeding holdings are rejected. Buys exceeding cash are
//! rejected. Sizes are rounded to the pair's increments and the rounded
//! notional must still clear the pair's minimum.

use crate::live::decider::{Action, Decision};
use crate::live::market_state::MarketState;
use crate::live::robinhood::types::{OrderSide, TradingPair};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RiskLimits {
    pub max_position_usd: f64,
    pub max_order_usd: f64,
    pub max_daily_loss_usd: f64,
    pub max_llm_cost_usd_per_day: f64,
    pub min_tick_interval_secs: u64,
    pub max_open_orders: usize,
    /// Unfilled limit orders older than this many ticks are cancelled.
    pub stale_order_ticks: u64,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_position_usd: 10.0,
            max_order_usd: 5.0,
            max_daily_loss_usd: 4.0,
            max_llm_cost_usd_per_day: 1.0,
            min_tick_interval_secs: 900,
            max_open_orders: 1,
            stale_order_ticks: 2,
        }
    }
}

impl RiskLimits {
    pub fn validate(&self) -> Result<(), String> {
        let pos = |name: &str, v: f64| {
            if v.is_finite() && v > 0.0 {
                Ok(())
            } else {
                Err(format!("{name} must be a positive finite number, got {v}"))
            }
        };
        pos("max_position_usd", self.max_position_usd)?;
        pos("max_order_usd", self.max_order_usd)?;
        pos("max_daily_loss_usd", self.max_daily_loss_usd)?;
        pos("max_llm_cost_usd_per_day", self.max_llm_cost_usd_per_day)?;
        if self.max_order_usd > self.max_position_usd {
            return Err("max_order_usd must not exceed max_position_usd".into());
        }
        if self.max_open_orders == 0 {
            return Err("max_open_orders must be >= 1".into());
        }
        if self.stale_order_ticks == 0 {
            return Err("stale_order_ticks must be >= 1".into());
        }
        Ok(())
    }
}

/// Venue rules for the pair, lifted from `trading_pairs`.
#[derive(Debug, Clone, PartialEq)]
pub struct PairRules {
    pub symbol: String,
    pub asset_increment: f64,
    pub quote_increment: f64,
    pub min_order_amount: Option<f64>,
    pub max_order_size: Option<f64>,
    pub is_api_tradable: bool,
}

impl From<&TradingPair> for PairRules {
    fn from(p: &TradingPair) -> Self {
        Self {
            symbol: p.symbol.clone(),
            asset_increment: p.asset_increment,
            quote_increment: p.quote_increment,
            min_order_amount: p.min_order_amount,
            max_order_size: p.max_order_size,
            is_api_tradable: p.is_api_tradable,
        }
    }
}

impl PairRules {
    /// Reasonable BTC-USD defaults for tests / synthetic paper runs.
    pub fn btc_usd_defaults() -> Self {
        Self {
            symbol: "BTC-USD".into(),
            asset_increment: 1e-8,
            quote_increment: 0.01,
            min_order_amount: Some(1.0),
            max_order_size: Some(100.0),
            is_api_tradable: true,
        }
    }
}

/// Timing / book inputs that are not part of `MarketState`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RiskInputs {
    pub now_secs: i64,
    /// When the last order was *submitted* (any side), if any today.
    pub last_order_secs: Option<i64>,
    pub open_orders: usize,
    /// Realized PnL for the current UTC day (negative = loss).
    pub daily_pnl_usd: f64,
    pub llm_cost_today_usd: f64,
    /// Notional of resting (unfilled) orders. Resting buys will add exposure
    /// when they fill; resting sells mean the position they reserve is still
    /// held (and `MarketState::position_qty` excludes reserved units), so both
    /// count towards the position cap.
    pub resting_notional_usd: f64,
}

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
            RiskReject::MinTickInterval { .. } => "min_tick_interval",
            RiskReject::PositionCapExceeded { .. } => "position_cap_exceeded",
            RiskReject::InsufficientCash { .. } => "insufficient_cash",
            RiskReject::InsufficientHoldings { .. } => "insufficient_holdings",
            RiskReject::RoundsToZero => "rounds_to_zero",
            RiskReject::BelowMinOrderAmount { .. } => "below_min_order_amount",
            RiskReject::AboveMaxOrderSize { .. } => "above_max_order_size",
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
        }
    }
}

/// A fully specified limit order the loop may submit (or paper-log).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovedOrder {
    pub client_order_id: String,
    pub symbol: String,
    pub side: OrderSide,
    pub limit_price: f64,
    pub asset_qty: f64,
    /// `asset_qty × limit_price`.
    pub notional_usd: f64,
    /// Wire strings, already rounded to the pair increments.
    pub limit_price_str: String,
    pub asset_qty_str: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RiskVerdict {
    /// Decider said hold; nothing to do.
    Hold,
    Approved(ApprovedOrder),
    Rejected(RiskReject),
}

impl RiskVerdict {
    /// Journal label.
    pub fn label(&self) -> String {
        match self {
            RiskVerdict::Hold => "hold".into(),
            RiskVerdict::Approved(o) => format!(
                "approved {} {} @ {} (${:.4})",
                o.side, o.asset_qty_str, o.limit_price_str, o.notional_usd
            ),
            RiskVerdict::Rejected(r) => format!("rejected:{} ({r})", r.tag()),
        }
    }
}

/// Checks that apply before spending a decider call: daily loss and LLM budget.
pub fn pre_tick_check(limits: &RiskLimits, inputs: &RiskInputs) -> Option<RiskReject> {
    let loss = -inputs.daily_pnl_usd;
    if loss >= limits.max_daily_loss_usd {
        return Some(RiskReject::DailyLossBreached {
            loss_usd: loss,
            cap_usd: limits.max_daily_loss_usd,
        });
    }
    if inputs.llm_cost_today_usd >= limits.max_llm_cost_usd_per_day {
        return Some(RiskReject::LlmBudgetExhausted {
            spent_usd: inputs.llm_cost_today_usd,
            cap_usd: limits.max_llm_cost_usd_per_day,
        });
    }
    None
}

/// Number of decimals needed to represent multiples of an increment exactly
/// (`0.01` → 2, `1e-8` → 8, `0.25` → 2, `0.5` → 1, `1` → 0).
pub fn decimals_for_increment(inc: f64) -> usize {
    if !(inc.is_finite() && inc > 0.0) {
        return 8;
    }
    for d in 0..=18usize {
        let scaled = inc * 10f64.powi(d as i32);
        let r = scaled.round();
        // `scaled` must be a positive integer: a tiny increment scaled by a
        // small power rounds to 0, which must not count as representable.
        if r >= 1.0 && (scaled - r).abs() < 1e-6 {
            return d;
        }
    }
    18
}

/// Round `x` down to a multiple of `inc` (with a tiny epsilon so exact
/// multiples are not knocked down by float noise).
pub fn round_down_to_increment(x: f64, inc: f64) -> f64 {
    if !(inc.is_finite() && inc > 0.0) || !x.is_finite() {
        return x;
    }
    let steps = (x / inc + 1e-9).floor();
    let d = decimals_for_increment(inc);
    let p = 10f64.powi(d as i32);
    ((steps * inc) * p).round() / p
}

/// Round `x` up to a multiple of `inc`.
pub fn round_up_to_increment(x: f64, inc: f64) -> f64 {
    if !(inc.is_finite() && inc > 0.0) || !x.is_finite() {
        return x;
    }
    let steps = (x / inc - 1e-9).ceil();
    let d = decimals_for_increment(inc);
    let p = 10f64.powi(d as i32);
    ((steps * inc) * p).round() / p
}

/// Format with exactly the increment's decimals (what the venue expects).
pub fn format_at_increment(x: f64, inc: f64) -> String {
    format!("{:.*}", decimals_for_increment(inc), x)
}

/// Approve or reject `decision` for `state`. Pure.
pub fn check(
    decision: &Decision,
    state: &MarketState,
    pair: &PairRules,
    limits: &RiskLimits,
    inputs: &RiskInputs,
) -> RiskVerdict {
    use RiskReject as R;
    use RiskVerdict::Rejected;

    if decision.is_hold() {
        return RiskVerdict::Hold;
    }
    // Defensive: the loop runs pre_tick_check first, but never trust the caller.
    if let Some(r) = pre_tick_check(limits, inputs) {
        return Rejected(r);
    }
    if !state.quote_is_sane() {
        return Rejected(R::BadQuote);
    }
    if !pair.is_api_tradable || !pair.symbol.eq_ignore_ascii_case(&state.symbol) {
        return Rejected(R::PairNotTradable);
    }
    let size = decision.size_usd;
    if !(size.is_finite() && size > 0.0) {
        return Rejected(R::InvalidSize { size_usd: size });
    }
    // `max_order_usd` bounds *new exposure*, i.e. buys. A sell never adds
    // exposure and the position it reduces was already approved under
    // `max_position_usd`, so sells are bounded by that cap (and, below, by
    // holdings). Otherwise a $5 buy that appreciates to $5.03 could never be
    // exited.
    let order_cap = match decision.action {
        Action::Buy => limits.max_order_usd,
        Action::Sell => limits.max_position_usd,
        Action::Hold => unreachable!("hold handled above"),
    };
    if size > order_cap + 1e-9 {
        return Rejected(R::OrderTooLarge {
            size_usd: size,
            cap_usd: order_cap,
        });
    }
    if inputs.open_orders >= limits.max_open_orders {
        return Rejected(R::TooManyOpenOrders {
            open: inputs.open_orders,
            max: limits.max_open_orders,
        });
    }
    if let Some(last) = inputs.last_order_secs {
        let since = inputs.now_secs - last;
        if since < limits.min_tick_interval_secs as i64 {
            return Rejected(R::MinTickInterval {
                since_secs: since,
                min_secs: limits.min_tick_interval_secs,
            });
        }
    }

    let (side, raw_limit) = match decision.action {
        Action::Buy => (OrderSide::Buy, state.bid),
        Action::Sell => (OrderSide::Sell, state.ask),
        Action::Hold => unreachable!("hold handled above"),
    };
    // Buy: round the bid down (never pay above bid). Sell: round the ask up.
    let limit_price = match side {
        OrderSide::Buy => round_down_to_increment(raw_limit, pair.quote_increment),
        OrderSide::Sell => round_up_to_increment(raw_limit, pair.quote_increment),
    };
    if !(limit_price.is_finite() && limit_price > 0.0) {
        return Rejected(R::BadQuote);
    }

    let mut qty = round_down_to_increment(size / limit_price, pair.asset_increment);

    match side {
        OrderSide::Buy => {
            let committed = state.position_usd + inputs.resting_notional_usd.max(0.0);
            if committed + size > limits.max_position_usd + 1e-9 {
                return Rejected(R::PositionCapExceeded {
                    position_usd: committed,
                    size_usd: size,
                    cap_usd: limits.max_position_usd,
                });
            }
            if size > state.cash_usd + 1e-9 {
                return Rejected(R::InsufficientCash {
                    needed_usd: size,
                    cash_usd: state.cash_usd,
                });
            }
        }
        OrderSide::Sell => {
            let held = round_down_to_increment(state.position_qty, pair.asset_increment);
            // Tolerate one increment of float slop when the decider asks for "everything".
            if qty > held + pair.asset_increment {
                return Rejected(R::InsufficientHoldings { qty, held });
            }
            if qty > held {
                qty = held;
            }
            // Never strand dust: if what would remain is below the pair's
            // minimum order notional it could never be sold, so sell it all.
            let remainder_usd = (held - qty) * limit_price;
            if qty < held && remainder_usd < pair.min_order_amount.unwrap_or(0.0) {
                qty = held;
            }
        }
    }

    if qty <= 0.0 {
        return Rejected(R::RoundsToZero);
    }
    if let Some(max_qty) = pair.max_order_size {
        if qty > max_qty {
            return Rejected(R::AboveMaxOrderSize { qty, max_qty });
        }
    }
    let notional = qty * limit_price;
    if let Some(min) = pair.min_order_amount {
        if notional + 1e-9 < min {
            return Rejected(R::BelowMinOrderAmount {
                notional_usd: notional,
                min_usd: min,
            });
        }
    }

    RiskVerdict::Approved(ApprovedOrder {
        client_order_id: uuid::Uuid::new_v4().to_string(),
        symbol: state.symbol.clone(),
        side,
        limit_price,
        asset_qty: qty,
        notional_usd: notional,
        limit_price_str: format_at_increment(limit_price, pair.quote_increment),
        asset_qty_str: format_at_increment(qty, pair.asset_increment),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(bid: f64, ask: f64, position_qty: f64, cash: f64) -> MarketState {
        MarketState::new(
            1_700_000_000,
            1,
            "BTC-USD",
            bid,
            ask,
            position_qty,
            if position_qty > 0.0 { Some(bid) } else { None },
            cash,
            0.0,
            vec![],
        )
    }

    fn inputs() -> RiskInputs {
        RiskInputs {
            now_secs: 1_700_000_000,
            last_order_secs: None,
            open_orders: 0,
            daily_pnl_usd: 0.0,
            llm_cost_today_usd: 0.0,
            resting_notional_usd: 0.0,
        }
    }

    fn pair() -> PairRules {
        PairRules::btc_usd_defaults()
    }

    fn limits() -> RiskLimits {
        RiskLimits::default()
    }

    fn reject(v: RiskVerdict) -> RiskReject {
        match v {
            RiskVerdict::Rejected(r) => r,
            other => panic!("expected reject, got {other:?}"),
        }
    }

    fn approved(v: RiskVerdict) -> ApprovedOrder {
        match v {
            RiskVerdict::Approved(o) => o,
            other => panic!("expected approval, got {other:?}"),
        }
    }

    #[test]
    fn defaults_match_the_issue() {
        let l = RiskLimits::default();
        assert_eq!(l.max_position_usd, 10.0);
        assert_eq!(l.max_order_usd, 5.0);
        assert_eq!(l.max_daily_loss_usd, 4.0);
        assert_eq!(l.max_llm_cost_usd_per_day, 1.0);
        assert_eq!(l.min_tick_interval_secs, 900);
        assert_eq!(l.max_open_orders, 1);
        assert_eq!(l.stale_order_ticks, 2);
        l.validate().unwrap();
    }

    #[test]
    fn validate_rejects_nonsense_limits() {
        let l = RiskLimits {
            max_order_usd: 11.0,
            ..Default::default()
        };
        assert!(l.validate().is_err());
        let l = RiskLimits {
            max_daily_loss_usd: 0.0,
            ..Default::default()
        };
        assert!(l.validate().is_err());
        let l = RiskLimits {
            max_open_orders: 0,
            ..Default::default()
        };
        assert!(l.validate().is_err());
        let l = RiskLimits {
            max_position_usd: f64::NAN,
            ..Default::default()
        };
        assert!(l.validate().is_err());
    }

    #[test]
    fn limits_deserialize_with_defaults_and_deny_unknown() {
        let l: RiskLimits = toml::from_str("max_order_usd = 1.0").unwrap();
        assert_eq!(l.max_order_usd, 1.0);
        assert_eq!(l.max_position_usd, 10.0);
        assert!(toml::from_str::<RiskLimits>("leverage = 3").is_err());
    }

    #[test]
    fn hold_is_hold() {
        let v = check(
            &Decision::hold("x"),
            &state(100.0, 101.0, 0.0, 10.0),
            &pair(),
            &limits(),
            &inputs(),
        );
        assert_eq!(v, RiskVerdict::Hold);
        assert_eq!(v.label(), "hold");
    }

    #[test]
    fn happy_path_buy_uses_bid_and_rounds_to_increments() {
        let s = state(99_999.995, 100_010.0, 0.0, 10.0);
        let o = approved(check(
            &Decision::buy(5.0, "r"),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert_eq!(o.side, OrderSide::Buy);
        assert_eq!(o.limit_price, 99_999.99); // bid rounded DOWN to 0.01
        assert_eq!(o.limit_price_str, "99999.99");
        assert_eq!(o.asset_qty_str, "0.00005000");
        assert!((o.asset_qty - 0.00005).abs() < 1e-12);
        assert!(o.notional_usd <= 5.0 && o.notional_usd > 4.99);
        assert_eq!(o.symbol, "BTC-USD");
        assert_eq!(
            uuid::Uuid::parse_str(&o.client_order_id)
                .unwrap()
                .get_version_num(),
            4
        );
    }

    #[test]
    fn happy_path_sell_uses_ask_rounded_up_and_clamps_to_holdings() {
        let s = state(100_000.0, 100_010.001, 0.00004, 6.0);
        // Ask for slightly more than the held value (~$4.0004): within one
        // increment of slop, so it is clamped to holdings rather than rejected.
        let o = approved(check(
            &Decision::sell(4.002, "exit"),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert_eq!(o.side, OrderSide::Sell);
        assert_eq!(o.limit_price, 100_010.01); // ask rounded UP
        assert!(
            (o.asset_qty - 0.00004).abs() < 1e-12,
            "clamped to holdings: {}",
            o.asset_qty
        );
        assert_eq!(o.asset_qty_str, "0.00004000");
    }

    #[test]
    fn sell_of_an_appreciated_position_is_not_trapped_by_max_order_usd() {
        // Bought $5.00 of BTC; it is now worth $5.30. Exiting must be allowed.
        let s = state(106_000.0, 106_010.0, 0.00005, 5.0);
        assert!(s.position_usd > 5.0);
        let o = approved(check(
            &Decision::sell(s.position_usd, "exit"),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert_eq!(o.side, OrderSide::Sell);
        assert!(
            (o.asset_qty - 0.00005).abs() < 1e-12,
            "whole position: {}",
            o.asset_qty
        );
        // But a sell can never exceed max_position_usd either.
        let r = reject(check(
            &Decision::sell(10.01, "exit"),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert!(matches!(r, RiskReject::OrderTooLarge { cap_usd, .. } if cap_usd == 10.0));
        // And buys are still capped at max_order_usd.
        let r = reject(check(
            &Decision::buy(5.3, ""),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert!(matches!(r, RiskReject::OrderTooLarge { cap_usd, .. } if cap_usd == 5.0));
    }

    #[test]
    fn sell_never_strands_unsellable_dust() {
        // Selling "position_usd" at the (higher) ask rounds qty down below
        // holdings; the remainder (< $1 pair minimum) would be unsellable.
        let s = state(100_000.0, 100_010.0, 0.00005, 5.0);
        let o = approved(check(
            &Decision::sell(s.position_usd, "exit"),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert!((o.asset_qty - 0.00005).abs() < 1e-12, "{}", o.asset_qty);
        // A genuine partial sell that leaves a sellable remainder is untouched.
        let big = state(100_000.0, 100_010.0, 0.00008, 2.0); // holds $8
        let o = approved(check(
            &Decision::sell(5.0, "trim"),
            &big,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert!(o.asset_qty < 0.00008 && o.asset_qty > 0.00004);
    }

    #[test]
    fn client_order_ids_are_unique() {
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        let a = approved(check(
            &Decision::buy(2.0, ""),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        let b = approved(check(
            &Decision::buy(2.0, ""),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert_ne!(a.client_order_id, b.client_order_id);
    }

    #[test]
    fn rejects_order_over_max_order_usd() {
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        let r = reject(check(
            &Decision::buy(5.01, ""),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert!(matches!(r, RiskReject::OrderTooLarge { cap_usd, .. } if cap_usd == 5.0));
        assert_eq!(r.tag(), "order_too_large");
        // Exactly at the cap is fine.
        approved(check(
            &Decision::buy(5.0, ""),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
    }

    #[test]
    fn rejects_position_cap() {
        // Holding ~$6, buying $5 would exceed $10.
        let s = state(100_000.0, 100_010.0, 0.00006, 4.0);
        let r = reject(check(
            &Decision::buy(5.0, ""),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert!(matches!(r, RiskReject::PositionCapExceeded { .. }));
        // $3.99 fits the cap but the cash check is later; $3.9 fits both.
        approved(check(
            &Decision::buy(3.9, ""),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
    }

    #[test]
    fn position_cap_counts_resting_order_notional() {
        // Flat book with $5.50 of buys already resting and max_open_orders
        // raised: another $5 buy would commit $10.50 > $10.
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        let mut i = inputs();
        i.resting_notional_usd = 5.5;
        let l = RiskLimits {
            max_open_orders: 3,
            ..Default::default()
        };
        let r = reject(check(&Decision::buy(5.0, ""), &s, &pair(), &l, &i));
        assert!(
            matches!(r, RiskReject::PositionCapExceeded { position_usd, .. } if (position_usd - 5.5).abs() < 1e-9),
            "{r:?}"
        );
        // Exactly at the cap with the resting order counted is still fine.
        i.resting_notional_usd = 5.0;
        approved(check(&Decision::buy(5.0, ""), &s, &pair(), &l, &i));
    }

    #[test]
    fn rejects_buy_exceeding_cash() {
        let s = state(100_000.0, 100_010.0, 0.0, 2.0);
        let r = reject(check(
            &Decision::buy(2.5, ""),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert!(matches!(r, RiskReject::InsufficientCash { cash_usd, .. } if cash_usd == 2.0));
    }

    #[test]
    fn rejects_sell_exceeding_holdings_no_shorts() {
        let s = state(100_000.0, 100_010.0, 0.00001, 9.0); // holds $1
        let r = reject(check(
            &Decision::sell(3.0, ""),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert!(matches!(r, RiskReject::InsufficientHoldings { .. }));
        // Flat book: any sell is a short.
        let flat = state(100_000.0, 100_010.0, 0.0, 10.0);
        let r = reject(check(
            &Decision::sell(1.0, ""),
            &flat,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert!(matches!(r, RiskReject::InsufficientHoldings { held, .. } if held == 0.0));
    }

    #[test]
    fn rejects_daily_loss_breach_and_halts_day() {
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        let mut i = inputs();
        i.daily_pnl_usd = -4.0;
        assert!(matches!(
            pre_tick_check(&limits(), &i),
            Some(RiskReject::DailyLossBreached { .. })
        ));
        let r = reject(check(&Decision::buy(1.0, ""), &s, &pair(), &limits(), &i));
        assert!(r.halts_day());
        i.daily_pnl_usd = -3.99;
        assert!(pre_tick_check(&limits(), &i).is_none());
    }

    #[test]
    fn rejects_when_llm_budget_exhausted() {
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        let mut i = inputs();
        i.llm_cost_today_usd = 1.0;
        let r = reject(check(&Decision::buy(1.0, ""), &s, &pair(), &limits(), &i));
        assert!(matches!(r, RiskReject::LlmBudgetExhausted { .. }));
        assert!(!r.halts_day());
    }

    #[test]
    fn rejects_min_tick_interval() {
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        let mut i = inputs();
        i.last_order_secs = Some(i.now_secs - 899);
        let r = reject(check(&Decision::buy(1.0, ""), &s, &pair(), &limits(), &i));
        assert!(matches!(
            r,
            RiskReject::MinTickInterval {
                since_secs: 899,
                min_secs: 900
            }
        ));
        i.last_order_secs = Some(i.now_secs - 900);
        approved(check(&Decision::buy(1.0, ""), &s, &pair(), &limits(), &i));
    }

    #[test]
    fn rejects_too_many_open_orders() {
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        let mut i = inputs();
        i.open_orders = 1;
        let r = reject(check(&Decision::buy(1.0, ""), &s, &pair(), &limits(), &i));
        assert!(matches!(
            r,
            RiskReject::TooManyOpenOrders { open: 1, max: 1 }
        ));
    }

    #[test]
    fn rejects_below_pair_min_order_amount() {
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        let r = reject(check(
            &Decision::buy(0.5, ""),
            &s,
            &pair(),
            &limits(),
            &inputs(),
        ));
        assert!(matches!(r, RiskReject::BelowMinOrderAmount { min_usd, .. } if min_usd == 1.0));
    }

    #[test]
    fn rejects_quantity_that_rounds_to_zero() {
        let mut p = pair();
        p.asset_increment = 0.001; // coarse increment: $1 buys 0.00001 BTC → rounds to 0
        p.min_order_amount = None;
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        let r = reject(check(&Decision::buy(1.0, ""), &s, &p, &limits(), &inputs()));
        assert_eq!(r, RiskReject::RoundsToZero);
    }

    #[test]
    fn rejects_above_pair_max_order_size() {
        let mut p = pair();
        p.max_order_size = Some(0.00001);
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        let r = reject(check(&Decision::buy(2.0, ""), &s, &p, &limits(), &inputs()));
        assert!(matches!(r, RiskReject::AboveMaxOrderSize { .. }));
    }

    #[test]
    fn rejects_invalid_sizes() {
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let r = reject(check(
                &Decision::buy(bad, ""),
                &s,
                &pair(),
                &limits(),
                &inputs(),
            ));
            assert!(matches!(r, RiskReject::InvalidSize { .. }), "{bad}");
        }
    }

    #[test]
    fn rejects_bad_quotes_and_untradable_pair() {
        let crossed = state(100_010.0, 100_000.0, 0.0, 10.0);
        assert_eq!(
            reject(check(
                &Decision::buy(1.0, ""),
                &crossed,
                &pair(),
                &limits(),
                &inputs()
            )),
            RiskReject::BadQuote
        );
        let zero = state(0.0, 0.0, 0.0, 10.0);
        assert_eq!(
            reject(check(
                &Decision::buy(1.0, ""),
                &zero,
                &pair(),
                &limits(),
                &inputs()
            )),
            RiskReject::BadQuote
        );
        let mut p = pair();
        p.is_api_tradable = false;
        let s = state(100_000.0, 100_010.0, 0.0, 10.0);
        assert_eq!(
            reject(check(&Decision::buy(1.0, ""), &s, &p, &limits(), &inputs())),
            RiskReject::PairNotTradable
        );
        let mut p = pair();
        p.symbol = "ETH-USD".into();
        assert_eq!(
            reject(check(&Decision::buy(1.0, ""), &s, &p, &limits(), &inputs())),
            RiskReject::PairNotTradable
        );
    }

    #[test]
    fn every_reject_variant_has_a_tag_and_display() {
        let all = [
            RiskReject::DailyLossBreached {
                loss_usd: 4.0,
                cap_usd: 4.0,
            },
            RiskReject::LlmBudgetExhausted {
                spent_usd: 1.0,
                cap_usd: 1.0,
            },
            RiskReject::BadQuote,
            RiskReject::PairNotTradable,
            RiskReject::InvalidSize { size_usd: 0.0 },
            RiskReject::OrderTooLarge {
                size_usd: 6.0,
                cap_usd: 5.0,
            },
            RiskReject::TooManyOpenOrders { open: 1, max: 1 },
            RiskReject::MinTickInterval {
                since_secs: 1,
                min_secs: 900,
            },
            RiskReject::PositionCapExceeded {
                position_usd: 9.0,
                size_usd: 2.0,
                cap_usd: 10.0,
            },
            RiskReject::InsufficientCash {
                needed_usd: 2.0,
                cash_usd: 1.0,
            },
            RiskReject::InsufficientHoldings {
                qty: 1.0,
                held: 0.0,
            },
            RiskReject::RoundsToZero,
            RiskReject::BelowMinOrderAmount {
                notional_usd: 0.5,
                min_usd: 1.0,
            },
            RiskReject::AboveMaxOrderSize {
                qty: 2.0,
                max_qty: 1.0,
            },
        ];
        let mut tags: Vec<&str> = all.iter().map(|r| r.tag()).collect();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), all.len(), "tags must be distinct");
        for r in &all {
            assert!(!r.to_string().is_empty());
            let label = RiskVerdict::Rejected(r.clone()).label();
            assert!(label.starts_with("rejected:"));
        }
    }

    #[test]
    fn increment_helpers() {
        assert_eq!(decimals_for_increment(0.01), 2);
        assert_eq!(decimals_for_increment(1e-8), 8);
        assert_eq!(decimals_for_increment(1.0), 0);
        assert_eq!(decimals_for_increment(0.25), 2);
        assert_eq!(decimals_for_increment(0.5), 1);
        assert_eq!(decimals_for_increment(0.001), 3);
        // Non-power-of-ten steps stay on grid and never round upwards.
        assert_eq!(round_down_to_increment(0.75, 0.25), 0.75);
        assert_eq!(round_down_to_increment(0.8, 0.25), 0.75);
        assert_eq!(round_down_to_increment(100.9, 0.25), 100.75);
        assert_eq!(round_up_to_increment(100.8, 0.25), 101.0);
        assert_eq!(format_at_increment(0.75, 0.25), "0.75");
        assert_eq!(round_down_to_increment(0.123456789, 1e-8), 0.12345678);
        assert_eq!(round_down_to_increment(100.0, 0.01), 100.0);
        assert_eq!(round_up_to_increment(100.001, 0.01), 100.01);
        assert_eq!(round_up_to_increment(100.0, 0.01), 100.0);
        assert_eq!(format_at_increment(0.00005, 1e-8), "0.00005000");
        assert_eq!(format_at_increment(99999.99, 0.01), "99999.99");
    }
}
