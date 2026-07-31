//! Publish simulated MarketPulse frames on LIMEN_IPC_SUB (default tcp://127.0.0.1:5555).
//!
//! Usage:
//!   cargo run --bin market_sim
//!   LIMEN_SIM_HZ=10 cargo run --bin market_sim

use spikenaut_execution_engine::wire::MarketPulse;
use std::f32::consts::PI;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::info;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let endpoint =
        std::env::var("LIMEN_IPC_SUB").unwrap_or_else(|_| "tcp://127.0.0.1:5555".to_string());
    let hz: f64 = std::env::var("LIMEN_SIM_HZ")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);
    let period = Duration::from_secs_f64(1.0 / hz.max(0.1));

    let ctx = zmq::Context::new();
    let publisher = ctx.socket(zmq::PUB)?;
    // Brain SUB connects; feed PUB binds
    publisher.bind(&endpoint)?;
    info!("market_sim PUB bound to {endpoint} @ {hz} Hz");
    // Brief settle for SUB connections
    thread::sleep(Duration::from_millis(200));

    let mut pulse = MarketPulse::default();
    let mut t = 0u64;
    loop {
        t += 1;
        let now_ns = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
        pulse.timestamp_ns = now_ns;
        let phase = (t as f32) * 0.05;
        for i in 0..7 {
            let wiggle = (phase + i as f32 * 0.3).sin() * 0.002;
            pulse.prices[i] = (1.0 + 0.01 * i as f32) * (1.0 + wiggle);
            pulse.vols[i] = 0.05 + 0.01 * ((phase * PI).sin().abs());
        }
        pulse.gpu_temp_c = 45.0;
        pulse.confidence_signal = 0.9;

        let buf = pulse.pack();
        publisher.send(&buf[..], 0)?;
        if t.is_multiple_of(50) {
            info!("published tick={t} price0={:.4}", pulse.prices[0]);
        }
        thread::sleep(period);
    }
}
