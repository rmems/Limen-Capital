use limen_execution::{ExecutionDecision, ExecutionEngine, TradeSide, TradeSignal};

fn signal(side: TradeSide, price: f64, quantity: f64, confidence: f32) -> TradeSignal {
    TradeSignal {
        ticker: "TEST".into(),
        side,
        price,
        quantity,
        confidence,
        timestamp_ns: 0,
    }
}

fn engine(dir: &tempfile::TempDir, limit: f64, kelly: bool) -> ExecutionEngine {
    ExecutionEngine::new(0.15, limit)
        .with_kelly_sizing(kelly)
        .with_log_path(dir.path().join("trades.jsonl").to_str().unwrap().into())
}

fn executed(decision: ExecutionDecision) -> (f64, f64) {
    match decision {
        ExecutionDecision::Executed {
            quantity, price, ..
        } => (quantity, price),
        other => panic!("expected a committed fill, got {other:?}"),
    }
}

#[test]
fn known_confidence_fills_policy_quantity_not_wallet_kelly() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(&dir, 10.0, true);
    let confidence = 0.953_f32;
    // Independent Kelly formula: ((p*b - (1-p))/b) * quarter-Kelly * ATP / price.
    let p = confidence as f64;
    let expected = (((p * 0.05 - (1.0 - p)) / 0.05) * 0.25 * 500.0) as f32;
    let before = engine.ghost_wallet.balance_atp;
    let (quantity, price) =
        executed(engine.process_signal(signal(TradeSide::Buy, 1.0, 99.0, confidence)));
    assert_eq!(quantity, expected as f64);
    assert_eq!(engine.ghost_wallet.balance("TEST"), expected);
    assert_eq!(price, 1.0);
    let expected_cash_delta = expected * 1.001;
    assert!((before - engine.ghost_wallet.balance_atp - expected_cash_delta).abs() < 0.00003);
    assert!(engine.ghost_wallet.balance("TEST") < 10.0);
    assert_eq!(engine.ghost_wallet.trade_count, 1);
    let log = std::fs::read_to_string(dir.path().join("trades.jsonl")).unwrap();
    let row: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(row["quantity"].as_f64().unwrap() as f32, expected);
}

#[test]
fn disabling_kelly_honors_buy_and_partial_sell_quantities() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(&dir, 10.0, false);
    assert_eq!(
        executed(engine.process_signal(signal(TradeSide::Buy, 2.0, 4.0, 0.5))).0,
        4.0
    );
    assert_eq!(engine.ghost_wallet.balance("TEST"), 4.0);
    assert_eq!(
        executed(engine.process_signal(signal(TradeSide::Sell, 2.0, 1.0, 0.5))).0,
        1.0
    );
    assert_eq!(engine.ghost_wallet.balance("TEST"), 3.0);
    assert_eq!(engine.ghost_wallet.trade_count, 2);
}

#[test]
fn flat_sell_and_oversell_are_rejected_without_fills() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(&dir, 10.0, true);
    assert!(matches!(
        engine.process_signal(signal(TradeSide::Sell, 1.0, 0.0, 0.953)),
        ExecutionDecision::RejectedFill { .. }
    ));
    assert_eq!(engine.ghost_wallet.trade_count, 0);
    assert_eq!(engine.ghost_wallet.balance_atp, 500.0);
    assert!(!dir.path().join("trades.jsonl").exists());
    engine.use_kelly_sizing = false;
    executed(engine.process_signal(signal(TradeSide::Buy, 1.0, 2.0, 0.5)));
    let before = engine.ghost_wallet.balance_atp;
    assert!(matches!(
        engine.process_signal(signal(TradeSide::Sell, 1.0, 3.0, 0.5)),
        ExecutionDecision::RejectedFill { .. }
    ));
    assert_eq!(engine.ghost_wallet.balance_atp, before);
    assert_eq!(engine.ghost_wallet.balance("TEST"), 2.0);
    assert_eq!(engine.ghost_wallet.trade_count, 1);
}

