//! Required in the cross-language CI job via scripts/test_julia_rust_wire.sh.
use limen_execution::universe::TradeUniverse;
use limen_execution::wire::{
    decode_market_pulse, decode_readout, readout_to_trade_signal, resolve_fill_price,
};
use limen_execution::{ExecutionDecision, ExecutionEngine, TradeSide};
use std::path::PathBuf;

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
}

#[test]
#[ignore = "requires Julia-produced frames; run scripts/test_julia_rust_wire.sh"]
fn julia_frames_reach_production_decisions_and_wallet() {
    // No fallback to fixtures: the runner must invoke the real Julia packers.
    let dir =
        PathBuf::from(std::env::var_os("LIMEN_JULIA_WIRE_DIR").expect("Julia frame directory"));
    let u = TradeUniverse::load(&TradeUniverse::default_config_path()).unwrap();
    let pulse = decode_market_pulse(&std::fs::read(dir.join("market.bin")).unwrap(), &u).unwrap();
    assert_eq!(pulse.timestamp_ns, 1_700_000_000_000_000_001);
    assert_eq!(
        pulse.prices,
        [100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0]
    );
    assert_eq!(pulse.vols, [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7]);
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("ghost_trades.jsonl");
    let mut engine = ExecutionEngine::default().with_log_path(log.to_str().unwrap().into());
    assert_eq!(engine.confidence_threshold, 0.15);
    assert_eq!(engine.max_position_size, 10.0);
    assert_eq!(engine.sizer.expected_payoff_ratio(), 0.05);

    for (name, side) in [("buy", TradeSide::Buy), ("sell", TradeSide::Sell)] {
        let packet =
            decode_readout(&std::fs::read(dir.join(format!("{name}.bin"))).unwrap(), &u).unwrap();
        assert_eq!(packet.tick, pulse.timestamp_ns as i64);
        assert_eq!(packet.relevance, [1.0, 0.0, 0.0, 0.0]);
        let mut expected_output = vec![0.0; u.n_out()];
        expected_output[usize::from(side == TradeSide::Sell)] = 3.0;
        assert_eq!(packet.readout, expected_output);
        let mut signal = readout_to_trade_signal(&packet, &u, 0.0, 999.0, None);
        assert_eq!(signal.side, side);
        assert_eq!(signal.ticker, u.symbols[0]);
        assert_eq!(signal.timestamp_ns, packet.tick);
        close(signal.confidence as f64, 3.0_f64.tanh());
        signal.price =
            resolve_fill_price(Some(&pulse), &u, &signal.ticker, pulse.timestamp_ns, 1).unwrap();
        assert_eq!(signal.price, 100.0);
        let atp = engine.ghost_wallet.balance_atp as f64;
        let held = engine.ghost_wallet.balance(&u.symbols[0]) as f64;
        let expected_policy_fraction =
            ((signal.confidence as f64 * 0.05 - (1.0 - signal.confidence as f64)) / 0.05) * 0.25;
        match engine.process_signal(signal) {
            ExecutionDecision::Executed {
                side: actual,
                quantity,
                kelly_fraction,
                price,
                ..
            } => {
                assert_eq!(actual, side);
                assert_eq!(price, 100.0);
                close(kelly_fraction, expected_policy_fraction);
                close(quantity, atp * expected_policy_fraction / 100.0);
            }
            other => panic!("expected {side:?} execution, got {other:?}"),
        }
        // Ledger fill sizing is independent of policy sizing (known limitation).
        if side == TradeSide::Buy {
            close(engine.ghost_wallet.balance_atp as f64, 460.0);
            close(engine.ghost_wallet.balance(&u.symbols[0]) as f64, 0.3996);
        } else {
            close(
                engine.ghost_wallet.balance_atp as f64,
                atp + held * 0.08 * 100.0 * 0.999,
            );
            close(
                engine.ghost_wallet.balance(&u.symbols[0]) as f64,
                held * 0.92,
            );
        }
    }
    let before = engine.ghost_wallet.balance_atp;
    for name in ["neutral", "low_confidence", "no_edge"] {
        let packet =
            decode_readout(&std::fs::read(dir.join(format!("{name}.bin"))).unwrap(), &u).unwrap();
        let signal = readout_to_trade_signal(&packet, &u, 100.0, 1.0, None);
        let result = engine.process_signal(signal);
        match name {
            "no_edge" => assert!(matches!(
                result,
                ExecutionDecision::RejectedNoEdge {
                    kelly_fraction: 0.0,
                    ..
                }
            )),
            _ => assert!(matches!(
                result,
                ExecutionDecision::RejectedLowConfidence { .. }
            )),
        }
        assert_eq!(engine.ghost_wallet.balance_atp, before);
        assert_eq!(engine.ghost_wallet.trade_count, 2);
        assert_eq!(engine.step_counter, 2);
    }
    let records: Vec<serde_json::Value> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    for (record, action, quantity) in [
        (&records[0], "buy", 0.3996),
        (&records[1], "sell", 0.031968),
    ] {
        assert_eq!(record["action"], action);
        assert_eq!(record["asset"], u.symbols[0]);
        assert_eq!(record["price_usd"], 100.0);
        close(record["quantity"].as_f64().unwrap(), quantity);
    }
}
