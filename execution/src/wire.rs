//! NervousWire v2 — binary pack/decode + readout → TradeSignal.
//!
//! Layouts: `docs/wire-protocol-v2.md` and golden fixtures under `wire/fixtures/`.

use crate::universe::TradeUniverse;
use crate::{TradeSide, TradeSignal};

pub use crate::binary_wire::{
    classify_wire_frame, readout_to_trade, MappedTrade, MarketPulse, ReadoutPacket, WireSide,
    MARKET_PULSE_BYTES, MARKET_PULSE_V1_BYTES, READOUT_PACKET_BYTES, READOUT_PACKET_V1_BYTES,
    WIRE_MAGIC,
};

pub const IPC_MARKET_DEFAULT: &str = "tcp://127.0.0.1:5555";
pub const IPC_READOUT_DEFAULT: &str = "tcp://127.0.0.1:5556";

/// `LIMEN_IPC_SUB` (or the default), validated by [`validate_binary_endpoint`].
pub fn market_endpoint() -> Result<String, String> {
    validate_binary_endpoint(
        &std::env::var("LIMEN_IPC_SUB").unwrap_or_else(|_| IPC_MARKET_DEFAULT.to_string()),
        remote_allowed(),
    )
}

/// `LIMEN_IPC_PUB` (or the default), validated by [`validate_binary_endpoint`].
pub fn readout_endpoint() -> Result<String, String> {
    validate_binary_endpoint(
        &std::env::var("LIMEN_IPC_PUB").unwrap_or_else(|_| IPC_READOUT_DEFAULT.to_string()),
        remote_allowed(),
    )
}

fn remote_allowed() -> bool {
    std::env::var("LIMEN_ALLOW_REMOTE_WIRE").as_deref() == Ok("1")
}

/// Accept `ipc://` absolute paths and `tcp://` literal loopback addresses.
///
/// Hostnames are not resolved: DNS can change between validation and
/// connect/bind, so `localhost` counts as remote.
pub fn validate_binary_endpoint(ep: &str, allow_remote: bool) -> Result<String, String> {
    if ep.starts_with("ipc://") {
        return validate_json_ipc_endpoint(ep);
    }
    let addr = ep
        .strip_prefix("tcp://")
        .ok_or("binary endpoint must use tcp:// or ipc://")?;
    let (host, port) = addr
        .rsplit_once(':')
        .ok_or("TCP endpoint requires a port")?;
    if port.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
        return Err("TCP endpoint requires a port in 1..65535".into());
    }
    if !allow_remote && !is_loopback_literal(host) {
        return Err("non-loopback wire endpoint requires LIMEN_ALLOW_REMOTE_WIRE=1 (use CURVE on untrusted networks)".into());
    }
    Ok(ep.to_string())
}

/// ZeroMQ needs IPv6 hosts in brackets (`[::1]`); a bare `::1` is refused.
fn is_loopback_literal(host: &str) -> bool {
    match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(v6) => v6
            .parse::<std::net::Ipv6Addr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback()),
    }
}

/// Per-stream timestamp watermark, seeded at process start so frames from
/// before a restart are refused.
#[derive(Debug, Clone, Copy)]
pub struct TickGuard {
    last: u64,
}

impl TickGuard {
    pub fn new(start_ns: u64) -> Self {
        Self { last: start_ns }
    }

    /// Accept only strictly increasing ticks that are neither in the future
    /// nor older than `max_age_ns`. A refused tick leaves the watermark alone.
    pub fn accept(&mut self, tick: u64, now_ns: u64, max_age_ns: u64) -> bool {
        if tick <= self.last || tick > now_ns || now_ns - tick > max_age_ns {
            return false;
        }
        self.last = tick;
        true
    }
}

pub const PULSE_MAX_AGE_MS_DEFAULT: u64 = 2_000;

pub fn parse_pulse_max_age_ms(raw: Option<&str>) -> Result<u64, String> {
    let ms = match raw {
        None => PULSE_MAX_AGE_MS_DEFAULT,
        Some(v) => v.trim().parse::<u64>().map_err(|e| {
            format!(
                "LIMEN_PULSE_MAX_AGE_MS={v:?} is not a non-negative integer of milliseconds: {e}"
            )
        })?,
    };
    Ok(ms.saturating_mul(1_000_000))
}

