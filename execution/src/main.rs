//! Limen-Capital Execution Engine — binary NervousWire listener (v1).
//!
//! Subscribes to Julia brain **ReadoutPacket** (88 B) on `LIMEN_IPC_PUB`
//! (default `tcp://127.0.0.1:5556`), maps to `TradeSignal`, runs confidence
//! gates, and executes ghost trades via metabolic-ledger.
//!
//! Optional: set `LIMEN_WIRE=json` to use legacy JSON IPC.
//! Endpoint: `LIMEN_JSON_IPC` (must be `ipc://` + absolute path), else
//! `$XDG_RUNTIME_DIR/limen-capital/signals.ipc`, else
//! `/tmp/limen-capital-$UID/signals.ipc` (directory mode 0700).
//!
//! Usage:
//!   cargo run --release
//!   LIMEN_WIRE=json cargo run --release

use spikenaut_execution_engine::wire::{
    decode_readout, json_ipc_endpoint, price_for_ticker, readout_endpoint, readout_to_trade_signal,
    MarketPulse,
};
use spikenaut_execution_engine::{ExecutionEngine, TradeSignal};
use std::sync::Arc;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("Limen-Capital execution engine starting...");

    // Research default 0.15 (NERO*mag often ~0.2–0.4); override with LIMEN_CONFIDENCE_THRESHOLD
    let confidence_threshold: f32 = std::env::var("LIMEN_CONFIDENCE_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.15);
    let max_position_size = 10.0;
    let engine = Arc::new(tokio::sync::Mutex::new(ExecutionEngine::new(
        confidence_threshold,
        max_position_size,
    )));

    let mode = std::env::var("LIMEN_WIRE").unwrap_or_else(|_| "binary".to_string());
    if mode.eq_ignore_ascii_case("json") {
        info!("Wire mode: JSON adapter (legacy IPC)");
        start_json_listener(engine).await?;
    } else {
        info!("Wire mode: binary ReadoutPacket");
        start_binary_listener(engine).await?;
    }
    Ok(())
}

async fn start_binary_listener(
    engine: Arc<tokio::sync::Mutex<ExecutionEngine>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = readout_endpoint();
    // Last market prices for sizing (updated if publisher also sends pulses on same process later)
    let last_pulse = Arc::new(tokio::sync::Mutex::new(MarketPulse::default()));

    let context = zmq::Context::new();
    let subscriber = context.socket(zmq::SUB)?;
    // Brain PUB binds :5556; muscle CONNECTS
    subscriber.connect(&endpoint)?;
    subscriber.set_subscribe(b"")?;
    info!("Binary SUB connected to {endpoint}");

    let eng = engine.clone();
    let pulse = last_pulse.clone();
    tokio::task::spawn_blocking(move || loop {
        match subscriber.recv_bytes(0) {
            Ok(bytes) => {
                if bytes.len() == 88 {
                    match decode_readout(&bytes) {
                        Ok(packet) => {
                            let p = pulse.blocking_lock().clone();
                            // provisional price from pulse; quantity placeholder
                            let mut sig = readout_to_trade_signal(&packet, 1.0, 1.0, None);
                            sig.price = price_for_ticker(&p, &sig.ticker);
                            if sig.price <= 0.0 {
                                sig.price = 1.0;
                            }
                            let engine_clone = eng.clone();
                            tokio::spawn(async move {
                                let mut e = engine_clone.lock().await;
                                let decision = e.process_signal(sig);
                                tracing::debug!("Decision: {:?}", decision);
                            });
                        }
                        Err(e) => error!("Readout decode failed: {e}"),
                    }
                } else if bytes.len() == 120 {
                    match spikenaut_execution_engine::wire::decode_market_pulse(&bytes) {
                        Ok(mp) => {
                            *pulse.blocking_lock() = mp;
                        }
                        Err(e) => warn!("MarketPulse decode failed: {e}"),
                    }
                } else {
                    warn!("Unexpected binary frame {} bytes", bytes.len());
                }
            }
            Err(e) => {
                error!("ZMQ error: {e}");
                break;
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    info!("Shutdown (Ctrl+C)");
    Ok(())
}

async fn start_json_listener(
    engine: Arc<tokio::sync::Mutex<ExecutionEngine>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let context = zmq::Context::new();
    let subscriber = context.socket(zmq::SUB)?;
    let ipc_addr = json_ipc_endpoint().map_err(|e| format!("JSON IPC setup: {e}"))?;
    subscriber.connect(&ipc_addr)?;
    subscriber.set_subscribe(b"")?;
    info!("JSON SUB connected to {ipc_addr}");

    tokio::task::spawn_blocking(move || loop {
        match subscriber.recv_string(0) {
            Ok(Ok(msg)) => match serde_json::from_str::<TradeSignal>(&msg) {
                Ok(signal) => {
                    let engine_clone = engine.clone();
                    tokio::spawn(async move {
                        let mut eng = engine_clone.lock().await;
                        let _ = eng.process_signal(signal);
                    });
                }
                Err(e) => error!("JSON parse: {e}"),
            },
            Ok(Err(_)) => {
                error!("binary message on JSON socket");
                break;
            }
            Err(e) => {
                error!("ZMQ: {e}");
                break;
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    Ok(())
}
