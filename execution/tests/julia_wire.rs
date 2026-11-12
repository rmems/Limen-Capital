//! Required in the cross-language CI job via scripts/test_julia_rust_wire.sh.
use limen_execution::universe::TradeUniverse;
use limen_execution::wire::{
    decode_market_pulse, decode_readout, readout_to_trade_signal, resolve_fill_price, MarketPulse,
    ReadoutPacket,
};
use limen_execution::{ExecutionDecision, ExecutionEngine, TradeSide, TradeSignal};
use std::path::{Path, PathBuf};

/// Shared fixtures for one Julia → Rust decision pass.
struct WireRun {
    dir: PathBuf,
    universe: TradeUniverse,
    pulse: MarketPulse,
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
}

fn policy_fraction(confidence: f64) -> f64 {
    ((confidence * 0.05 - (1.0 - confidence)) / 0.05) * 0.25
}

fn assert_pulse_contract(pulse: &MarketPulse) {
    assert_eq!(pulse.timestamp_ns, 1_700_000_000_000_000_001);
    assert_eq!(
        pulse.prices,
        [100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0]
    );
    assert_eq!(pulse.vols, [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7]);
}

fn assert_default_engine(engine: &ExecutionEngine) {
    assert_eq!(engine.confidence_threshold, 0.15);
    assert_eq!(engine.max_position_size, 10.0);
    assert_eq!(engine.sizer.expected_payoff_ratio(), 0.05);
}

fn assert_packet_contract(packet: &ReadoutPacket, tick: i64, side: TradeSide) {
    assert_eq!(packet.tick, tick);
    assert_eq!(packet.relevance, [1.0, 0.0, 0.0, 0.0]);
    let mut expected_output = vec![0.0; packet.readout.len()];
    expected_output[usize::from(side == TradeSide::Sell)] = 3.0;
    assert_eq!(packet.readout, expected_output);
}

fn assert_signal_ids(signal: &TradeSignal, universe: &TradeUniverse, side: TradeSide, tick: i64) {
    assert_eq!(signal.side, side);
    assert_eq!(signal.ticker, universe.symbols[0]);
    assert_eq!(signal.timestamp_ns, tick);
}

fn assert_fill_sizing(quantity: f64, kelly_fraction: f64, atp: f64, fraction: f64) {
    close(kelly_fraction, fraction);
    close(quantity, atp * fraction / 100.0);
}

fn assert_buy_wallet(engine: &ExecutionEngine, universe: &TradeUniverse) {
    close(engine.ghost_wallet.balance_atp as f64, 387.86917);
    close(
        engine.ghost_wallet.balance(&universe.symbols[0]) as f64,
        1.120188,
    );
}

fn assert_sell_wallet(engine: &ExecutionEngine, universe: &TradeUniverse) {
    close(engine.ghost_wallet.balance_atp as f64, 474.67957);
    close(
        engine.ghost_wallet.balance(&universe.symbols[0]) as f64,
        0.25121522,
    );
}

fn assert_wallet_after(engine: &ExecutionEngine, universe: &TradeUniverse, side: TradeSide) {
    if side == TradeSide::Buy {
        assert_buy_wallet(engine, universe);
    } else {
        assert_sell_wallet(engine, universe);
    }
}

fn exercise_side(run: &WireRun, engine: &mut ExecutionEngine, name: &str, side: TradeSide) {
    let packet = decode_readout(
        &std::fs::read(run.dir.join(format!("{name}.bin"))).unwrap(),
        &run.universe,
    )
    .unwrap();
    assert_packet_contract(&packet, run.pulse.timestamp_ns as i64, side);
    let mut signal = readout_to_trade_signal(&packet, &run.universe, 0.0, 999.0, None);
    assert_signal_ids(&signal, &run.universe, side, packet.tick);
    close(signal.confidence as f64, 3.0_f64.tanh());
    signal.price = resolve_fill_price(
        Some(&run.pulse),
        &run.universe,
        &signal.ticker,
        run.pulse.timestamp_ns,
        1,
    )
    .unwrap();
    assert_eq!(signal.price, 100.0);
    let atp = engine.ghost_wallet.balance_atp as f64;
    let fraction = policy_fraction(signal.confidence as f64);
    let (filled_side, quantity, kelly_fraction, price) = match engine.process_signal(signal) {
        ExecutionDecision::Executed {
            side,
            quantity,
            kelly_fraction,
            price,
            ..
        } => (side, quantity, kelly_fraction, price),
        other => panic!("expected {side:?} execution, got {other:?}"),
    };
    assert_eq!(filled_side, side);
    assert_eq!(price, 100.0);
    assert_fill_sizing(quantity, kelly_fraction, atp, fraction);
    // Ledger fill sizing is independent of policy sizing (known limitation).
    assert_wallet_after(engine, &run.universe, side);
}

fn assert_rejection(run: &WireRun, engine: &mut ExecutionEngine, name: &str, before: f32) {
    let packet = decode_readout(
        &std::fs::read(run.dir.join(format!("{name}.bin"))).unwrap(),
        &run.universe,
    )
    .unwrap();
    let signal = readout_to_trade_signal(&packet, &run.universe, 100.0, 1.0, None);
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

fn assert_rejections(run: &WireRun, engine: &mut ExecutionEngine) {
    let before = engine.ghost_wallet.balance_atp;
    for name in ["neutral", "low_confidence", "no_edge"] {
        assert_rejection(run, engine, name, before);
    }
}

fn assert_record_price_and_quantity(record: &serde_json::Value, quantity: f64) {
    assert_eq!(record["price_usd"], 100.0);
    close(record["quantity"].as_f64().unwrap(), quantity);
}

fn assert_record(
    record: &serde_json::Value,
    action: &str,
    quantity: f64,
    universe: &TradeUniverse,
) {
    assert_eq!(record["action"], action);
    assert_eq!(record["asset"], universe.symbols[0]);
    assert_record_price_and_quantity(record, quantity);
}

fn assert_log_records(log: &Path, universe: &TradeUniverse) {
    let records: Vec<serde_json::Value> = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_record(&records[0], "buy", 1.120188, universe);
    assert_record(&records[1], "sell", 0.8689728, universe);
}

#[test]
#[ignore = "requires Julia-produced frames; run scripts/test_julia_rust_wire.sh"]
fn julia_frames_reach_production_decisions_and_wallet() {
    // No fallback to fixtures: the runner must invoke the real Julia packers.
    let dir =
        PathBuf::from(std::env::var_os("LIMEN_JULIA_WIRE_DIR").expect("Julia frame directory"));
    let universe = TradeUniverse::load(&TradeUniverse::default_config_path()).unwrap();
    let pulse =
        decode_market_pulse(&std::fs::read(dir.join("market.bin")).unwrap(), &universe).unwrap();
    assert_pulse_contract(&pulse);
    let run = WireRun {
        dir,
        universe,
        pulse,
    };
    let logs = tempfile::tempdir().unwrap();
    let log = logs.path().join("ghost_trades.jsonl");
    let mut engine = ExecutionEngine::default().with_log_path(log.to_str().unwrap().into());
    assert_default_engine(&engine);
    for (name, side) in [("buy", TradeSide::Buy), ("sell", TradeSide::Sell)] {
        exercise_side(&run, &mut engine, name, side);
    }
    assert_rejections(&run, &mut engine);
    assert_log_records(&log, &run.universe);
}