pub fn pulse_max_age_ns() -> Result<u64, String> {
    parse_pulse_max_age_ms(std::env::var("LIMEN_PULSE_MAX_AGE_MS").ok().as_deref())
}

pub fn load_trade_universe() -> Result<TradeUniverse, String> {
    TradeUniverse::load_default()
}

pub fn decode_market_pulse(buf: &[u8], universe: &TradeUniverse) -> Result<MarketPulse, String> {
    MarketPulse::decode(buf, Some(universe))
}

pub fn decode_readout(buf: &[u8], universe: &TradeUniverse) -> Result<ReadoutPacket, String> {
    ReadoutPacket::decode(buf, Some(universe))
}

pub fn pack_market_pulse(pulse: &MarketPulse) -> Result<Vec<u8>, String> {
    pulse.pack_v2()
}

pub fn pack_readout(packet: &ReadoutPacket) -> Result<Vec<u8>, String> {
    packet.pack_v2()
}

/// `timestamp_ns: None` uses `packet.tick`, the source MarketPulse timestamp,
/// so signal latency includes wire and brain time.
pub fn readout_to_trade_signal(
    packet: &ReadoutPacket,
    universe: &TradeUniverse,
    price: f64,
    quantity: f64,
    timestamp_ns: Option<i64>,
) -> TradeSignal {
    let m = readout_to_trade(packet, universe);
    let side = match m.side {
        WireSide::Buy => TradeSide::Buy,
        WireSide::Sell => TradeSide::Sell,
        WireSide::Neutral => TradeSide::Neutral,
    };
    let ts = timestamp_ns.unwrap_or(packet.tick);
    TradeSignal {
        ticker: m.ticker,
        side,
        price,
        quantity,
        confidence: m.confidence,
        timestamp_ns: ts,
    }
}

pub fn price_for_ticker(
    pulse: &MarketPulse,
    universe: &TradeUniverse,
    ticker: &str,
) -> Option<f64> {
    if pulse.symbol_table_hash != universe.hash {
        return None;
    }
    universe
        .symbols
        .iter()
        .position(|t| t == ticker)
        .and_then(|i| pulse.prices.get(i).copied())
        .map(f64::from)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceError {
    NoPulse,
    Stale { age_ns: u64, max_age_ns: u64 },
    UnknownTicker,
}

impl std::fmt::Display for PriceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PriceError::NoPulse => write!(f, "no MarketPulse received yet"),
            PriceError::Stale { age_ns, max_age_ns } => write!(
                f,
                "last MarketPulse is {:.1} ms old (max {:.1} ms)",
                *age_ns as f64 / 1e6,
                *max_age_ns as f64 / 1e6
            ),
            PriceError::UnknownTicker => write!(f, "pair is not a tradeable asset"),
        }
    }
}

pub fn resolve_fill_price(
    last_pulse: Option<&MarketPulse>,
    universe: &TradeUniverse,
    ticker: &str,
    now_ns: u64,
    max_age_ns: u64,
) -> Result<f64, PriceError> {
    let pulse = last_pulse
        .filter(|p| p.timestamp_ns != 0)
        .ok_or(PriceError::NoPulse)?;
    let age_ns = now_ns.saturating_sub(pulse.timestamp_ns);
    if age_ns > max_age_ns {
        return Err(PriceError::Stale { age_ns, max_age_ns });
    }
    price_for_ticker(pulse, universe, ticker).ok_or(PriceError::UnknownTicker)
}

// ── JSON IPC helpers (unchanged) ─────────────────────────────────────────────

pub fn validate_json_ipc_endpoint(ep: &str) -> Result<String, String> {
    let ep = ep.trim();
    if ep.is_empty() {
        return Err("LIMEN_JSON_IPC is empty".into());
    }
    let Some(path) = ep.strip_prefix("ipc://") else {
        return Err("LIMEN_JSON_IPC must start with ipc://".into());
    };
    if !path.starts_with('/') {
        return Err("LIMEN_JSON_IPC path must be absolute (ipc:///path/...)".into());
    }
    if path.split('/').any(|seg| seg == "..") {
        return Err("LIMEN_JSON_IPC must not contain '..' path segments".into());
    }
    Ok(ep.to_string())
}

