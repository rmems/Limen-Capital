//! Limen-Capital Execution Engine — binary NervousWire listener (v1).
//!
//! Subscribes to Julia brain **ReadoutPacket** (88 B) on `LIMEN_IPC_PUB`
//! (default `tcp://127.0.0.1:5556`) and to the **MarketPulse** feed (120 B) on
//! `LIMEN_IPC_SUB` (default `tcp://127.0.0.1:5555`, e.g. `market_sim`). Each
//! readout is priced from the last pulse, mapped to `TradeSignal`, run through
//! the confidence gates, and ghost-traded via metabolic-ledger. Readouts are
//! refused (not filled at a placeholder price) until a pulse has arrived, when
//! the pulse is older than `LIMEN_PULSE_MAX_AGE_MS` (default 2000), or when the
//! primary pair is RESIDUAL.
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
    decode_readout, json_ipc_endpoint, market_endpoint, pulse_max_age_ns, readout_endpoint,
    readout_to_trade_signal, resolve_fill_price, MarketPulse, MARKET_PULSE_BYTES,
    READOUT_PACKET_BYTES,
};
use spikenaut_execution_engine::{ExecutionEngine, TradeSignal};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

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
    let readout_ep = readout_endpoint();
    let market_ep = market_endpoint();
    let max_age_ns = pulse_max_age_ns();
    // Last MarketPulse; `None` until the feed delivers one. Fills are refused
    // without a fresh pulse rather than priced at a placeholder.
    let last_pulse: Arc<tokio::sync::Mutex<Option<MarketPulse>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    let context = zmq::Context::new();
    let subscriber = context.socket(zmq::SUB)?;
    // Brain PUB binds :5556 and the feed PUB binds :5555; the muscle CONNECTS
    // one SUB to both. Frames are told apart by length (88 B vs 120 B).
    subscriber.connect(&readout_ep)?;
    subscriber.connect(&market_ep)?;
    subscriber.set_subscribe(b"")?;
    info!(
        "Binary SUB connected to readout {readout_ep} and market {market_ep} (pulse max age {} ms)",
        max_age_ns / 1_000_000
    );

    let eng = engine.clone();
    let pulse = last_pulse.clone();
    tokio::task::spawn_blocking(move || {
        let mut unpriced: u64 = 0;
        loop {
            match subscriber.recv_bytes(0) {
                Ok(bytes) => {
                    if bytes.len() == READOUT_PACKET_BYTES {
                        match decode_readout(&bytes) {
                            Ok(packet) => {
                                let mut sig = readout_to_trade_signal(&packet, 0.0, 1.0, None);
                                let priced = {
                                    let p = pulse.blocking_lock();
                                    resolve_fill_price(
                                        p.as_ref(),
                                        &sig.ticker,
                                        now_ns(),
                                        max_age_ns,
                                    )
                                };
                                match priced {
                                    Ok(px) => sig.price = px,
                                    Err(e) => {
                                        unpriced += 1;
                                        // Pre-pulse readouts arrive at tick rate: log the first, then every 100th.
                                        if unpriced == 1 || unpriced.is_multiple_of(100) {
                                            warn!(
                                                "[{}] readout refused ({e}); {unpriced} unpriced so far",
                                                sig.ticker
                                            );
                                        }
                                        continue;
                                    }
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
                    } else if bytes.len() == MARKET_PULSE_BYTES {
                        match spikenaut_execution_engine::wire::decode_market_pulse(&bytes) {
                            Ok(mp) => {
                                *pulse.blocking_lock() = Some(mp);
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
