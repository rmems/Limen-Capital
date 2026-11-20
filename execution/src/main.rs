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

    let config = limen_execution::config::ExecutionConfig::from_env()?;
    info!(
        "Research sizing: threshold={} b={} scalar={} max_order_notional={}; effective gate: p >= threshold AND p > {} (uncalibrated score, not a win probability)",
        config.confidence_threshold, config.kelly_b, config.kelly_scalar,
        config.max_order_notional, 1.0 / (1.0 + config.kelly_b)
    );
    let engine = config.engine();

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

/// Which socket to read: `Some(true)` = market, `Some(false)` = readout.
///
/// When both are readable the choice alternates, so a busy stream cannot
/// starve the other (stale pulses would otherwise refuse every readout).
fn pick_market(readout_ready: bool, market_ready: bool, market_next: &mut bool) -> Option<bool> {
    match (readout_ready, market_ready) {
        (true, true) => {
            let pick = *market_next;
            *market_next = !pick;
            Some(pick)
        }
        (false, true) => Some(true),
        (true, false) => Some(false),
        (false, false) => None,
    }
}

/// Wait for one frame on either socket. `Ok(None)` means nothing arrived.
fn poll_pair(
    readout: &zmq::Socket,
    market: &zmq::Socket,
    timeout_ms: i32,
    market_next: &mut bool,
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
    match pick_market(items[0].is_readable(), items[1].is_readable(), market_next) {
        Some(true) => recv_frame(market),
        Some(false) => recv_frame(readout),
        None => Ok(None),
    }
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

fn binary_state(engine: ExecutionEngine) -> Result<BinaryState, Box<dyn std::error::Error>> {
    let universe = load_trade_universe()?;
    info!(
        "Trade universe: {} symbols (hash 0x{:016x})",
        universe.n_assets(),
        universe.hash
    );
    let max_age_ns = pulse_max_age_ns()?;
    Ok(BinaryState::new(engine, universe, now_ns(), max_age_ns))
}

struct Endpoints {
    readout: String,
    market: String,
}

fn spawn_curve_listener(
    context: &zmq::Context,
    eps: &Endpoints,
    stop: Arc<AtomicBool>,
    mut state: BinaryState,
    zap_active: bool,
) -> Result<tokio::task::JoinHandle<()>, Box<dyn std::error::Error>> {
    let timeout_ms = RECV_TIMEOUT.as_millis() as i32;
    let client_sec = zmq_curve::client_secret_key_z85()?;
    let readout_pub = zmq_curve::readout_server_public_z85()?;
    let market_pub = zmq_curve::market_server_public_z85()?;
    let sub_readout = curve_sub(context, &readout_pub, &client_sec, &eps.readout, timeout_ms)?;
    let sub_market = curve_sub(context, &market_pub, &client_sec, &eps.market, timeout_ms)?;
    info!(
        "Binary CURVE SUBs: readout {}, market {} (ZAP allowlist active: {zap_active})",
        eps.readout, eps.market
    );
    Ok(tokio::task::spawn_blocking(move || {
        let mut market_next = true;
        run_listener(&stop, &mut state, || {
            poll_pair(&sub_readout, &sub_market, timeout_ms, &mut market_next)
        });
    }))
}

fn plain_sub(context: &zmq::Context, eps: &Endpoints) -> Result<zmq::Socket, zmq::Error> {
    let subscriber = context.socket(zmq::SUB)?;
    subscriber.connect(&eps.readout)?;
    subscriber.connect(&eps.market)?;
    subscriber.set_subscribe(b"")?;
    subscriber.set_rcvtimeo(RECV_TIMEOUT.as_millis() as i32)?;
    Ok(subscriber)
}

fn spawn_plain_listener(
    context: &zmq::Context,
    eps: &Endpoints,
    stop: Arc<AtomicBool>,
    mut state: BinaryState,
) -> Result<tokio::task::JoinHandle<()>, Box<dyn std::error::Error>> {
    let subscriber = plain_sub(context, eps)?;
    info!(
        "Binary SUB connected to readout {} and market {} (pulse max age {} ms)",
        eps.readout,
        eps.market,
        state.max_age_ns / 1_000_000
    );
    Ok(tokio::task::spawn_blocking(move || {
        run_listener(&stop, &mut state, || recv_frame(&subscriber));
    }))
}

async fn start_binary_listener(engine: ExecutionEngine) -> Result<(), Box<dyn std::error::Error>> {
    let eps = Endpoints {
        readout: readout_endpoint()?,
        market: market_endpoint()?,
    };
    let state = binary_state(engine)?;
    let stop = Arc::new(AtomicBool::new(false));
    let context = zmq::Context::new();
    let zap = zmq_curve::maybe_start_zap(&context).map_err(|e| format!("ZAP setup: {e}"))?;
    let listener = if zmq_curve::curve_enabled() {
        spawn_curve_listener(&context, &eps, stop.clone(), state, zap.is_some())?
    } else {
        spawn_plain_listener(&context, &eps, stop.clone(), state)?
    };
    // `zap` must outlive the listener: dropping it stops the ZAP handler.
    let result = supervise("binary", stop, listener).await;
    drop(zap);
    result
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

    #[test]
    fn busy_stream_cannot_starve_the_other() {
        let mut market_next = true;
        let picks: Vec<_> = (0..4)
            .map(|_| pick_market(true, true, &mut market_next))
            .collect();
        assert_eq!(picks, [Some(true), Some(false), Some(true), Some(false)]);
        assert_eq!(pick_market(true, false, &mut market_next), Some(false));
        assert_eq!(pick_market(false, true, &mut market_next), Some(true));
        assert_eq!(pick_market(false, false, &mut market_next), None);
    }

    #[tokio::test]
    async fn exited_listener_is_reported_without_repolling_completed_handle() {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = tokio::task::spawn_blocking(|| ());
        assert!(supervise("test", stop.clone(), handle).await.is_err());
        assert!(stop.load(Ordering::Relaxed));
    }
}
