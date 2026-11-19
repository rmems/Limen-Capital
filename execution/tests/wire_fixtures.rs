use limen_execution::universe::TradeUniverse;
use limen_execution::wire::{
    decode_market_pulse, decode_readout, pack_market_pulse, pack_readout, MarketPulse,
    ReadoutPacket,
};
use limen_execution::wire::{readout_to_trade_signal, resolve_fill_price};
use limen_execution::{ExecutionDecision, ExecutionEngine, TradeSide};
use std::path::Path;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../wire/fixtures")
            .join(name),
    )
    .expect("committed fixture must exist")
}

#[test]
fn golden_readout_with_production_config_passes_confidence_but_has_no_kelly_edge() {
    let u = TradeUniverse::load(&TradeUniverse::default_config_path()).unwrap();
    let packet = decode_readout(&fixture("readout.bin"), &u).unwrap();
    let pulse = decode_market_pulse(&fixture("marketpulse.bin"), &u).unwrap();
    let mut signal = readout_to_trade_signal(&packet, &u, 0.0, 1.0, None);
    signal.price =
        resolve_fill_price(Some(&pulse), &u, &signal.ticker, pulse.timestamp_ns, 1).unwrap();
    assert_eq!(signal.side, TradeSide::Buy);
    assert_eq!(signal.price, 1.0);
    // Independently computed from score vector [0.7, -0.1, 0, ...].
    let expected = 0.4 * 0.5_f64.sqrt().tanh();
    assert!((signal.confidence as f64 - expected).abs() < 1e-7);
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("ghost_trades.jsonl");
    let mut engine = ExecutionEngine::default().with_log_path(log.to_str().unwrap().into());
    assert!(signal.passes_gate(engine.confidence_threshold));
    assert!(matches!(
        engine.process_signal(signal),
        ExecutionDecision::RejectedNoEdge {
            kelly_fraction: 0.0,
            ..
        }
    ));
    assert_eq!(engine.ghost_wallet.balance_atp, 500.0);
    assert_eq!(engine.ghost_wallet.balance(&u.symbols[0]), 0.0);
    assert_eq!(engine.ghost_wallet.trade_count, 0);
    assert_eq!(engine.step_counter, 0);
    assert!(!log.exists());
}

#[test]
fn committed_fixtures_pin_writers_and_all_decoded_fields() {
    let u = TradeUniverse::load(&TradeUniverse::default_config_path()).unwrap();
    let pulse = MarketPulse {
        timestamp_ns: 1_700_000_000_000_000_001,
        symbol_table_hash: 0x72b1_5f3e_ee1d_443e,
        prices: vec![1.0, 1.1, 1.2, 1.3, 1.4, 1.5, 1.6],
        vols: vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7],
        confidence_signal: 0.9,
        funding_rate: 0.0001,
        liquidation_vol: 1_000_000.0,
        liquidity_delta: 0.05,
        l3_order_imbalance: 0.1,
        gpu_temp_c: 45.0,
        gpu_power_w: 200.0,
        gpu_util_pct: 0.5,
        basys_buffer_load: 0.1,
        dydx_oi_delta: 0.01,
        dydx_funding_rate: 0.0002,
    };
    let v1 = fixture("marketpulse.bin");
    let v2 = fixture("marketpulse_v2.bin");
    assert_eq!(pulse.pack_v1_legacy().unwrap().as_slice(), v1);
    assert_eq!(pack_market_pulse(&pulse).unwrap(), v2);
    assert_eq!(decode_market_pulse(&v1, &u).unwrap(), pulse);
    assert_eq!(decode_market_pulse(&v2, &u).unwrap(), pulse);

    let mut packet = ReadoutPacket {
        tick: 42,
        symbol_table_hash: 0x72b1_5f3e_ee1d_443e,
        readout: vec![
            0.8, 0.1, 0.2, 0.3, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ],
        relevance: [0.4, 0.3, 0.2, 0.1],
    };
    let v1 = fixture("readout.bin");
    assert_eq!(packet.pack_v1_legacy().unwrap().as_slice(), v1);
    assert_eq!(decode_readout(&v1, &u).unwrap(), packet);
    packet.readout[4..].copy_from_slice(&[
        0.04, 0.05, 0.06, 0.07, 0.08, 0.09, 0.10, 0.11, 0.12, 0.13, 0.14, 0.15,
    ]);
    let v2 = fixture("readout_v2.bin");
    assert_eq!(pack_readout(&packet).unwrap(), v2);
    assert_eq!(decode_readout(&v2, &u).unwrap(), packet);
}
