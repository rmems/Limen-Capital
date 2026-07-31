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

/// Decode MarketPulse and reject non-finite prices/vols (Capital-side guard).
/// Soft-clamps volumes into [0, 1]. Price ≤ 0 or NaN/Inf → error.
pub fn decode_market_pulse(buf: &[u8]) -> Result<MarketPulse, String> {
    let mut pulse = MarketPulse::decode(buf)?;
    for (i, &p) in pulse.prices.iter().enumerate() {
        if !p.is_finite() {
            return Err(format!("MarketPulse price[{i}] is not finite: {p}"));
        }
        if p <= 0.0 {
            return Err(format!("MarketPulse price[{i}] must be > 0, got {p}"));
        }
    }
    for (i, v) in pulse.vols.iter_mut().enumerate() {
        if !v.is_finite() {
            return Err(format!("MarketPulse vol[{i}] is not finite: {v}"));
        }
        *v = v.clamp(0.0, 1.0);
    }
    Ok(pulse)
}

/// Default JSON IPC endpoint (user-scoped; override with `LIMEN_JSON_IPC`).
/// Prefer `$XDG_RUNTIME_DIR/limen-capital/signals.ipc`, else `/tmp/limen-capital-$USER/`.
pub fn json_ipc_endpoint() -> String {
    if let Ok(v) = std::env::var("LIMEN_JSON_IPC") {
        if !v.is_empty() {
            return v;
        }
    }
    let dir = if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        if !runtime.is_empty() {
            std::path::PathBuf::from(runtime).join("limen-capital")
        } else {
            default_tmp_json_ipc_dir()
        }
    } else {
        default_tmp_json_ipc_dir()
    };
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    format!("ipc://{}/signals.ipc", dir.display())
}

fn default_tmp_json_ipc_dir() -> std::path::PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
    std::path::PathBuf::from(format!("/tmp/limen-capital-{user}"))
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
        // prices[0] starts at offset 8
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
    fn json_ipc_respects_env() {
        std::env::set_var("LIMEN_JSON_IPC", "ipc:///tmp/custom-limen-test.ipc");
        assert_eq!(json_ipc_endpoint(), "ipc:///tmp/custom-limen-test.ipc");
        std::env::remove_var("LIMEN_JSON_IPC");
    }
}
