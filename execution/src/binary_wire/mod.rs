//! Binary wire packets for SNN-HFT research (NervousWire v2 + legacy v1 decode).
//!
//! v2 frames: magic/version header + variable asset count (runtime universe hash).
//! v1 fixed 120/88 B layouts remain decodable for golden regression tests.

mod cursor_io;
mod market;
mod readout;

pub use market::MarketPulse;
pub use readout::{readout_to_trade, MappedTrade, ReadoutPacket, WireSide};

use cursor_io::peek_magic;

/// Little-endian `b"LNRV"` (`0x56524E4C`).
pub const WIRE_MAGIC: u32 = 0x5652_4E4C;
pub const WIRE_VERSION_V2: u8 = 2;
pub const FRAME_MARKET_PULSE: u8 = 1;
pub const FRAME_READOUT: u8 = 2;

pub const WIRE_HEADER_BYTES: usize = 8;

pub const MARKET_PULSE_V1_BYTES: usize = 120;
pub const READOUT_PACKET_V1_BYTES: usize = 88;
pub const N_RELEVANCE: usize = 4;
pub const SIDE_EPS: f32 = 1e-4;

/// Deprecated aliases for tests still naming v1 constants.
pub const MARKET_PULSE_BYTES: usize = MARKET_PULSE_V1_BYTES;
pub const READOUT_PACKET_BYTES: usize = READOUT_PACKET_V1_BYTES;

pub(crate) const TRAILING_SIGNALS: usize = 11;

/// Classify an inbound ZMQ payload without length-only guessing.
pub fn classify_wire_frame(buf: &[u8]) -> Result<&'static str, String> {
    if looks_like_v2(buf) {
        return v2_kind(buf[5]);
    }
    legacy_kind(buf.len())
}

fn looks_like_v2(buf: &[u8]) -> bool {
    buf.len() >= WIRE_HEADER_BYTES && peek_magic(buf) == WIRE_MAGIC
}

fn v2_kind(frame_type: u8) -> Result<&'static str, String> {
    match frame_type {
        FRAME_MARKET_PULSE => Ok("market_v2"),
        FRAME_READOUT => Ok("readout_v2"),
        other => Err(format!("unknown v2 frame type {other}")),
    }
}

fn legacy_kind(len: usize) -> Result<&'static str, String> {
    if len == MARKET_PULSE_V1_BYTES {
        return Ok("market_v1");
    }
    if len == READOUT_PACKET_V1_BYTES {
        return Ok("readout_v1");
    }
    Err(format!("unrecognized wire frame ({len} bytes)"))
}
