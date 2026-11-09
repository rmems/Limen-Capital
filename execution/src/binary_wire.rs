//! Binary wire packets for SNN-HFT research (NervousWire v2 + legacy v1 decode).
//!
//! v2 frames: magic/version header + variable asset count (runtime universe hash).
//! v1 fixed 120/88 B layouts remain decodable for golden regression tests.

use crate::universe::TradeUniverse;
use std::io::{Cursor, Read, Write};

// ── Wire v2 frame header ─────────────────────────────────────────────────────

/// Little-endian `b"LNRV"` (`0x56524E4C`).
pub const WIRE_MAGIC: u32 = 0x5652_4E4C;
pub const WIRE_VERSION_V2: u8 = 2;
pub const FRAME_MARKET_PULSE: u8 = 1;
pub const FRAME_READOUT: u8 = 2;

pub const WIRE_HEADER_BYTES: usize = 8;

// ── Legacy v1 sizes (regression only) ──────────────────────────────────────────

pub const MARKET_PULSE_V1_BYTES: usize = 120;
pub const READOUT_PACKET_V1_BYTES: usize = 88;
pub const N_RELEVANCE: usize = 4;
pub const SIDE_EPS: f32 = 1e-4;

/// Deprecated aliases for tests still naming v1 constants.
pub const MARKET_PULSE_BYTES: usize = MARKET_PULSE_V1_BYTES;
pub const READOUT_PACKET_BYTES: usize = READOUT_PACKET_V1_BYTES;

const TRAILING_SIGNALS: usize = 11;

fn peek_magic(buf: &[u8]) -> u32 {
    if buf.len() < 4 {
        return 0;
    }
    u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
}

