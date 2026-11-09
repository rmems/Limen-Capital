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

use spikenaut_execution_engine::universe::TradeUniverse;
use spikenaut_execution_engine::wire::{
    classify_wire_frame, decode_market_pulse, decode_readout, json_ipc_endpoint,
    load_trade_universe, market_endpoint, pulse_max_age_ns, readout_endpoint,
    readout_to_trade_signal, resolve_fill_price, MarketPulse,
};
use spikenaut_execution_engine::{zmq_curve, ExecutionEngine, TradeSignal};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

/// Poll interval for the blocking ZMQ receive loops, so they notice shutdown.
const RECV_TIMEOUT: Duration = Duration::from_millis(500);

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// Run until Ctrl+C or until the listener thread stops on its own.
///
/// A listener that exits (ZMQ error, bad frame, panic) is a fatal condition:
/// the process would otherwise sit idle looking healthy while processing
/// nothing. `stop` tells the listener to return so the runtime can shut down.
async fn supervise(
    name: &str,
    stop: Arc<AtomicBool>,
    listener: tokio::task::JoinHandle<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::pin!(listener);
    let result = tokio::select! {
        r = &mut listener => match r {
            Ok(()) => Err(format!("{name} listener exited; see log for the ZMQ error").into()),
            Err(e) => Err(format!("{name} listener panicked: {e}").into()),
        },
        r = tokio::signal::ctrl_c() => {
            r?;
            info!("Shutdown (Ctrl+C)");
            Ok(())
        }
    };
    stop.store(true, Ordering::Relaxed);
    // Give the blocking loop one RECV_TIMEOUT to observe `stop` and return.
    let _ = tokio::time::timeout(RECV_TIMEOUT * 2, &mut listener).await;
    result
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

fn dispatch_readout_frame(
    bytes: &[u8],
    universe: &Arc<TradeUniverse>,
    eng: &Arc<tokio::sync::Mutex<ExecutionEngine>>,
    pulse: &Arc<tokio::sync::Mutex<Option<MarketPulse>>>,
    unpriced: &Arc<AtomicU64>,
    max_age_ns: u64,
) {
    let packet = match decode_readout(bytes, universe) {
        Ok(p) => p,
        Err(e) => {
            error!("Readout decode failed: {e}");
            return;
        }
    };
    let mut sig = readout_to_trade_signal(&packet, universe, 0.0, 1.0, None);
    let engine_clone = eng.clone();
    let pulse_clone = pulse.clone();
    let unpriced = unpriced.clone();
    let universe_async = universe.clone();
    tokio::spawn(async move {
        let mut e = engine_clone.lock().await;
        let priced = {
            let p = pulse_clone.lock().await;
            resolve_fill_price(
                p.as_ref(),
                &universe_async,
                &sig.ticker,
                now_ns(),
                max_age_ns,
            )
        };
        match priced {
            Ok(px) => sig.price = px,
            Err(err) => {
                let n = unpriced.fetch_add(1, Ordering::Relaxed) + 1;
                if n == 1 || n.is_multiple_of(100) {
                    warn!(
                        "[{}] readout refused ({err}); {n} unpriced so far",
                        sig.ticker
                    );
                }
                return;
            }
        }
        let decision = e.process_signal(sig);
        tracing::debug!("Decision: {:?}", decision);
    });
}

fn process_binary_frame(
    bytes: &[u8],
    universe: &Arc<TradeUniverse>,
    eng: &Arc<tokio::sync::Mutex<ExecutionEngine>>,
    pulse: &Arc<tokio::sync::Mutex<Option<MarketPulse>>>,
    unpriced: &Arc<AtomicU64>,
    max_age_ns: u64,
) {
    let kind = match classify_wire_frame(bytes) {
        Ok(k) => k,
        Err(e) => {
            warn!("{e}");
            return;
        }
    };
    match kind {
        "readout_v2" | "readout_v1" => {
            dispatch_readout_frame(bytes, universe, eng, pulse, unpriced, max_age_ns);
        }
        "market_v2" | "market_v1" => match decode_market_pulse(bytes, universe) {
            Ok(mp) => {
                *pulse.blocking_lock() = Some(mp);
            }
            Err(e) => warn!("MarketPulse decode failed: {e}"),
        },
        _ => warn!("Unhandled wire frame kind {kind}"),
    }
}

async fn start_binary_listener(
    engine: Arc<tokio::sync::Mutex<ExecutionEngine>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let readout_ep = readout_endpoint();
    let market_ep = market_endpoint();
    let universe = Arc::new(load_trade_universe()?);
    info!(
        "Trade universe: {} symbols (hash 0x{:016x})",
        universe.n_assets(),
        universe.hash
    );
    let max_age_ns = pulse_max_age_ns()?;
    // Last MarketPulse; `None` until the feed delivers one. Fills are refused
    // without a fresh pulse rather than priced at a placeholder.
    let last_pulse: Arc<tokio::sync::Mutex<Option<MarketPulse>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    // Pre-pulse readouts arrive at tick rate; count refusals to throttle the log.
    let unpriced = Arc::new(AtomicU64::new(0));

    let context = zmq::Context::new();
    let _zap = zmq_curve::maybe_start_zap(&context).map_err(|e| format!("ZAP setup: {e}"))?;

    let timeout_ms = RECV_TIMEOUT.as_millis() as i32;
    let curve_on = zmq_curve::curve_enabled();

    let eng = engine.clone();
    let pulse = last_pulse.clone();
    let stop_flag = stop.clone();
    let unpriced_c = unpriced.clone();
    let max_age = max_age_ns;
    let universe_curve = universe.clone();
    let universe_plain = universe.clone();

    let listener = if curve_on {
        let client_sec = zmq_curve::client_secret_key_z85()?;
        let sub_readout = context.socket(zmq::SUB)?;
        zmq_curve::apply_curve_client(
            &sub_readout,
            &zmq_curve::readout_server_public_z85()?,
            &client_sec,
            None,
        )?;
        sub_readout.connect(&readout_ep)?;
        sub_readout.set_subscribe(b"")?;
        sub_readout.set_rcvtimeo(timeout_ms)?;

        let sub_market = context.socket(zmq::SUB)?;
        zmq_curve::apply_curve_client(
            &sub_market,
            &zmq_curve::market_server_public_z85()?,
            &client_sec,
            None,
        )?;
        sub_market.connect(&market_ep)?;
        sub_market.set_subscribe(b"")?;
        sub_market.set_rcvtimeo(timeout_ms)?;

        info!(
            "Binary CURVE SUBs: readout {readout_ep}, market {market_ep} (ZAP allowlist active: {})",
            _zap.is_some()
        );

        tokio::task::spawn_blocking(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                let mut items = [
                    sub_readout.as_poll_item(zmq::POLLIN),
                    sub_market.as_poll_item(zmq::POLLIN),
                ];
                if zmq::poll(&mut items, i64::from(timeout_ms)).unwrap_or(0) == 0 {
                    continue;
                }
                let sock = if items[0].is_readable() {
                    &sub_readout
                } else if items[1].is_readable() {
                    &sub_market
                } else {
                    continue;
                };
                let Ok(bytes) = sock.recv_bytes(0) else {
                    continue;
                };
                process_binary_frame(&bytes, &universe_curve, &eng, &pulse, &unpriced_c, max_age);
            }
        })
    } else {
        let subscriber = context.socket(zmq::SUB)?;
        subscriber.connect(&readout_ep)?;
        subscriber.connect(&market_ep)?;
        subscriber.set_subscribe(b"")?;
        subscriber.set_rcvtimeo(timeout_ms)?;
        info!(
            "Binary SUB connected to readout {readout_ep} and market {market_ep} (pulse max age {} ms)",
            max_age_ns / 1_000_000
        );

        tokio::task::spawn_blocking(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                let bytes = match subscriber.recv_bytes(0) {
                    Ok(b) => b,
                    Err(zmq::Error::EAGAIN) => continue,
                    Err(e) => {
                        error!("ZMQ error: {e}");
                        return;
                    }
                };
                process_binary_frame(&bytes, &universe_plain, &eng, &pulse, &unpriced_c, max_age);
            }
        })
    };

    supervise("binary", stop, listener).await
}

async fn start_json_listener(
    engine: Arc<tokio::sync::Mutex<ExecutionEngine>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let context = zmq::Context::new();
    let subscriber = context.socket(zmq::SUB)?;
    let ipc_addr = json_ipc_endpoint().map_err(|e| format!("JSON IPC setup: {e}"))?;
    subscriber.connect(&ipc_addr)?;
    subscriber.set_subscribe(b"")?;
    subscriber.set_rcvtimeo(RECV_TIMEOUT.as_millis() as i32)?;
    info!("JSON SUB connected to {ipc_addr}");

    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let listener = tokio::task::spawn_blocking(move || {
        while !stop_flag.load(Ordering::Relaxed) {
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
                // A non-UTF-8 frame is a bad message, not a dead socket.
                Ok(Err(_)) => error!("binary message on JSON socket; dropped"),
                Err(zmq::Error::EAGAIN) => continue,
                Err(e) => {
                    error!("ZMQ: {e}");
                    return;
                }
            }
        }
    });

    supervise("json", stop, listener).await
}
