//! Limen-Capital Execution Engine — binary NervousWire listener (v2).
//!
//! Subscribes to Julia brain **ReadoutPacket** frames on `LIMEN_IPC_PUB`
//! (default `tcp://127.0.0.1:5556`) and to the **MarketPulse** feed on
//! `LIMEN_IPC_SUB` (default `tcp://127.0.0.1:5555`, e.g. `market_sim`). Each
//! readout is priced from the last pulse, mapped to `TradeSignal`, run through
//! the confidence gates, and ghost-traded via metabolic-ledger. Readouts are
//! refused (not filled at a placeholder price) until a pulse has arrived, when
//! the pulse is older than `LIMEN_PULSE_MAX_AGE_MS` (default 2000), or when the
//! primary pair is RESIDUAL.
//!
//! Frames are processed in order on the listener thread. Each stream keeps a
//! timestamp watermark seeded at startup: duplicate, out-of-order, stale,
//! future, and pre-restart frames are refused. Endpoints must be loopback or
//! `ipc://` unless `LIMEN_ALLOW_REMOTE_WIRE=1`.
//!
//! Optional: set `LIMEN_WIRE=json` to use legacy JSON IPC.
//! Endpoint: `LIMEN_JSON_IPC` (must be `ipc://` + absolute path), else
//! `$XDG_RUNTIME_DIR/limen-capital/signals.ipc`, else
//! `/tmp/limen-capital-$UID/signals.ipc` (directory mode 0700).
//!
//! Usage:
//!   cargo run --release
//!   LIMEN_WIRE=json cargo run --release