fn write_header(c: &mut Cursor<&mut [u8]>, frame_type: u8) -> Result<(), String> {
    c.write_all(&WIRE_MAGIC.to_le_bytes())
        .map_err(|e| e.to_string())?;
    c.write_all(&[WIRE_VERSION_V2, frame_type, 0, 0])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn read_header(c: &mut Cursor<&[u8]>) -> Result<u8, String> {
    let mut magic = [0u8; 4];
    c.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if u32::from_le_bytes(magic) != WIRE_MAGIC {
        return Err("invalid wire magic".into());
    }
    let mut rest = [0u8; 4];
    c.read_exact(&mut rest).map_err(|e| e.to_string())?;
    if rest[0] != WIRE_VERSION_V2 {
        return Err(format!("unsupported wire version {}", rest[0]));
    }
    Ok(rest[1])
}

fn trailing_slice(p: &MarketPulse) -> [f32; TRAILING_SIGNALS] {
    [
        p.confidence_signal,
        p.funding_rate,
        p.liquidation_vol,
        p.liquidity_delta,
        p.l3_order_imbalance,
        p.gpu_temp_c,
        p.gpu_power_w,
        p.gpu_util_pct,
        p.basys_buffer_load,
        p.dydx_oi_delta,
        p.dydx_funding_rate,
    ]
}

fn trailing_named(p: &MarketPulse) -> [(&str, f32); TRAILING_SIGNALS] {
    [
        ("confidence_signal", p.confidence_signal),
        ("funding_rate", p.funding_rate),
        ("liquidation_vol", p.liquidation_vol),
        ("liquidity_delta", p.liquidity_delta),
        ("l3_order_imbalance", p.l3_order_imbalance),
        ("gpu_temp_c", p.gpu_temp_c),
        ("gpu_power_w", p.gpu_power_w),
        ("gpu_util_pct", p.gpu_util_pct),
        ("basys_buffer_load", p.basys_buffer_load),
        ("dydx_oi_delta", p.dydx_oi_delta),
        ("dydx_funding_rate", p.dydx_funding_rate),
    ]
}

fn read_trailing(c: &mut Cursor<&[u8]>) -> Result<[f32; TRAILING_SIGNALS], String> {
    let mut out = [0f32; TRAILING_SIGNALS];
    for v in &mut out {
        let mut b = [0u8; 4];
        c.read_exact(&mut b).map_err(|e| e.to_string())?;
        *v = f32::from_le_bytes(b);
    }
    Ok(out)
}

fn apply_trailing(p: &mut MarketPulse, t: [f32; TRAILING_SIGNALS]) {
    p.confidence_signal = t[0];
    p.funding_rate = t[1];
    p.liquidation_vol = t[2];
    p.liquidity_delta = t[3];
    p.l3_order_imbalance = t[4];
    p.gpu_temp_c = t[5];
    p.gpu_power_w = t[6];
    p.gpu_util_pct = t[7];
    p.basys_buffer_load = t[8];
    p.dydx_oi_delta = t[9];
    p.dydx_funding_rate = t[10];
}

/// Market tick packet (feed → brain).
#[derive(Debug, Clone, PartialEq)]
pub struct MarketPulse {
    pub timestamp_ns: u64,
    pub symbol_table_hash: u64,
    pub prices: Vec<f32>,
    pub vols: Vec<f32>,
    pub confidence_signal: f32,
    pub funding_rate: f32,
    pub liquidation_vol: f32,
    pub liquidity_delta: f32,
    pub l3_order_imbalance: f32,
    pub gpu_temp_c: f32,
    pub gpu_power_w: f32,
    pub gpu_util_pct: f32,
    pub basys_buffer_load: f32,
    pub dydx_oi_delta: f32,
    pub dydx_funding_rate: f32,
}

impl MarketPulse {
    pub fn n_assets(&self) -> usize {
        self.prices.len()
    }

    pub fn new_for_universe(universe: &TradeUniverse) -> Self {
        let n = universe.n_assets();
        Self {
            timestamp_ns: 0,
            symbol_table_hash: universe.hash,
            prices: vec![1.0; n],
            vols: vec![0.0; n],
            confidence_signal: 0.0,
            funding_rate: 0.0,
            liquidation_vol: 0.0,
            liquidity_delta: 0.0,
            l3_order_imbalance: 0.0,
            gpu_temp_c: 40.0,
            gpu_power_w: 0.0,
            gpu_util_pct: 0.0,
            basys_buffer_load: 0.0,
            dydx_oi_delta: 0.0,
            dydx_funding_rate: 0.0,
        }
    }

    pub fn frame_len_v2(&self) -> usize {
        WIRE_HEADER_BYTES + 8 + 8 + 4 + 8 * self.n_assets() + TRAILING_SIGNALS * 4
    }

    pub fn pack_v2(&self) -> Result<Vec<u8>, String> {
        let n = self.n_assets();
        if self.vols.len() != n {
            return Err("MarketPulse prices/vols length mismatch".into());
        }
        let mut buf = vec![0u8; self.frame_len_v2()];
        let mut c = Cursor::new(&mut buf[..]);
        write_header(&mut c, FRAME_MARKET_PULSE)?;
        c.write_all(&self.timestamp_ns.to_le_bytes())
            .map_err(|e| e.to_string())?;
        c.write_all(&self.symbol_table_hash.to_le_bytes())
            .map_err(|e| e.to_string())?;
        c.write_all(&(n as u16).to_le_bytes())
            .map_err(|e| e.to_string())?;
        c.write_all(&0u16.to_le_bytes()).map_err(|e| e.to_string())?;
        for i in 0..n {
            c.write_all(&self.prices[i].to_le_bytes())
                .map_err(|e| e.to_string())?;
            c.write_all(&self.vols[i].to_le_bytes())
                .map_err(|e| e.to_string())?;
        }
        for v in trailing_slice(self) {
            c.write_all(&v.to_le_bytes()).map_err(|e| e.to_string())?;
        }
        Ok(buf)
    }

    pub fn pack_v1_legacy(&self) -> Result<[u8; MARKET_PULSE_V1_BYTES], String> {
        if self.n_assets() != 7 {
            return Err("v1 MarketPulse requires exactly 7 assets".into());
        }
        let mut buf = [0u8; MARKET_PULSE_V1_BYTES];
        let mut c = Cursor::new(&mut buf[..]);
        c.write_all(&self.timestamp_ns.to_le_bytes()).unwrap();
        for i in 0..7 {
            c.write_all(&self.prices[i].to_le_bytes()).unwrap();
            c.write_all(&self.vols[i].to_le_bytes()).unwrap();
        }
        for v in trailing_slice(self) {
            c.write_all(&v.to_le_bytes()).unwrap();
        }
        Ok(buf)
    }

    pub fn decode(buf: &[u8], expected: Option<&TradeUniverse>) -> Result<Self, String> {
        if buf.len() >= WIRE_HEADER_BYTES && peek_magic(buf) == WIRE_MAGIC {
            return Self::decode_v2(buf, expected);
        }
        if buf.len() == MARKET_PULSE_V1_BYTES {
            return Self::decode_v1(buf, expected);
        }
        Err(format!(
            "MarketPulse: unrecognized frame ({} bytes)",
            buf.len()
        ))
    }

    fn decode_v2(buf: &[u8], expected: Option<&TradeUniverse>) -> Result<Self, String> {
        let mut c = Cursor::new(buf);
        let frame_type = read_header(&mut c)?;
        if frame_type != FRAME_MARKET_PULSE {
            return Err(format!("expected MarketPulse frame, got type {frame_type}"));
        }
        let mut ts = [0u8; 8];
        c.read_exact(&mut ts).map_err(|e| e.to_string())?;
        let timestamp_ns = u64::from_le_bytes(ts);
        let mut hb = [0u8; 8];
        c.read_exact(&mut hb).map_err(|e| e.to_string())?;
        let symbol_table_hash = u64::from_le_bytes(hb);
        let mut nb = [0u8; 2];
        c.read_exact(&mut nb).map_err(|e| e.to_string())?;
        let n_assets = u16::from_le_bytes(nb) as usize;
        let mut rb = [0u8; 2];
        c.read_exact(&mut rb).map_err(|e| e.to_string())?;
        if n_assets == 0 || n_assets > 256 {
            return Err(format!("invalid n_assets {n_assets}"));
        }
        let expected_len = WIRE_HEADER_BYTES + 8 + 8 + 4 + n_assets * 8 + TRAILING_SIGNALS * 4;
        if buf.len() != expected_len {
            return Err(format!(
                "MarketPulse v2: expected {expected_len} bytes, got {}",
                buf.len()
            ));
        }
        if let Some(u) = expected {
            if symbol_table_hash != u.hash {
                return Err(format!(
                    "symbol_table_hash 0x{symbol_table_hash:016x} != loaded universe 0x{:016x}",
                    u.hash
                ));
            }
            if n_assets != u.n_assets() {
                return Err(format!(
                    "n_assets {n_assets} != universe size {}",
                    u.n_assets()
                ));
            }
        }
        let mut read_f32 = || -> Result<f32, String> {
            let mut b = [0u8; 4];
            c.read_exact(&mut b).map_err(|e| e.to_string())?;
            Ok(f32::from_le_bytes(b))
        };
        let mut prices = vec![0f32; n_assets];
        let mut vols = vec![0f32; n_assets];
        for i in 0..n_assets {
            prices[i] = read_f32()?;
            vols[i] = read_f32()?;
        }
        let mut pulse = Self {
            timestamp_ns,
            symbol_table_hash,
            prices,
            vols,
            confidence_signal: 0.0,
            funding_rate: 0.0,
            liquidation_vol: 0.0,
            liquidity_delta: 0.0,
            l3_order_imbalance: 0.0,
            gpu_temp_c: 0.0,
            gpu_power_w: 0.0,
            gpu_util_pct: 0.0,
            basys_buffer_load: 0.0,
            dydx_oi_delta: 0.0,
            dydx_funding_rate: 0.0,
        };
        apply_trailing(&mut pulse, read_trailing(&mut c)?);
        pulse.validate_and_clamp()?;
        Ok(pulse)
    }

    fn decode_v1(buf: &[u8], expected: Option<&TradeUniverse>) -> Result<Self, String> {
        if let Some(u) = expected {
            if u.n_assets() != 7 {
                return Err("v1 MarketPulse requires a 7-symbol universe".into());
            }
        }
        let mut c = Cursor::new(buf);
        let mut ts = [0u8; 8];
        c.read_exact(&mut ts).map_err(|e| e.to_string())?;
        let timestamp_ns = u64::from_le_bytes(ts);
        let mut read_f32 = || -> Result<f32, String> {
            let mut b = [0u8; 4];
            c.read_exact(&mut b).map_err(|e| e.to_string())?;
            Ok(f32::from_le_bytes(b))
        };
        let mut prices = vec![0f32; 7];
        let mut vols = vec![0f32; 7];
        for i in 0..7 {
            prices[i] = read_f32()?;
            vols[i] = read_f32()?;
        }
        let hash = expected.map(|u| u.hash).unwrap_or(0);
        let mut pulse = Self {
            timestamp_ns,
            symbol_table_hash: hash,
            prices,
            vols,
            confidence_signal: read_f32()?,
            funding_rate: read_f32()?,
            liquidation_vol: read_f32()?,
            liquidity_delta: read_f32()?,
            l3_order_imbalance: read_f32()?,
            gpu_temp_c: read_f32()?,
            gpu_power_w: read_f32()?,
            gpu_util_pct: read_f32()?,
            basys_buffer_load: read_f32()?,
            dydx_oi_delta: read_f32()?,
            dydx_funding_rate: read_f32()?,
        };
        pulse.validate_and_clamp()?;
        Ok(pulse)
    }

    pub fn validate_and_clamp(&mut self) -> Result<(), String> {
        for (i, &p) in self.prices.iter().enumerate() {
            if !p.is_finite() {
                return Err(format!("MarketPulse price[{i}] is not finite: {p}"));
            }
        }
        for (i, &v) in self.vols.iter().enumerate() {
            if !v.is_finite() {
                return Err(format!("MarketPulse vol[{i}] is not finite: {v}"));
            }
        }
        for (name, x) in trailing_named(self) {
            if !x.is_finite() {
                return Err(format!("MarketPulse {name} is not finite: {x}"));
            }
        }
        for (i, &p) in self.prices.iter().enumerate() {
            if p <= 0.0 {
                return Err(format!("MarketPulse price[{i}] must be > 0, got {p}"));
            }
        }
        for v in &mut self.vols {
            *v = v.clamp(0.0, 1.0);
        }
        Ok(())
    }
}

/// Brain readout packet (Julia → Rust muscle).
#[derive(Debug, Clone, PartialEq)]
pub struct ReadoutPacket {
    pub tick: i64,
    pub symbol_table_hash: u64,
    pub readout: Vec<f32>,
    pub relevance: [f32; N_RELEVANCE],
}

impl ReadoutPacket {
    pub fn n_pairs(&self) -> usize {
        self.readout.len() / 2
    }

    pub fn frame_len_v2(&self) -> usize {
        WIRE_HEADER_BYTES + 8 + 8 + 4 + self.readout.len() * 4 + N_RELEVANCE * 4
    }

    pub fn pack_v2(&self) -> Result<Vec<u8>, String> {
        if self.readout.len() % 2 != 0 {
            return Err("readout length must be even (bull/bear pairs)".into());
        }
        let n_pairs = self.n_pairs();
        let mut buf = vec![0u8; self.frame_len_v2()];
        let mut c = Cursor::new(&mut buf[..]);
        write_header(&mut c, FRAME_READOUT)?;
        c.write_all(&self.tick.to_le_bytes())
            .map_err(|e| e.to_string())?;
        c.write_all(&self.symbol_table_hash.to_le_bytes())
            .map_err(|e| e.to_string())?;
        c.write_all(&(n_pairs as u16).to_le_bytes())
            .map_err(|e| e.to_string())?;
        c.write_all(&0u16.to_le_bytes()).map_err(|e| e.to_string())?;
        for v in &self.readout {
            c.write_all(&v.to_le_bytes()).map_err(|e| e.to_string())?;
        }
        for v in &self.relevance {
            c.write_all(&v.to_le_bytes()).map_err(|e| e.to_string())?;
        }
        Ok(buf)
    }

    pub fn pack_v1_legacy(&self) -> Result<[u8; READOUT_PACKET_V1_BYTES], String> {
        if self.readout.len() != 16 {
            return Err("v1 ReadoutPacket requires 16 readout floats".into());
        }
        let mut buf = [0u8; READOUT_PACKET_V1_BYTES];
        let mut c = Cursor::new(&mut buf[..]);
        c.write_all(&self.tick.to_le_bytes()).unwrap();
        for v in &self.readout {
            c.write_all(&v.to_le_bytes()).unwrap();
        }
        for v in &self.relevance {
            c.write_all(&v.to_le_bytes()).unwrap();
        }
        Ok(buf)
    }

    pub fn decode(buf: &[u8], expected: Option<&TradeUniverse>) -> Result<Self, String> {
        if buf.len() >= WIRE_HEADER_BYTES && peek_magic(buf) == WIRE_MAGIC {
            return Self::decode_v2(buf, expected);
        }
        if buf.len() == READOUT_PACKET_V1_BYTES {
            return Self::decode_v1(buf, expected);
        }
        Err(format!(
            "ReadoutPacket: unrecognized frame ({} bytes)",
            buf.len()
        ))
    }

    fn decode_v2(buf: &[u8], expected: Option<&TradeUniverse>) -> Result<Self, String> {
        let mut c = Cursor::new(buf);
        let frame_type = read_header(&mut c)?;
        if frame_type != FRAME_READOUT {
            return Err(format!("expected Readout frame, got type {frame_type}"));
        }
        let mut tb = [0u8; 8];
        c.read_exact(&mut tb).map_err(|e| e.to_string())?;
        let tick = i64::from_le_bytes(tb);
        let mut hb = [0u8; 8];
        c.read_exact(&mut hb).map_err(|e| e.to_string())?;
        let symbol_table_hash = u64::from_le_bytes(hb);
        let mut nb = [0u8; 2];
        c.read_exact(&mut nb).map_err(|e| e.to_string())?;
        let n_pairs = u16::from_le_bytes(nb) as usize;
        let mut rb = [0u8; 2];
        c.read_exact(&mut rb).map_err(|e| e.to_string())?;
        if n_pairs == 0 || n_pairs > 257 {
            return Err(format!("invalid n_pairs {n_pairs}"));
        }
        let n_readout = n_pairs * 2;
        let expected_len = WIRE_HEADER_BYTES + 8 + 8 + 4 + n_readout * 4 + N_RELEVANCE * 4;
        if buf.len() != expected_len {
            return Err(format!(
                "ReadoutPacket v2: expected {expected_len} bytes, got {}",
                buf.len()
            ));
        }
        if let Some(u) = expected {
            if symbol_table_hash != u.hash {
                return Err(format!(
                    "symbol_table_hash 0x{symbol_table_hash:016x} != loaded universe 0x{:016x}",
                    u.hash
                ));
            }
            if n_pairs != u.n_pairs() {
                return Err(format!(
                    "n_pairs {n_pairs} != universe pairs {}",
                    u.n_pairs()
                ));
            }
        }
        let mut readout = vec![0f32; n_readout];
        for r in &mut readout {
            let mut b = [0u8; 4];
            c.read_exact(&mut b).map_err(|e| e.to_string())?;
            *r = f32::from_le_bytes(b);
        }
        let mut relevance = [0f32; N_RELEVANCE];
        for r in &mut relevance {
            let mut b = [0u8; 4];
            c.read_exact(&mut b).map_err(|e| e.to_string())?;
            *r = f32::from_le_bytes(b);
        }
        let packet = Self {
            tick,
            symbol_table_hash,
            readout,
            relevance,
        };
        packet.validate_readout()?;
        Ok(packet)
    }

    fn decode_v1(buf: &[u8], expected: Option<&TradeUniverse>) -> Result<Self, String> {
        if let Some(u) = expected {
            if u.n_pairs() != 8 {
                return Err("v1 ReadoutPacket requires an 8-pair universe".into());
            }
        }
        let mut c = Cursor::new(buf);
        let mut tb = [0u8; 8];
        c.read_exact(&mut tb).map_err(|e| e.to_string())?;
        let tick = i64::from_le_bytes(tb);
        let mut readout = vec![0f32; 16];
        for r in &mut readout {
            let mut b = [0u8; 4];
            c.read_exact(&mut b).map_err(|e| e.to_string())?;
            *r = f32::from_le_bytes(b);
        }
        let mut relevance = [0f32; N_RELEVANCE];
        for r in &mut relevance {
            let mut b = [0u8; 4];
            c.read_exact(&mut b).map_err(|e| e.to_string())?;
            *r = f32::from_le_bytes(b);
        }
        let hash = expected.map(|u| u.hash).unwrap_or(0);
        let packet = Self {
            tick,
            symbol_table_hash: hash,
            readout,
            relevance,
        };
        packet.validate_readout()?;
        Ok(packet)
    }

    fn validate_readout(&self) -> Result<(), String> {
        for (i, &x) in self.readout.iter().enumerate() {
            if !x.is_finite() {
                return Err(format!("ReadoutPacket readout[{i}] is not finite: {x}"));
            }
        }
        for (i, &x) in self.relevance.iter().enumerate() {
            if !x.is_finite() {
                return Err(format!("ReadoutPacket relevance[{i}] is not finite: {x}"));
            }
            if !(0.0..=1.0).contains(&x) {
                return Err(format!(
                    "ReadoutPacket relevance[{i}] must be in [0, 1], got {x}"
                ));
            }
        }
        let rel_sum: f32 = self.relevance.iter().sum();
        if !(0.95..=1.05).contains(&rel_sum) {
            return Err(format!(
                "ReadoutPacket relevance must sum to ~1.0 (±0.05), got {rel_sum}"
            ));
        }
        Ok(())
    }
}

/// Classify an inbound ZMQ payload without length-only guessing.
pub fn classify_wire_frame(buf: &[u8]) -> Result<&'static str, String> {
    if buf.len() >= WIRE_HEADER_BYTES && peek_magic(buf) == WIRE_MAGIC {
        let frame_type = buf[5];
        return Ok(match frame_type {
            FRAME_MARKET_PULSE => "market_v2",
            FRAME_READOUT => "readout_v2",
            _ => return Err(format!("unknown v2 frame type {frame_type}")),
        });
    }
    if buf.len() == MARKET_PULSE_V1_BYTES {
        return Ok("market_v1");
    }
    if buf.len() == READOUT_PACKET_V1_BYTES {
        return Ok("readout_v1");
    }
    Err(format!("unrecognized wire frame ({} bytes)", buf.len()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireSide {
    Buy,
    Sell,
    Neutral,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MappedTrade {
    pub ticker: String,
    pub side: WireSide,
    pub confidence: f32,
    pub score: f32,
    pub pair_index: usize,
    pub tick: i64,
}

fn neutral_trade(
    universe: &TradeUniverse,
    packet: &ReadoutPacket,
    pair_index: usize,
) -> MappedTrade {
    let ticker = universe
        .ticker_for_pair(pair_index)
        .unwrap_or("RESIDUAL")
        .to_string();
    MappedTrade {
        ticker,
        side: WireSide::Neutral,
        confidence: 0.0,
        score: 0.0,
        pair_index,
        tick: packet.tick,
    }
}

/// Map readout + NERO relevance → trade side/confidence (universe-wide ranking).
pub fn readout_to_trade(packet: &ReadoutPacket, universe: &TradeUniverse) -> MappedTrade {
    let n_pairs = packet.n_pairs();
    let mut scores = vec![0f32; n_pairs];
    for (i, score) in scores.iter_mut().enumerate() {
        let bull = packet.readout[2 * i];
        let bear = packet.readout[2 * i + 1];
        *score = bull - bear;
    }

    if let Some(i) = scores.iter().position(|s| !s.is_finite()) {
        return neutral_trade(universe, packet, i);
    }

    let mut primary = 0usize;
    let mut best_abs = 0f32;
    for (i, s) in scores.iter().enumerate() {
        let a = s.abs();
        if a > best_abs {
            best_abs = a;
            primary = i;
        }
    }
    let score = scores[primary];
    let side = if score > SIDE_EPS {
        WireSide::Buy
    } else if score < -SIDE_EPS {
        WireSide::Sell
    } else {
        WireSide::Neutral
    };

    let l2 = scores.iter().map(|s| s * s).sum::<f32>().sqrt();
    if !l2.is_finite() {
        return neutral_trade(universe, packet, primary);
    }
    let rel_sum: f32 = packet.relevance.iter().sum();
    if packet
        .relevance
        .iter()
        .any(|r| !r.is_finite() || !(0.0..=1.0).contains(r))
        || !(0.95..=1.05).contains(&rel_sum)
    {
        return neutral_trade(universe, packet, primary);
    }
    let mag = l2.tanh();
    let max_rel = packet.relevance.iter().cloned().fold(0.0f32, f32::max);
    let confidence = (max_rel * mag).clamp(0.0, 1.0);

    let ticker = universe
        .ticker_for_pair(primary)
        .unwrap_or("RESIDUAL")
        .to_string();

    MappedTrade {
        ticker,
        side,
        confidence,
        score,
        pair_index: primary,
        tick: packet.tick,
    }
}
