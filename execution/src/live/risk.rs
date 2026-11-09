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
//! | `max_order_usd`              | 5.0 (buys only; sells are bounded by holdings) |
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
mod increments;
mod reject;
pub use increments::{
    decimals_for_increment, format_at_increment, round_down_to_increment, round_up_to_increment,
};
pub use reject::RiskReject;

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

fn positive_limit(name: &str, v: f64) -> Result<(), String> {
    if v.is_finite() && v > 0.0 {
        Ok(())
    } else {
        Err(format!("{name} must be a positive finite number, got {v}"))
    }
}

impl RiskLimits {
    pub fn validate(&self) -> Result<(), String> {
        positive_limit("max_position_usd", self.max_position_usd)?;
        positive_limit("max_order_usd", self.max_order_usd)?;
        positive_limit("max_daily_loss_usd", self.max_daily_loss_usd)?;
        positive_limit("max_llm_cost_usd_per_day", self.max_llm_cost_usd_per_day)?;
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

/// Approve or reject `decision` for `state`. Pure.
pub fn check(
    decision: &Decision,
    state: &MarketState,
    pair: &PairRules,
    limits: &RiskLimits,
    inputs: &RiskInputs,
) -> RiskVerdict {
    if decision.is_hold() {
        return RiskVerdict::Hold;
    }
    match approve_order(decision, state, pair, limits, inputs) {
        Ok(order) => RiskVerdict::Approved(order),
        Err(reason) => RiskVerdict::Rejected(reason),
    }
}

/// Keep validation order explicit: daily budgets, proposal, cadence, price,
/// balances, then venue size bounds. The first rejection wins.
fn approve_order(
    decision: &Decision,
    state: &MarketState,
    pair: &PairRules,
    limits: &RiskLimits,
    inputs: &RiskInputs,
) -> Result<ApprovedOrder, RiskReject> {
    check_order_context(decision, state, pair, limits, inputs)?;
    let (side, limit_price) = order_limit(decision.action, state, pair)?;
    let raw_qty = round_down_to_increment(decision.size_usd / limit_price, pair.asset_increment);
    let qty = match side {
        OrderSide::Buy => {
            check_buy_balance(decision.size_usd, state, limits, inputs)?;
            raw_qty
        }
        OrderSide::Sell => sell_quantity(raw_qty, limit_price, state, pair)?,
    };
    let notional = qty * limit_price;
    check_pair_size(qty, notional, pair)?;
    Ok(ApprovedOrder {
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

fn check_order_context(
    decision: &Decision,
    state: &MarketState,
    pair: &PairRules,
    limits: &RiskLimits,
    inputs: &RiskInputs,
) -> Result<(), RiskReject> {
    if let Some(reason) = pre_tick_check(limits, inputs) {
        return Err(reason);
    }
    check_proposal(decision, state, pair, limits)?;
    check_cadence(inputs, limits)
}

fn check_proposal(
    decision: &Decision,
    state: &MarketState,
    pair: &PairRules,
    limits: &RiskLimits,
) -> Result<(), RiskReject> {
    if !state.quote_is_sane() {
        return Err(RiskReject::BadQuote);
    }
    if !pair.is_api_tradable || !pair.symbol.eq_ignore_ascii_case(&state.symbol) {
        return Err(RiskReject::PairNotTradable);
    }
    let size = decision.size_usd;
    if !(size.is_finite() && size > 0.0) {
        return Err(RiskReject::InvalidSize { size_usd: size });
    }
    // Only buys add exposure. Sells are bounded by holdings, not a USD cap.
    if decision.action == Action::Buy && size > limits.max_order_usd + 1e-9 {
        return Err(RiskReject::OrderTooLarge {
            size_usd: size,
            cap_usd: limits.max_order_usd,
        });
    }
    Ok(())
}

fn check_cadence(inputs: &RiskInputs, limits: &RiskLimits) -> Result<(), RiskReject> {
    if inputs.open_orders >= limits.max_open_orders {
        return Err(RiskReject::TooManyOpenOrders {
            open: inputs.open_orders,
            max: limits.max_open_orders,
        });
    }
    if let Some(last) = inputs.last_order_secs {
        let since = inputs.now_secs - last;
        if since < limits.min_tick_interval_secs as i64 {
            return Err(RiskReject::MinTickInterval {
                since_secs: since,
                min_secs: limits.min_tick_interval_secs,
            });
        }
    }
    Ok(())
}

fn order_limit(
    action: Action,
    state: &MarketState,
    pair: &PairRules,
) -> Result<(OrderSide, f64), RiskReject> {
    let (side, price) = match action {
        Action::Buy => (
            OrderSide::Buy,
            round_down_to_increment(state.bid, pair.quote_increment),
        ),
        Action::Sell => (
            OrderSide::Sell,
            round_up_to_increment(state.ask, pair.quote_increment),
        ),
        Action::Hold => unreachable!("hold handled by check"),
    };
    if !(price.is_finite() && price > 0.0) {
        return Err(RiskReject::BadQuote);
    }
    Ok((side, price))
}

fn check_buy_balance(
    size: f64,
    state: &MarketState,
    limits: &RiskLimits,
    inputs: &RiskInputs,
) -> Result<(), RiskReject> {
    let committed = state.position_usd + inputs.resting_notional_usd.max(0.0);
    if committed + size > limits.max_position_usd + 1e-9 {
        return Err(RiskReject::PositionCapExceeded {
            position_usd: committed,
            size_usd: size,
            cap_usd: limits.max_position_usd,
        });
    }
    if size > state.cash_usd + 1e-9 {
        return Err(RiskReject::InsufficientCash {
            needed_usd: size,
            cash_usd: state.cash_usd,
        });
    }
    Ok(())
}

fn sell_quantity(
    mut qty: f64,
    limit_price: f64,
    state: &MarketState,
    pair: &PairRules,
) -> Result<f64, RiskReject> {
    let held = round_down_to_increment(state.position_qty, pair.asset_increment);
    // Tolerate one increment of float slop when asking for everything.
    if qty > held + pair.asset_increment {
        return Err(RiskReject::InsufficientHoldings { qty, held });
    }
    if qty > held {
        qty = held;
    }
    // Sell the whole position rather than strand unsellable dust.
    let remainder_usd = (held - qty) * limit_price;
    if qty < held && remainder_usd < pair.min_order_amount.unwrap_or(0.0) {
        qty = held;
    }
    Ok(qty)
}

fn check_pair_size(qty: f64, notional: f64, pair: &PairRules) -> Result<(), RiskReject> {
    if qty <= 0.0 {
        return Err(RiskReject::RoundsToZero);
    }
    if let Some(max_qty) = pair.max_order_size {
        if qty > max_qty {
            return Err(RiskReject::AboveMaxOrderSize { qty, max_qty });
        }
    }
    if let Some(min) = pair.min_order_amount {
        if notional + 1e-9 < min {
            return Err(RiskReject::BelowMinOrderAmount {
                notional_usd: notional,
                min_usd: min,
            });
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests;