#[test]
fn position_limit_covers_individual_and_accumulated_fills() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(&dir, 10.0, false);
    assert!(matches!(
        engine.process_signal(signal(TradeSide::Buy, 1.0, 10.1, 0.5)),
        ExecutionDecision::RejectedPositionLimit { .. }
    ));
    executed(engine.process_signal(signal(TradeSide::Buy, 1.0, 6.0, 0.5)));
    assert!(matches!(
        engine.process_signal(signal(TradeSide::Buy, 1.0, 6.0, 0.5)),
        ExecutionDecision::RejectedPositionLimit { .. }
    ));
    executed(engine.process_signal(signal(TradeSide::Buy, 1.0, 4.0, 0.5)));
    assert_eq!(engine.ghost_wallet.balance("TEST"), 10.0);
    assert!(matches!(
        engine.process_signal(signal(TradeSide::Buy, 1.0, 1.0, 0.5)),
        ExecutionDecision::RejectedPositionLimit { .. }
    ));
    executed(engine.process_signal(signal(TradeSide::Sell, 1.0, 10.0, 0.5)));
    assert_eq!(engine.ghost_wallet.balance("TEST"), 0.0);
}

#[test]
fn flat_round_trip_pnl_equals_both_fees_and_cash_loss() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(&dir, 10.0, false);
    executed(engine.process_signal(signal(TradeSide::Buy, 10.0, 4.0, 0.5)));
    assert_eq!(engine.ghost_wallet.cumulative_pnl, 0.0); // fee held in basis
    executed(engine.process_signal(signal(TradeSide::Sell, 10.0, 1.0, 0.5)));
    assert!((engine.ghost_wallet.cumulative_pnl + 0.02).abs() < 0.00001);
    executed(engine.process_signal(signal(TradeSide::Sell, 10.0, 3.0, 0.5)));
    // 40 quote notional * 10 bps per side = 0.08 total friction.
    assert!((engine.ghost_wallet.cumulative_pnl + 0.08).abs() < 0.00001);
    // Three f32 cash updates can accumulate rounding at the 500-ATP scale.
    assert!(
        (engine.ghost_wallet.balance_atp as f64 - 499.92).abs() < 3.0 * f32::EPSILON as f64 * 500.0
    );
    assert_eq!(
        engine.ghost_wallet.summary().total_realized_pnl,
        engine.ghost_wallet.cumulative_pnl
    );
    assert_eq!(engine.ghost_wallet.win_rate(), Some(0.0));
    let log = std::fs::read_to_string(dir.path().join("trades.jsonl")).unwrap();
    let logged: f64 = log
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["realized_pnl_usdt"]
                .as_f64()
                .unwrap()
        })
        .sum();
    assert!((logged + 0.08).abs() < 0.00001);
}

#[test]
fn fee_and_log_failures_do_not_report_execution_or_mutate_wallet() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(&dir, 10.0, false);
    // Notional alone fits, but not notional plus fee.
    assert!(matches!(
        engine.process_signal(signal(TradeSide::Buy, 500.0, 1.0, 0.5)),
        ExecutionDecision::RejectedFill { .. }
    ));
    engine.log_path = dir.path().to_str().unwrap().into(); // a directory cannot be appended
    assert!(matches!(
        engine.process_signal(signal(TradeSide::Buy, 1.0, 1.0, 0.5)),
        ExecutionDecision::RejectedFill { .. }
    ));
    assert_eq!(engine.ghost_wallet.balance_atp, 500.0);
    assert_eq!(engine.ghost_wallet.trade_count, 0);
    assert!(engine.ghost_wallet.balances.is_empty());
    engine.log_path = dir.path().join("retry.jsonl").to_str().unwrap().into();
    executed(engine.process_signal(signal(TradeSide::Buy, 1.0, 1.0, 0.5)));
    assert_eq!(engine.ghost_wallet.trade_count, 1);
}

#[test]
fn invalid_or_unrepresentable_inputs_never_execute() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = engine(&dir, 10.0, false);
    for price in [
        0.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        f64::MAX,
        f64::MIN_POSITIVE,
    ] {
        assert!(!matches!(
            engine.process_signal(signal(TradeSide::Buy, price, 1.0, 0.5)),
            ExecutionDecision::Executed { .. }
        ));
    }
    for qty in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MIN_POSITIVE, 1e-30] {
        assert!(!matches!(
            engine.process_signal(signal(TradeSide::Buy, 1.0, qty, 0.5)),
            ExecutionDecision::Executed { .. }
        ));
    }
    assert_eq!(engine.ghost_wallet.trade_count, 0);
    assert_eq!(engine.ghost_wallet.balance_atp, 500.0);
}
