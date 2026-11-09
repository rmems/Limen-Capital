//! NervousWire v2 — binary pack/decode + readout → TradeSignal.
//!
//! Layouts: `docs/wire-protocol-v2.md` and golden fixtures under `wire/fixtures/`.

use crate::universe::TradeUniverse;
use crate::{TradeSide, TradeSignal};
use std::time::{SystemTime, UNIX_EPOCH};

pub use crate::binary_wire::{
    classify_wire_frame, readout_to_trade, MappedTrade, MarketPulse, ReadoutPacket, WireSide,
    MARKET_PULSE_BYTES, MARKET_PULSE_V1_BYTES, READOUT_PACKET_BYTES, READOUT_PACKET_V1_BYTES,
    WIRE_MAGIC,
};

pub const IPC_MARKET_DEFAULT: &str = "tcp://127.0.0.1:5555";
pub const IPC_READOUT_DEFAULT: &str = "tcp://127.0.0.1:5556";

pub fn market_endpoint() -> String {
    std::env::var("LIMEN_IPC_SUB").unwrap_or_else(|_| IPC_MARKET_DEFAULT.to_string())
}

pub fn readout_endpoint() -> String {
    std::env::var("LIMEN_IPC_PUB").unwrap_or_else(|_| IPC_READOUT_DEFAULT.to_string())
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
    let ts = timestamp_ns.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64
    });
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
    universe
        .symbols
        .iter()
        .position(|t| t == ticker)
        .map(|i| pulse.prices[i] as f64)
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
        TradeUniverse::load_default().expect("default universe")
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
    fn json_ipc_rejects_traversal() {
        assert!(validate_json_ipc_endpoint("ipc:///tmp/../etc/passwd").is_err());
    }
}
