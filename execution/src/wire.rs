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

/// Default maximum age of the last MarketPulse before fills are refused (2 s).
pub const PULSE_MAX_AGE_MS_DEFAULT: u64 = 2_000;

/// `LIMEN_PULSE_MAX_AGE_MS` as nanoseconds (default [`PULSE_MAX_AGE_MS_DEFAULT`]).
pub fn pulse_max_age_ns() -> u64 {
    std::env::var("LIMEN_PULSE_MAX_AGE_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(PULSE_MAX_AGE_MS_DEFAULT)
        .saturating_mul(1_000_000)
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

/// Price for `ticker` from a MarketPulse. `None` when the ticker is not one of
/// [`ASSET_TICKERS`] (e.g. the RESIDUAL pair, which is not tradeable).
pub fn price_for_ticker(pulse: &MarketPulse, ticker: &str) -> Option<f64> {
    ASSET_TICKERS
        .iter()
        .position(|t| *t == ticker)
        .map(|i| pulse.prices[i] as f64)
}

/// Why a readout could not be priced for execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceError {
    /// No MarketPulse has been received yet.
    NoPulse,
    /// The last MarketPulse is older than the configured maximum age.
    Stale { age_ns: u64, max_age_ns: u64 },
    /// The primary pair is not a tradeable asset (RESIDUAL).
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

/// Resolve the fill price for a readout from the last MarketPulse.
///
/// Refuses (instead of falling back to a placeholder price) when no pulse has
/// arrived, when the pulse is older than `max_age_ns` relative to `now_ns`, or
/// when `ticker` is not a tradeable asset. A pulse with `timestamp_ns == 0`
/// is treated as never received.
pub fn resolve_fill_price(
    last_pulse: Option<&MarketPulse>,
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
    price_for_ticker(pulse, ticker).ok_or(PriceError::UnknownTicker)
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

    fn pulse_at(ts: u64) -> MarketPulse {
        let mut p = MarketPulse {
            timestamp_ns: ts,
            ..MarketPulse::default()
        };
        for (i, px) in p.prices.iter_mut().enumerate() {
            *px = 10.0 + i as f32;
        }
        p
    }

    #[test]
    fn price_for_ticker_maps_assets_and_rejects_residual() {
        let p = pulse_at(1);
        assert_eq!(price_for_ticker(&p, "DNX"), Some(10.0));
        assert_eq!(price_for_ticker(&p, "Verus"), Some(16.0));
        assert_eq!(price_for_ticker(&p, "RESIDUAL"), None);
        assert_eq!(price_for_ticker(&p, "nope"), None);
    }

    #[test]
    fn resolve_fill_price_refuses_without_pulse() {
        assert_eq!(
            resolve_fill_price(None, "DNX", 1_000, 1_000),
            Err(PriceError::NoPulse)
        );
        // A default pulse (timestamp 0) is "never received", not a $1.00 market.
        let never = MarketPulse::default();
        assert_eq!(
            resolve_fill_price(Some(&never), "DNX", 1_000, 1_000),
            Err(PriceError::NoPulse)
        );
    }

    #[test]
    fn resolve_fill_price_uses_pulse_price_when_fresh() {
        let p = pulse_at(5_000);
        assert_eq!(resolve_fill_price(Some(&p), "Quai", 5_500, 1_000), Ok(11.0));
        // Exactly at the limit is still fresh.
        assert_eq!(resolve_fill_price(Some(&p), "Quai", 6_000, 1_000), Ok(11.0));
        // Clock skew (pulse from the future) is not stale.
        assert_eq!(resolve_fill_price(Some(&p), "Quai", 4_000, 1_000), Ok(11.0));
    }

    #[test]
    fn resolve_fill_price_refuses_stale_pulse() {
        let p = pulse_at(5_000);
        assert_eq!(
            resolve_fill_price(Some(&p), "Quai", 6_001, 1_000),
            Err(PriceError::Stale {
                age_ns: 1_001,
                max_age_ns: 1_000
            })
        );
    }

    #[test]
    fn resolve_fill_price_refuses_residual() {
        let p = pulse_at(5_000);
        assert_eq!(
            resolve_fill_price(Some(&p), "RESIDUAL", 5_000, 1_000),
            Err(PriceError::UnknownTicker)
        );
    }

    #[test]
    fn golden_readout_prices_from_golden_pulse() {
        let r = std::fs::read(fixture("readout.bin")).expect("fixture");
        let m = std::fs::read(fixture("marketpulse.bin")).expect("fixture");
        let packet = decode_readout(&r).unwrap();
        let pulse = decode_market_pulse(&m).unwrap();
        let sig = readout_to_trade_signal(&packet, 0.0, 1.0, None);
        let px = resolve_fill_price(Some(&pulse), &sig.ticker, pulse.timestamp_ns, 1).unwrap();
        assert!((px - pulse.prices[0] as f64).abs() < 1e-6);
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
        // Valid simplex relevance so only score overflow trips fail-closed mapping.
        let mut packet = ReadoutPacket {
            tick: 1,
            readout: [0.0; 16],
            relevance: [0.4, 0.3, 0.2, 0.1],
        };
        // bull=MAX, bear=-MAX → score overflows to non-finite
        packet.readout[0] = f32::MAX;
        packet.readout[1] = -f32::MAX;
        let m = readout_to_trade(&packet);
        assert_eq!(m.side, WireSide::Neutral);
        assert_eq!(m.confidence, 0.0);
        assert_eq!(m.pair_index, 0);
    }

    #[test]
    fn readout_rejects_relevance_out_of_range() {
        let mut bytes = std::fs::read(fixture("readout.bin")).expect("fixture");
        // first relevance float at offset 8 + 16*4 = 72
        bytes[72..76].copy_from_slice(&2.0f32.to_le_bytes());
        let err = decode_readout(&bytes).unwrap_err();
        assert!(
            err.contains("must be in [0, 1]"),
            "expected range error, got: {err}"
        );

        let mut bytes_neg = std::fs::read(fixture("readout.bin")).expect("fixture");
        bytes_neg[72..76].copy_from_slice(&(-0.1f32).to_le_bytes());
        let err_neg = decode_readout(&bytes_neg).unwrap_err();
        assert!(
            err_neg.contains("must be in [0, 1]"),
            "expected range error, got: {err_neg}"
        );
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