pub fn resolve_json_ipc_endpoint(
    override_ep: Option<&str>,
    xdg_runtime_dir: Option<&str>,
) -> Result<String, String> {
    if let Some(v) = override_ep {
        if !v.is_empty() {
            return validate_json_ipc_endpoint(v);
        }
    }
    let dir = if let Some(runtime) = xdg_runtime_dir {
        if !runtime.is_empty() {
            std::path::PathBuf::from(runtime).join("limen-capital")
        } else {
            default_tmp_json_ipc_dir()
        }
    } else {
        default_tmp_json_ipc_dir()
    };
    prepare_json_ipc_dir(&dir)?;
    Ok(format!("ipc://{}/signals.ipc", dir.display()))
}

pub fn json_ipc_endpoint() -> Result<String, String> {
    let override_ep = std::env::var("LIMEN_JSON_IPC").ok();
    let xdg = std::env::var("XDG_RUNTIME_DIR").ok();
    resolve_json_ipc_endpoint(
        override_ep.as_deref().filter(|s| !s.is_empty()),
        xdg.as_deref().filter(|s| !s.is_empty()),
    )
}

fn default_tmp_json_ipc_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(format!("/tmp/limen-capital-{}", current_uid()))
}

fn prepare_json_ipc_dir(dir: &std::path::Path) -> Result<(), String> {
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() {
            return Err(format!(
                "JSON IPC path {} is a symlink — refusing to use attacker-controlled path",
                dir.display()
            ));
        }
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("failed to create JSON IPC directory {}: {e}", dir.display()))?;
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("failed to stat JSON IPC directory {}: {e}", dir.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "JSON IPC path {} resolved as a symlink after create",
            dir.display()
        ));
    }
    if !meta.is_dir() {
        return Err(format!(
            "JSON IPC path {} is not a directory",
            dir.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let uid = current_uid();
        if meta.uid() != uid {
            return Err(format!(
                "JSON IPC directory {} owned by uid {}, expected {}",
                dir.display(),
                meta.uid(),
                uid
            ));
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            format!(
                "failed to chmod 0700 JSON IPC directory {}: {e}",
                dir.display()
            )
        })?;
    }
    Ok(())
}

