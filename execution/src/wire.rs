//! NervousWire v1 — binary pack/decode + readout → TradeSignal.
//!
//! Layouts and golden fixtures: `wire/` and `docs/wire-protocol-v1.md`.
//! Wire types are owned in-tree (`binary_wire`) until published corpus-ipc
//! exposes MarketPulse/ReadoutPacket on a public rev.

use crate::{TradeSide, TradeSignal};
use std::time::{SystemTime, UNIX_EPOCH};

pub use crate::binary_wire::{
    readout_to_trade, MappedTrade, MarketPulse, ReadoutPacket, WireSide, ASSET_TICKERS,
    MARKET_PULSE_BYTES, READOUT_PACKET_BYTES,
};

pub const IPC_MARKET_DEFAULT: &str = "tcp://127.0.0.1:5555";
pub const IPC_READOUT_DEFAULT: &str = "tcp://127.0.0.1:5556";

/// Env-overridable endpoints (match Julia `LIMEN_IPC_*`).
pub fn market_endpoint() -> String {
    std::env::var("LIMEN_IPC_SUB").unwrap_or_else(|_| IPC_MARKET_DEFAULT.to_string())
}

pub fn readout_endpoint() -> String {
    std::env::var("LIMEN_IPC_PUB").unwrap_or_else(|_| IPC_READOUT_DEFAULT.to_string())
}

/// Decode MarketPulse with validation (finite fields, prices > 0, vol clamp).
/// Validation lives in [`MarketPulse::decode`] (defense-in-depth).
pub fn decode_market_pulse(buf: &[u8]) -> Result<MarketPulse, String> {
    MarketPulse::decode(buf)
}

/// Validate a `LIMEN_JSON_IPC` override: must be `ipc://` + absolute path, no `..`.
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

/// Resolve JSON IPC endpoint from an optional override (pure of process env).
/// When `override_ep` is `Some` and non-empty, validates and returns it.
/// Otherwise builds the default UID-scoped path under XDG or `/tmp`.
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

/// Default JSON IPC endpoint (OS-UID scoped; override with `LIMEN_JSON_IPC`).
/// Prefer `$XDG_RUNTIME_DIR/limen-capital/signals.ipc`, else `/tmp/limen-capital-$UID/`.
/// Returns an error if the endpoint is invalid or the directory cannot be created.
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

