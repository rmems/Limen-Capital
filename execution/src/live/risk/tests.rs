use super::*;

pub(super) fn state(bid: f64, ask: f64, position_qty: f64, cash: f64) -> MarketState {
    MarketState::new(
        1_700_000_000,
        1,
        "BTC-USD",
        bid,
        ask,
        crate::live::market_state::AccountState {
            position_qty,
            avg_cost_usd: if position_qty > 0.0 { Some(bid) } else { None },
            cash_usd: cash,
            realized_pnl_usd: 0.0,
        },
        vec![],
    )
}

pub(super) fn inputs() -> RiskInputs {
    RiskInputs {
        now_secs: 1_700_000_000,
        last_order_secs: None,
        open_orders: 0,
        daily_pnl_usd: 0.0,
        llm_cost_today_usd: 0.0,
        resting_notional_usd: 0.0,
    }
}

pub(super) fn pair() -> PairRules {
    PairRules::btc_usd_defaults()
}

pub(super) fn limits() -> RiskLimits {
    RiskLimits::default()
}

pub(super) fn reject(v: RiskVerdict) -> RiskReject {
    match v {
        RiskVerdict::Rejected(r) => r,
        other => panic!("expected reject, got {other:?}"),
    }
}

pub(super) fn approved(v: RiskVerdict) -> ApprovedOrder {
    match v {
        RiskVerdict::Approved(o) => o,
        other => panic!("expected approval, got {other:?}"),
    }
}

mod orders;
mod rejects;
