use super::cursor_io::{
    read_f32s, read_header, read_i64, read_u16, read_u64, require_count, require_len, write_bytes,
    write_f32s, write_header,
};
use super::{FRAME_READOUT, N_RELEVANCE, READOUT_PACKET_V1_BYTES, SIDE_EPS, WIRE_HEADER_BYTES};
use crate::universe::TradeUniverse;
use std::io::Cursor;

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
        readout_v2_len(self.n_pairs())
    }

    pub fn pack_v2(&self) -> Result<Vec<u8>, String> {
        if !self.readout.len().is_multiple_of(2) {
            return Err("readout length must be even (bull/bear pairs)".into());
        }
        let n_pairs = self.n_pairs();
        let mut buf = vec![0u8; self.frame_len_v2()];
        let mut c = Cursor::new(&mut buf[..]);
        write_readout_prefix(&mut c, self, n_pairs)?;
        write_f32s(&mut c, &self.readout)?;
        write_f32s(&mut c, &self.relevance)?;
        Ok(buf)
    }

    pub fn pack_v1_legacy(&self) -> Result<[u8; READOUT_PACKET_V1_BYTES], String> {
        if self.readout.len() != 16 {
            return Err("v1 ReadoutPacket requires 16 readout floats".into());
        }
        let mut buf = [0u8; READOUT_PACKET_V1_BYTES];
        let mut c = Cursor::new(&mut buf[..]);
        write_bytes(&mut c, &self.tick.to_le_bytes()).unwrap();
        write_f32s(&mut c, &self.readout).unwrap();
        write_f32s(&mut c, &self.relevance).unwrap();
        Ok(buf)
    }

    pub fn decode(buf: &[u8], expected: Option<&TradeUniverse>) -> Result<Self, String> {
        if super::cursor_io::peek_magic(buf) == super::WIRE_MAGIC && buf.len() >= WIRE_HEADER_BYTES
        {
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
        let (tick, symbol_table_hash, n_pairs) = read_readout_prefix(&mut c)?;
        require_len(buf.len(), readout_v2_len(n_pairs), "ReadoutPacket v2")?;
        require_readout_universe(expected, symbol_table_hash, n_pairs)?;
        let readout = read_f32s(&mut c, n_pairs * 2)?;
        let relevance = relevance_array(&read_f32s(&mut c, N_RELEVANCE)?);
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
        require_v1_pairs(expected)?;
        let mut c = Cursor::new(buf);
        let tick = read_i64(&mut c)?;
        let readout = read_f32s(&mut c, 16)?;
        let relevance = relevance_array(&read_f32s(&mut c, N_RELEVANCE)?);
        let packet = Self {
            tick,
            symbol_table_hash: expected.map(|u| u.hash).unwrap_or(0),
            readout,
            relevance,
        };
        packet.validate_readout()?;
        Ok(packet)
    }

    fn validate_readout(&self) -> Result<(), String> {
        reject_non_finite(&self.readout, "readout")?;
        reject_relevance(&self.relevance)?;
        let rel_sum: f32 = self.relevance.iter().sum();
        if !(0.95..=1.05).contains(&rel_sum) {
            return Err(format!(
                "ReadoutPacket relevance must sum to ~1.0 (±0.05), got {rel_sum}"
            ));
        }
        Ok(())
    }
}

fn readout_v2_len(n_pairs: usize) -> usize {
    WIRE_HEADER_BYTES + 8 + 8 + 4 + n_pairs * 8 + N_RELEVANCE * 4
}

fn write_readout_prefix(
    c: &mut Cursor<&mut [u8]>,
    packet: &ReadoutPacket,
    n_pairs: usize,
) -> Result<(), String> {
    write_header(c, FRAME_READOUT)?;
    write_bytes(c, &packet.tick.to_le_bytes())?;
    write_bytes(c, &packet.symbol_table_hash.to_le_bytes())?;
    write_bytes(c, &(n_pairs as u16).to_le_bytes())?;
    write_bytes(c, &0u16.to_le_bytes())
}

fn read_readout_prefix(c: &mut Cursor<&[u8]>) -> Result<(i64, u64, usize), String> {
    let frame_type = read_header(c)?;
    if frame_type != FRAME_READOUT {
        return Err(format!("expected Readout frame, got type {frame_type}"));
    }
    let tick = read_i64(c)?;
    let symbol_table_hash = read_u64(c)?;
    let n_pairs = usize::from(read_u16(c)?);
    let _reserved = read_u16(c)?;
    require_count(n_pairs, 257, "n_pairs")?;
    Ok((tick, symbol_table_hash, n_pairs))
}