use limen_execution::universe::TradeUniverse;
use limen_execution::wire::{
    classify_wire_frame, decode_market_pulse, decode_readout, json_ipc_endpoint,
    load_trade_universe, market_endpoint, pulse_max_age_ns, readout_endpoint,
    readout_to_trade_signal, resolve_fill_price, MarketPulse, TickGuard,
};
use limen_execution::{zmq_curve, ExecutionEngine, TradeSignal};
use std::sync::atomic::{AtomicBool, Ordering};
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
/// A listener that exits (ZMQ error, panic) is a fatal condition: the process
/// would otherwise sit idle looking healthy while processing nothing. `stop`
/// tells the listener to return so the runtime can shut down.
async fn supervise(
    name: &str,
    stop: Arc<AtomicBool>,
    listener: tokio::task::JoinHandle<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::pin!(listener);
    tokio::select! {
        r = &mut listener => {
            stop.store(true, Ordering::Relaxed);
            // A completed JoinHandle must not be polled again.
            match r {
                Ok(()) => Err(format!("{name} listener exited; see log for the ZMQ error").into()),
                Err(e) => Err(format!("{name} listener panicked: {e}").into()),
            }
        },
        r = tokio::signal::ctrl_c() => {
            r?;
            info!("Shutdown (Ctrl+C)");
            stop.store(true, Ordering::Relaxed);
            // Give the blocking loop one RECV_TIMEOUT to observe `stop` and return.
            let _ = tokio::time::timeout(RECV_TIMEOUT * 2, &mut listener).await;
            Ok(())
        }
    }
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
    let engine = ExecutionEngine::new(confidence_threshold, max_position_size);

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

/// Listener-owned state; frames are handled one at a time, so no locks.
struct BinaryState {
    engine: ExecutionEngine,
    universe: TradeUniverse,
    pulse: Option<MarketPulse>,
    readout_ticks: TickGuard,
    pulse_ticks: TickGuard,
    unpriced: u64,
    max_age_ns: u64,
}

impl BinaryState {
    fn new(
        engine: ExecutionEngine,
        universe: TradeUniverse,
        start_ns: u64,
        max_age_ns: u64,
    ) -> Self {
        Self {
            engine,
            universe,
            pulse: None,
            readout_ticks: TickGuard::new(start_ns),
            pulse_ticks: TickGuard::new(start_ns),
            unpriced: 0,
            max_age_ns,
        }
    }

    fn process(&mut self, bytes: &[u8], now: u64) {
        if let Err(e) = self.process_checked(bytes, now) {
            warn!("Binary frame refused: {e}");
        }
    }

    fn process_checked(&mut self, bytes: &[u8], now: u64) -> Result<(), String> {
        // Header-less v1 frames are typed only by length, so they are
        // decodable for regression fixtures but refused on the network.
        match classify_wire_frame(bytes)? {
            "readout_v2" => self.on_readout(bytes, now),
            "market_v2" => self.on_market(bytes, now),
            kind => Err(format!("{kind} frame refused; publishers must send v2")),
        }
    }

    fn on_market(&mut self, bytes: &[u8], now: u64) -> Result<(), String> {
        let mp = decode_market_pulse(bytes, &self.universe)?;
        if !self
            .pulse_ticks
            .accept(mp.timestamp_ns, now, self.max_age_ns)
        {
            return Err("non-increasing, pre-start, stale, or future market timestamp".into());
        }
        self.pulse = Some(mp);
        Ok(())
    }

    fn on_readout(&mut self, bytes: &[u8], now: u64) -> Result<(), String> {
        let packet = decode_readout(bytes, &self.universe)?;
        let tick = u64::try_from(packet.tick).map_err(|_| "negative readout timestamp")?;
        if !self.readout_ticks.accept(tick, now, self.max_age_ns) {
            return Err("non-increasing, pre-start, stale, or future readout timestamp".into());
        }
        let mut sig = readout_to_trade_signal(&packet, &self.universe, 0.0, 1.0, None);
        let priced = resolve_fill_price(
            self.pulse.as_ref(),
            &self.universe,
            &sig.ticker,
            now,
            self.max_age_ns,
        );
        match priced {
            Ok(px) => sig.price = px,
            Err(err) => {
                self.note_unpriced(&sig.ticker, &err.to_string());
                return Ok(());
            }
        }
        let decision = self.engine.process_signal(sig);
        tracing::debug!("Decision: {:?}", decision);
        Ok(())
    }

    fn note_unpriced(&mut self, ticker: &str, err: &str) {
        self.unpriced += 1;
        // Pre-pulse readouts arrive at tick rate; throttle the log.
        if self.unpriced == 1 || self.unpriced.is_multiple_of(100) {
            warn!(
                "[{ticker}] readout refused ({err}); {} unpriced so far",
                self.unpriced
            );
        }
    }
}

fn curve_sub(
    context: &zmq::Context,
    server_public: &str,
    client_secret: &str,
    endpoint: &str,
    timeout_ms: i32,
) -> Result<zmq::Socket, Box<dyn std::error::Error>> {
    let sock = context.socket(zmq::SUB)?;
    zmq_curve::apply_curve_client(&sock, server_public, client_secret, None)?;
    sock.connect(endpoint)?;
    sock.set_subscribe(b"")?;
    sock.set_rcvtimeo(timeout_ms)?;
    Ok(sock)
}

/// Wait for one frame on either socket. `Ok(None)` means nothing arrived.
fn poll_pair(
    readout: &zmq::Socket,
    market: &zmq::Socket,
    timeout_ms: i32,
) -> Result<Option<Vec<u8>>, zmq::Error> {
    let mut items = [
        readout.as_poll_item(zmq::POLLIN),
        market.as_poll_item(zmq::POLLIN),
    ];
    match zmq::poll(&mut items, i64::from(timeout_ms)) {
        Ok(0) | Err(zmq::Error::EINTR) => return Ok(None),
        Ok(_) => (),
        Err(e) => return Err(e),
    }
    let sock = if items[0].is_readable() {
        readout
    } else if items[1].is_readable() {
        market
    } else {
        return Ok(None);
    };
    recv_frame(sock)
}

fn recv_frame(sock: &zmq::Socket) -> Result<Option<Vec<u8>>, zmq::Error> {
    match sock.recv_bytes(0) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(zmq::Error::EAGAIN) | Err(zmq::Error::EINTR) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Feed frames to `state` until `stop` is set or the socket reports an error.
fn run_listener(
    stop: &AtomicBool,
    state: &mut BinaryState,
    mut next: impl FnMut() -> Result<Option<Vec<u8>>, zmq::Error>,
) {
    while !stop.load(Ordering::Relaxed) {
        match next() {
            Ok(Some(bytes)) => state.process(&bytes, now_ns()),
            Ok(None) => continue,
            Err(e) => {
                error!("ZMQ error: {e}");
                return;
            }
        }
    }
}

async fn start_binary_listener(engine: ExecutionEngine) -> Result<(), Box<dyn std::error::Error>> {
    let readout_ep = readout_endpoint()?;
    let market_ep = market_endpoint()?;
    let universe = load_trade_universe()?;
    info!(
        "Trade universe: {} symbols (hash 0x{:016x})",
        universe.n_assets(),
        universe.hash
    );
    let max_age_ns = pulse_max_age_ns()?;
    let mut state = BinaryState::new(engine, universe, now_ns(), max_age_ns);
    let stop = Arc::new(AtomicBool::new(false));

    let context = zmq::Context::new();
    let _zap = zmq_curve::maybe_start_zap(&context).map_err(|e| format!("ZAP setup: {e}"))?;
    let timeout_ms = RECV_TIMEOUT.as_millis() as i32;
    let stop_flag = stop.clone();

    let listener = if zmq_curve::curve_enabled() {
        let client_sec = zmq_curve::client_secret_key_z85()?;
        let readout_pub = zmq_curve::readout_server_public_z85()?;
        let market_pub = zmq_curve::market_server_public_z85()?;
        let sub_readout = curve_sub(&context, &readout_pub, &client_sec, &readout_ep, timeout_ms)?;
        let sub_market = curve_sub(&context, &market_pub, &client_sec, &market_ep, timeout_ms)?;
        info!(
            "Binary CURVE SUBs: readout {readout_ep}, market {market_ep} (ZAP allowlist active: {})",
            _zap.is_some()
        );
        tokio::task::spawn_blocking(move || {
            run_listener(&stop_flag, &mut state, || {
                poll_pair(&sub_readout, &sub_market, timeout_ms)
            });
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
            run_listener(&stop_flag, &mut state, || recv_frame(&subscriber));
        })
    };

    supervise("binary", stop, listener).await
}

async fn start_json_listener(
    mut engine: ExecutionEngine,
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
                        let _ = engine.process_signal(signal);
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

#[cfg(test)]
mod tests {
    use super::*;
    use limen_execution::wire::{pack_market_pulse, pack_readout, ReadoutPacket};

    fn universe() -> TradeUniverse {
        TradeUniverse::load(&TradeUniverse::default_config_path()).expect("default universe")
    }

    fn state() -> BinaryState {
        BinaryState::new(ExecutionEngine::default(), universe(), 100, 100)
    }

    fn readout(tick: i64) -> Vec<u8> {
        let u = universe();
        pack_readout(&ReadoutPacket {
            tick,
            symbol_table_hash: u.hash,
            readout: vec![0.0; u.n_out()],
            relevance: [0.25; 4],
        })
        .unwrap()
    }

    fn market(timestamp_ns: u64, price: f32) -> Vec<u8> {
        let mut p = MarketPulse::new_for_universe(&universe());
        p.timestamp_ns = timestamp_ns;
        p.prices.iter_mut().for_each(|px| *px = price);
        pack_market_pulse(&p).unwrap()
    }

    #[test]
    fn inline_listener_refuses_replays_even_when_original_was_unpriced() {
        let mut state = state();
        state.process_checked(&readout(101), 101).unwrap();
        assert_eq!(state.unpriced, 1);
        state.process_checked(&market(102, 10.0), 102).unwrap();
        assert!(state.process_checked(&readout(101), 102).is_err());
        assert!(state.process_checked(&market(102, 20.0), 102).is_err());
        assert!(state.process_checked(&market(101, 20.0), 102).is_err());
        assert_eq!(state.pulse.as_ref().unwrap().prices[0], 10.0);
        state.process_checked(&readout(103), 103).unwrap();
        assert_eq!(state.unpriced, 1);
        assert!(state.process_checked(&readout(102), 103).is_err());
        assert!(state.process_checked(&readout(104), 205).is_err());
        state.process_checked(&readout(205), 205).unwrap();
        assert_eq!(state.unpriced, 2); // last pulse is now stale
    }

    #[test]
    fn invalid_frame_does_not_poison_later_valid_frames() {
        let mut state = state();
        for tick in [-1, 99, 100, 202] {
            assert!(state.process_checked(&readout(tick), 201).is_err());
        }
        let mut bad = readout(201);
        bad[28..32].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(state.process_checked(&bad, 201).is_err());
        bad = readout(201);
        bad[4] = 3;
        assert!(state.process_checked(&bad, 201).is_err());
        bad = readout(201);
        bad[6] = 1;
        assert!(state.process_checked(&bad, 201).is_err());
        let v1 = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../wire/fixtures/readout.bin"
        ))
        .unwrap();
        assert!(state.process_checked(&v1, 201).is_err());
        state.process_checked(&market(201, 10.0), 201).unwrap();
        state.process_checked(&readout(201), 201).unwrap();
        assert_eq!(state.unpriced, 0);
    }

    #[tokio::test]
    async fn exited_listener_is_reported_without_repolling_completed_handle() {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = tokio::task::spawn_blocking(|| ());
        assert!(supervise("test", stop.clone(), handle).await.is_err());
        assert!(stop.load(Ordering::Relaxed));
    }
}
