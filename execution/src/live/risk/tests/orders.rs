use super::*;
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
    // A sell is bounded by holdings, not by a USD cap: asking for more
    // than is held is a short and is rejected as such.
    let r = reject(check(
        &Decision::sell(10.01, "exit"),
        &s,
        &pair(),
        &limits(),
        &inputs(),
    ));
    assert!(
        matches!(r, RiskReject::InsufficientHoldings { .. }),
        "{r:?}"
    );
    // A full-size position that appreciated above max_position_usd can still be exited.
    let big = state(210_000.0, 210_010.0, 0.00005, 0.0); // $10.50 held, bought at $10
    let o = approved(check(
        &Decision::sell(big.position_usd, "exit"),
        &big,
        &pair(),
        &limits(),
        &inputs(),
    ));
    assert!((o.asset_qty - 0.00005).abs() < 1e-12);
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