fn current_uid() -> u32 {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(uid) = rest.split_whitespace().next().and_then(|s| s.parse().ok()) {
                    return uid;
                }
            }
        }
    }
    #[cfg(unix)]
    {
        extern "C" {
            fn getuid() -> u32;
        }
        // SAFETY: getuid is always safe on POSIX.
        unsafe { getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn universe_7() -> TradeUniverse {
        TradeUniverse::load(&TradeUniverse::default_config_path()).expect("default universe")
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../wire/fixtures")
            .join(name)
    }

    #[test]
    fn golden_readout_v1_to_signal() {
        let u = universe_7();
        let bytes = std::fs::read(fixture("readout.bin")).expect("fixture");
        let packet = decode_readout(&bytes, &u).unwrap();
        let sig = readout_to_trade_signal(&packet, &u, 1.0, 1.0, Some(123));
        assert_eq!(sig.side, TradeSide::Buy);
        assert_eq!(sig.ticker, u.symbols[0]);
        assert!(sig.confidence > 0.0);
        assert_eq!(sig.timestamp_ns, 123);
    }

    #[test]
    fn golden_marketpulse_v1() {
        let u = universe_7();
        let bytes = std::fs::read(fixture("marketpulse.bin")).expect("fixture");
        let p = decode_market_pulse(&bytes, &u).unwrap();
        assert_eq!(p.timestamp_ns, 1_700_000_000_000_000_001);
        assert!((p.prices[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn v2_roundtrip_market_and_readout() {
        let u7 = universe_7();
        let u3 = TradeUniverse::load(&fixture("universe_3.toml")).expect("universe_3");
        let u12 = TradeUniverse::load(&fixture("universe_12.toml")).expect("universe_12");

        for u in [&u7, &u3, &u12] {
            let mut pulse = MarketPulse::new_for_universe(u);
            pulse.timestamp_ns = 42;
            for (i, px) in pulse.prices.iter_mut().enumerate() {
                *px = 1.0 + i as f32 * 0.1;
            }
            let enc = pack_market_pulse(&pulse).unwrap();
            let dec = decode_market_pulse(&enc, u).unwrap();
            assert_eq!(dec.prices, pulse.prices);

            let mut packet = ReadoutPacket {
                tick: 7,
                symbol_table_hash: u.hash,
                readout: vec![0.0; u.n_out()],
                relevance: [0.4, 0.3, 0.2, 0.1],
            };
            packet.readout[0] = 0.9;
            packet.readout[1] = 0.1;
            let rb = pack_readout(&packet).unwrap();
            let dr = decode_readout(&rb, u).unwrap();
            assert_eq!(dr.readout, packet.readout);
        }
    }

    #[test]
    fn hash_mismatch_rejected() {
        let u = universe_7();
        let u3 = TradeUniverse::load(&fixture("universe_3.toml")).unwrap();
        let mut pulse = MarketPulse::new_for_universe(&u3);
        pulse.timestamp_ns = 1;
        let bytes = pulse.pack_v2().unwrap();
        assert!(decode_market_pulse(&bytes, &u).is_err());
    }

    fn pulse_at(ts: u64, u: &TradeUniverse) -> MarketPulse {
        let mut p = MarketPulse::new_for_universe(u);
        p.timestamp_ns = ts;
        for (i, px) in p.prices.iter_mut().enumerate() {
            *px = 10.0 + i as f32;
        }
        p
    }

    #[test]
    fn price_for_ticker_maps_assets_and_rejects_residual() {
        let u = universe_7();
        let p = pulse_at(1, &u);
        assert_eq!(price_for_ticker(&p, &u, &u.symbols[0]), Some(10.0));
        assert_eq!(
            price_for_ticker(&p, &u, u.symbols.last().unwrap()),
            Some(10.0 + (u.n_assets() - 1) as f64)
        );
        assert_eq!(price_for_ticker(&p, &u, "RESIDUAL"), None);
        assert_eq!(price_for_ticker(&p, &u, "nope"), None);
    }

    #[test]
    fn pulse_max_age_parses_or_fails_loudly() {
        assert_eq!(
            parse_pulse_max_age_ms(None),
            Ok(PULSE_MAX_AGE_MS_DEFAULT * 1_000_000)
        );
        assert_eq!(parse_pulse_max_age_ms(Some("250")), Ok(250_000_000));
        assert!(parse_pulse_max_age_ms(Some("2s")).is_err());
    }

    #[test]
    fn resolve_fill_price_refuses_without_pulse() {
        let u = universe_7();
        assert_eq!(
            resolve_fill_price(None, &u, &u.symbols[0], 1_000, 1_000),
            Err(PriceError::NoPulse)
        );
        let never = MarketPulse::new_for_universe(&u);
        assert_eq!(
            resolve_fill_price(Some(&never), &u, &u.symbols[0], 1_000, 1_000),
            Err(PriceError::NoPulse)
        );
    }

    #[test]
    fn resolve_fill_price_uses_pulse_price_when_fresh() {
        let u = universe_7();
        let p = pulse_at(5_000, &u);
        let sym = u.symbols[1].clone();
        assert_eq!(
            resolve_fill_price(Some(&p), &u, &sym, 5_500, 1_000),
            Ok(11.0)
        );
    }

    #[test]
    fn resolve_fill_price_refuses_residual() {
        let u = universe_7();
        let p = pulse_at(5_000, &u);
        assert_eq!(
            resolve_fill_price(Some(&p), &u, "RESIDUAL", 5_000, 1_000),
            Err(PriceError::UnknownTicker)
        );
    }

    #[test]
    fn golden_readout_prices_from_golden_pulse() {
        let u = universe_7();
        let r = std::fs::read(fixture("readout.bin")).expect("fixture");
        let m = std::fs::read(fixture("marketpulse.bin")).expect("fixture");
        let packet = decode_readout(&r, &u).unwrap();
        let pulse = decode_market_pulse(&m, &u).unwrap();
        let sig = readout_to_trade_signal(&packet, &u, 0.0, 1.0, None);
        let px = resolve_fill_price(Some(&pulse), &u, &sig.ticker, pulse.timestamp_ns, 1).unwrap();
        assert!((px - pulse.prices[0] as f64).abs() < 1e-6);
    }

    #[test]
    fn reject_non_finite_price_v1_fixture() {
        let u = universe_7();
        let mut bytes = std::fs::read(fixture("marketpulse.bin")).expect("fixture");
        bytes[8..12].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(decode_market_pulse(&bytes, &u).is_err());
    }

    #[test]
    fn readout_overflow_scores_fail_closed() {
        let u = universe_7();
        let mut packet = ReadoutPacket {
            tick: 1,
            symbol_table_hash: u.hash,
            readout: vec![0.0; u.n_out()],
            relevance: [0.4, 0.3, 0.2, 0.1],
        };
        packet.readout[0] = f32::MAX;
        packet.readout[1] = -f32::MAX;
        let m = readout_to_trade(&packet, &u);
        assert_eq!(m.side, WireSide::Neutral);
        assert_eq!(m.confidence, 0.0);
        assert_eq!(m.pair_index, 0);
    }

    #[test]
    fn reserved_header_and_prefix_bytes_must_be_zero() {
        let u = universe_7();
        let pulse = pulse_at(1, &u);
        let good = pack_market_pulse(&pulse).unwrap();
        assert!(decode_market_pulse(&good, &u).is_ok());
        // Header bytes 6..8 and the u16 after n_assets (offset 26..28).
        for offset in [6, 7, 26, 27] {
            let mut bad = good.clone();
            bad[offset] = 1;
            assert!(decode_market_pulse(&bad, &u).is_err(), "market {offset}");
        }
        let packet = ReadoutPacket {
            tick: 1,
            symbol_table_hash: u.hash,
            readout: vec![0.0; u.n_out()],
            relevance: [0.25; 4],
        };
        let good = pack_readout(&packet).unwrap();
        assert!(decode_readout(&good, &u).is_ok());
        for offset in [6, 7, 26, 27] {
            let mut bad = good.clone();
            bad[offset] = 1;
            assert!(decode_readout(&bad, &u).is_err(), "readout {offset}");
        }
    }

    #[test]
    fn binary_endpoints_are_local_unless_explicitly_allowed() {
        for ep in [
            "tcp://127.0.0.1:5555",
            "tcp://127.42.0.1:5556",
            "tcp://[::1]:5555",
            "ipc:///tmp/wire.sock",
        ] {
            assert_eq!(validate_binary_endpoint(ep, false).unwrap(), ep);
        }
        for ep in [
            "tcp://0.0.0.0:5555",
            "tcp://*:5555",
            "tcp://[::]:5555",
            "tcp://192.0.2.1:5555",
            "tcp://localhost:5555",
            "tcp://example.org:5555",
            "tcp://::1:5555",
            "tcp://[127.0.0.1]:5555",
        ] {
            assert!(validate_binary_endpoint(ep, false).is_err(), "{ep}");
            assert_eq!(validate_binary_endpoint(ep, true).unwrap(), ep);
        }
        for ep in [
            "",
            "udp://127.0.0.1:5555",
            "ipc://relative",
            "ipc:///tmp/../wire",
            "tcp://127.0.0.1:0",
            "tcp://127.0.0.1:65536",
            "tcp://127.0.0.1",
        ] {
            assert!(validate_binary_endpoint(ep, false).is_err(), "{ep}");
            assert!(validate_binary_endpoint(ep, true).is_err(), "{ep}");
        }
    }

    #[test]
    fn tick_guard_rejects_replay_staleness_and_future_without_poisoning() {
        let mut ticks = TickGuard::new(100);
        for tick in [0, 99, 100, 201] {
            assert!(!ticks.accept(tick, 200, 50));
        }
        assert!(!ticks.accept(149, 200, 50));
        assert!(ticks.accept(150, 200, 50));
        assert!(!ticks.accept(150, 200, 50));
        assert!(ticks.accept(151, 200, 50));
        assert!(!ticks.accept(150, 200, 50));
        let mut restarted = TickGuard::new(200);
        assert!(!restarted.accept(151, 201, 50));
        assert!(restarted.accept(201, 201, 50));
    }

    #[test]
    fn default_signal_timestamp_is_source_tick_not_receipt_time() {
        let u = universe_7();
        let packet = ReadoutPacket {
            tick: 123,
            symbol_table_hash: u.hash,
            readout: vec![0.0; u.n_out()],
            relevance: [0.25; 4],
        };
        let sig = readout_to_trade_signal(&packet, &u, 1.0, 1.0, None);
        assert_eq!(sig.timestamp_ns, 123);
    }

    #[test]
    fn json_ipc_rejects_traversal() {
        assert!(validate_json_ipc_endpoint("ipc:///tmp/../etc/passwd").is_err());
    }
}