/// Create (if needed) and verify a non-symlink, owner-only IPC directory.
fn prepare_json_ipc_dir(dir: &std::path::Path) -> Result<(), String> {
    // Refuse to create through an existing symlink at the leaf path.
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

/// Numeric OS UID for multi-user /tmp isolation.
/// Prefer `/proc/self/status`; fall back to libc `getuid` (not UID 0).
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
        // libc is transitive; declare the symbol for a correct fallback.
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

pub fn pack_market_pulse(pulse: &MarketPulse) -> [u8; MARKET_PULSE_BYTES] {
    pulse.pack()
}

pub fn decode_readout(buf: &[u8]) -> Result<ReadoutPacket, String> {
    ReadoutPacket::decode(buf)
}

pub fn pack_readout(packet: &ReadoutPacket) -> [u8; READOUT_PACKET_BYTES] {
    packet.pack()
}

/// Map binary readout → Capital `TradeSignal` (JSON-compatible sides).
pub fn readout_to_trade_signal(
    packet: &ReadoutPacket,
    price: f64,
    quantity: f64,
    timestamp_ns: Option<i64>,
) -> TradeSignal {
    let m = readout_to_trade(packet);
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

/// Price for primary asset from last MarketPulse (fallback 1.0).
pub fn price_for_ticker(pulse: &MarketPulse, ticker: &str) -> f64 {
    if let Some(i) = ASSET_TICKERS.iter().position(|t| *t == ticker) {
        pulse.prices[i] as f64
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../wire/fixtures")
            .join(name)
    }

    #[test]
    fn golden_readout_to_signal() {
        let bytes = std::fs::read(fixture("readout.bin")).expect("fixture");
        let packet = decode_readout(&bytes).unwrap();
        let sig = readout_to_trade_signal(&packet, 1.0, 1.0, Some(123));
        assert_eq!(sig.side, TradeSide::Buy);
        assert_eq!(sig.ticker, "DNX");
        assert!(sig.confidence > 0.0);
        assert_eq!(sig.timestamp_ns, 123);
    }

    #[test]
    fn golden_marketpulse() {
        let bytes = std::fs::read(fixture("marketpulse.bin")).expect("fixture");
        let p = decode_market_pulse(&bytes).unwrap();
        assert_eq!(p.timestamp_ns, 1_700_000_000_000_000_001);
        assert!((p.prices[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn reject_non_finite_price() {
        let mut bytes = std::fs::read(fixture("marketpulse.bin")).expect("fixture");
        bytes[8..12].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(decode_market_pulse(&bytes).is_err());
    }

    #[test]
    fn reject_non_positive_price() {
        let mut bytes = std::fs::read(fixture("marketpulse.bin")).expect("fixture");
        bytes[8..12].copy_from_slice(&0.0f32.to_le_bytes());
        assert!(decode_market_pulse(&bytes).is_err());
    }

    #[test]
    fn reject_non_finite_signal_field() {
        let mut bytes = std::fs::read(fixture("marketpulse.bin")).expect("fixture");
        // confidence_signal at offset 8 + 14*4 = 64
        bytes[64..68].copy_from_slice(&f32::INFINITY.to_le_bytes());
        assert!(decode_market_pulse(&bytes).is_err());
    }

    #[test]
    fn reject_non_finite_readout() {
        let mut bytes = std::fs::read(fixture("readout.bin")).expect("fixture");
        bytes[8..12].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(decode_readout(&bytes).is_err());
    }

    #[test]
    fn json_ipc_respects_override_without_env_mutation() {
        // Pure helper: no process-global env mutation (parallel-test safe).
        assert_eq!(
            resolve_json_ipc_endpoint(Some("ipc:///tmp/custom-limen-test.ipc"), None).unwrap(),
            "ipc:///tmp/custom-limen-test.ipc"
        );
    }

    #[test]
    fn readout_overflow_scores_fail_closed() {
        let mut packet = ReadoutPacket {
            tick: 1,
            readout: [0.0; 16],
            relevance: [1.0; 4],
        };
        // bull=MAX, bear=-MAX → score overflows to non-finite
        packet.readout[0] = f32::MAX;
        packet.readout[1] = -f32::MAX;
        let m = readout_to_trade(&packet);
        assert_eq!(m.side, WireSide::Neutral);
        assert_eq!(m.confidence, 0.0);
    }

    #[test]
    fn readout_rejects_relevance_out_of_range() {
        let mut bytes = std::fs::read(fixture("readout.bin")).expect("fixture");
        // first relevance float at offset 8 + 16*4 = 72
        bytes[72..76].copy_from_slice(&2.0f32.to_le_bytes());
        assert!(decode_readout(&bytes).is_err());

        let mut bytes_neg = std::fs::read(fixture("readout.bin")).expect("fixture");
        bytes_neg[72..76].copy_from_slice(&(-0.1f32).to_le_bytes());
        assert!(decode_readout(&bytes_neg).is_err());
    }

    #[test]
    fn readout_rejects_relevance_not_normalized() {
        let mut bytes = std::fs::read(fixture("readout.bin")).expect("fixture");
        // all four relevance slots = 1.0 → sum 4.0 (malformed simplex)
        for i in 0..4 {
            let off = 72 + i * 4;
            bytes[off..off + 4].copy_from_slice(&1.0f32.to_le_bytes());
        }
        assert!(decode_readout(&bytes).is_err());
    }

    #[test]
    fn json_ipc_rejects_traversal() {
        assert!(validate_json_ipc_endpoint("ipc:///tmp/../etc/passwd").is_err());
        assert!(validate_json_ipc_endpoint("tcp://127.0.0.1:1").is_err());
        assert!(validate_json_ipc_endpoint("ipc://relative").is_err());
    }
}
