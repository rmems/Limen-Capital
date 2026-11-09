use super::*;
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
    let all = all_rejects();
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

fn all_rejects() -> [RiskReject; 14] {
    [
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
    ]
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