fn require_readout_universe(
    expected: Option<&TradeUniverse>,
    hash: u64,
    n_pairs: usize,
) -> Result<(), String> {
    let Some(u) = expected else {
        return Ok(());
    };
    if hash != u.hash {
        return Err(format!(
            "symbol_table_hash 0x{hash:016x} != loaded universe 0x{:016x}",
            u.hash
        ));
    }
    if n_pairs != u.n_pairs() {
        return Err(format!(
            "n_pairs {n_pairs} != universe pairs {}",
            u.n_pairs()
        ));
    }
    Ok(())
}

fn require_v1_pairs(expected: Option<&TradeUniverse>) -> Result<(), String> {
    if let Some(u) = expected {
        if u.n_pairs() != 8 {
            return Err("v1 ReadoutPacket requires an 8-pair universe".into());
        }
    }
    Ok(())
}

fn relevance_array(values: &[f32]) -> [f32; N_RELEVANCE] {
    let mut relevance = [0f32; N_RELEVANCE];
    relevance.copy_from_slice(&values[..N_RELEVANCE]);
    relevance
}

fn reject_non_finite(values: &[f32], label: &str) -> Result<(), String> {
    for (i, &x) in values.iter().enumerate() {
        if !x.is_finite() {
            return Err(format!("ReadoutPacket {label}[{i}] is not finite: {x}"));
        }
    }
    Ok(())
}

fn reject_relevance(relevance: &[f32; N_RELEVANCE]) -> Result<(), String> {
    for (i, &x) in relevance.iter().enumerate() {
        if !x.is_finite() {
            return Err(format!("ReadoutPacket relevance[{i}] is not finite: {x}"));
        }
        if !(0.0..=1.0).contains(&x) {
            return Err(format!(
                "ReadoutPacket relevance[{i}] must be in [0, 1], got {x}"
            ));
        }
    }
    Ok(())
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
    MappedTrade {
        ticker: universe
            .ticker_for_pair(pair_index)
            .unwrap_or("RESIDUAL")
            .to_string(),
        side: WireSide::Neutral,
        confidence: 0.0,
        score: 0.0,
        pair_index,
        tick: packet.tick,
    }
}

/// Map readout + NERO relevance → trade side/confidence (universe-wide ranking).
pub fn readout_to_trade(packet: &ReadoutPacket, universe: &TradeUniverse) -> MappedTrade {
    let scores = pair_scores(packet);
    if let Some(i) = scores.iter().position(|s| !s.is_finite()) {
        return neutral_trade(universe, packet, i);
    }
    let primary = argmax_abs(&scores);
    let l2 = l2_norm(&scores);
    if !l2.is_finite() {
        return neutral_trade(universe, packet, primary);
    }
    let Some(confidence) = confidence_from_relevance(&packet.relevance, l2) else {
        return neutral_trade(universe, packet, primary);
    };
    let score = scores[primary];
    MappedTrade {
        ticker: universe
            .ticker_for_pair(primary)
            .unwrap_or("RESIDUAL")
            .to_string(),
        side: side_of(score),
        confidence,
        score,
        pair_index: primary,
        tick: packet.tick,
    }
}

fn pair_scores(packet: &ReadoutPacket) -> Vec<f32> {
    (0..packet.n_pairs())
        .map(|i| packet.readout[2 * i] - packet.readout[2 * i + 1])
        .collect()
}

fn argmax_abs(scores: &[f32]) -> usize {
    let mut primary = 0usize;
    let mut best_abs = 0f32;
    for (i, s) in scores.iter().enumerate() {
        let a = s.abs();
        if a > best_abs {
            best_abs = a;
            primary = i;
        }
    }
    primary
}

fn side_of(score: f32) -> WireSide {
    if score > SIDE_EPS {
        WireSide::Buy
    } else if score < -SIDE_EPS {
        WireSide::Sell
    } else {
        WireSide::Neutral
    }
}

fn l2_norm(scores: &[f32]) -> f32 {
    scores.iter().map(|s| s * s).sum::<f32>().sqrt()
}

fn confidence_from_relevance(relevance: &[f32; N_RELEVANCE], l2: f32) -> Option<f32> {
    if !relevance_simplex(relevance) {
        return None;
    }
    let mag = l2.tanh();
    let max_rel = relevance.iter().copied().fold(0.0f32, f32::max);
    Some((max_rel * mag).clamp(0.0, 1.0))
}

fn relevance_simplex(relevance: &[f32; N_RELEVANCE]) -> bool {
    let in_range = relevance
        .iter()
        .all(|r| r.is_finite() && (0.0..=1.0).contains(r));
    let rel_sum: f32 = relevance.iter().sum();
    in_range && (0.95..=1.05).contains(&rel_sum)
}
