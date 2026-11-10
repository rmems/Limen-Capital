//! Publish simulated MarketPulse frames on LIMEN_IPC_SUB (default tcp://127.0.0.1:5555).
//!
//! Usage:
//!   cargo run --bin market_sim
//!   LIMEN_SIM_HZ=10 cargo run --bin market_sim

use spikenaut_execution_engine::universe::TradeUniverse;
use spikenaut_execution_engine::wire::{market_endpoint, MarketPulse};
use spikenaut_execution_engine::zmq_curve;
use std::f32::consts::PI;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::info;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let universe = TradeUniverse::load_default()?;
    info!(
        "market_sim universe: {} symbols (hash 0x{:016x})",
        universe.n_assets(),
        universe.hash
    );

    let endpoint = market_endpoint()?;
    let hz: f64 = std::env::var("LIMEN_SIM_HZ")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);
    let period = Duration::from_secs_f64(1.0 / hz.max(0.1));

    let ctx = zmq::Context::new();
    let _zap = zmq_curve::maybe_start_zap(&ctx).map_err(|e| format!("ZAP setup: {e}"))?;
    let publisher = ctx.socket(zmq::PUB)?;
    if zmq_curve::curve_enabled() {
        let use_zap = _zap.is_some();
        let secret = zmq_curve::market_server_secret_z85()?;
        zmq_curve::apply_curve_server(&publisher, &secret, use_zap)?;
        info!("market_sim CURVE enabled (ZAP allowlist={use_zap})");
    }
    publisher.bind(&endpoint)?;
    info!("market_sim PUB bound to {endpoint} @ {hz} Hz");
    thread::sleep(Duration::from_millis(200));

    let n = universe.n_assets();
    let mut pulse = MarketPulse::new_for_universe(&universe);
    let mut t = 0u64;
    loop {
        t += 1;
        let now_ns = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
        pulse.timestamp_ns = now_ns;
        let phase = (t as f32) * 0.05;
        for i in 0..n {
            let wiggle = (phase + i as f32 * 0.3).sin() * 0.002;
            pulse.prices[i] = (1.0 + 0.01 * i as f32) * (1.0 + wiggle);
            pulse.vols[i] = 0.05 + 0.01 * ((phase * PI).sin().abs());
        }
        pulse.gpu_temp_c = 45.0;
        pulse.confidence_signal = 0.9;

        let buf = pulse.pack_v2()?;
        publisher.send(&buf[..], 0)?;
        if t.is_multiple_of(50) {
            info!("published tick={t} price0={:.4}", pulse.prices[0]);
        }
        thread::sleep(period);
    }
}
