//! NervousWire v1 — binary pack/decode + readout → TradeSignal.
//!
//! Layouts and golden fixtures: `wire/` and `docs/wire-protocol-v1.md`.
//! Shared types live in Limen-Neural `corpus-ipc`.

use crate::{TradeSide, TradeSignal};
use std::time::{SystemTime, UNIX_EPOCH};

pub use corpus_ipc::{
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

pub fn decode_market_pulse(buf: &[u8]) -> Result<MarketPulse, String> {
    MarketPulse::decode(buf)
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
}
