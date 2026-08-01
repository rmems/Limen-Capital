//! Binary wire packets for SNN-HFT research (NervousWire v1).
//!
//! Owned by Limen-Capital until Limen-Neural `corpus-ipc` publishes equivalent
//! types on a public rev. Layouts: little-endian fixed size.
//! - [`MarketPulse`]: 120 bytes (market → brain)
//! - [`ReadoutPacket`]: 88 bytes (brain → muscle)
//!
//! Golden fixtures: `wire/fixtures/`.

use std::io::{Cursor, Read, Write};

/// Asset tickers in MarketPulse / readout pair order (0..6). Pair 7 is residual.
pub const ASSET_TICKERS: [&str; 7] = ["DNX", "Quai", "Qubic", "Kaspa", "Monero", "Ocean", "Verus"];

pub const MARKET_PULSE_BYTES: usize = 120;
pub const READOUT_PACKET_BYTES: usize = 88;
pub const N_OUT: usize = 16;
pub const N_RELEVANCE: usize = 4;
pub const N_PAIRS: usize = 8;
pub const SIDE_EPS: f32 = 1e-4;

/// 120-byte market tick packet (Rust/feed → Julia brain).
#[derive(Debug, Clone, PartialEq)]
pub struct MarketPulse {
    pub timestamp_ns: u64,
    /// 7 assets × (price, vol)
    pub prices: [f32; 7],
    pub vols: [f32; 7],
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

impl Default for MarketPulse {
    fn default() -> Self {
        Self {
            timestamp_ns: 0,
            prices: [1.0; 7],
            vols: [0.0; 7],
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
}

impl MarketPulse {
    pub fn pack(&self) -> [u8; MARKET_PULSE_BYTES] {
        let mut buf = [0u8; MARKET_PULSE_BYTES];
        let mut c = Cursor::new(&mut buf[..]);
        c.write_all(&self.timestamp_ns.to_le_bytes()).unwrap();
        for i in 0..7 {
            c.write_all(&self.prices[i].to_le_bytes()).unwrap();
            c.write_all(&self.vols[i].to_le_bytes()).unwrap();
        }
        for v in [
            self.confidence_signal,
            self.funding_rate,
            self.liquidation_vol,
            self.liquidity_delta,
            self.l3_order_imbalance,
            self.gpu_temp_c,
            self.gpu_power_w,
            self.gpu_util_pct,
            self.basys_buffer_load,
            self.dydx_oi_delta,
            self.dydx_funding_rate,
        ] {
            c.write_all(&v.to_le_bytes()).unwrap();
        }
        // bytes 108..120 reserved zero
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, String> {
        if buf.len() != MARKET_PULSE_BYTES {
            return Err(format!(
                "MarketPulse: expected {MARKET_PULSE_BYTES} bytes, got {}",
                buf.len()
            ));
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

        let mut prices = [0f32; 7];
        let mut vols = [0f32; 7];
        for i in 0..7 {
            prices[i] = read_f32()?;
            vols[i] = read_f32()?;
        }
        let mut pulse = Self {
            timestamp_ns,
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

    /// Finite check on all f32 fields; reject price ≤ 0; clamp vols to [0, 1].
    /// Order matches Julia `validate_market_pulse_fields!`: finite-all first, then prices.
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
        for (name, x) in [
            ("confidence_signal", self.confidence_signal),
            ("funding_rate", self.funding_rate),
            ("liquidation_vol", self.liquidation_vol),
            ("liquidity_delta", self.liquidity_delta),
            ("l3_order_imbalance", self.l3_order_imbalance),
            ("gpu_temp_c", self.gpu_temp_c),
            ("gpu_power_w", self.gpu_power_w),
            ("gpu_util_pct", self.gpu_util_pct),
            ("basys_buffer_load", self.basys_buffer_load),
            ("dydx_oi_delta", self.dydx_oi_delta),
            ("dydx_funding_rate", self.dydx_funding_rate),
        ] {
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

/// 88-byte brain readout packet (Julia → Rust muscle).
#[derive(Debug, Clone, PartialEq)]
pub struct ReadoutPacket {
    pub tick: i64,
    pub readout: [f32; N_OUT],
    /// Trailer: Scalper, Day, Swing, Macro relevance (sum ≈ 1).
    pub relevance: [f32; N_RELEVANCE],
}

impl Default for ReadoutPacket {
    fn default() -> Self {
        Self {
            tick: 0,
            readout: [0.0; N_OUT],
            relevance: [0.25; N_RELEVANCE],
        }
    }
}

impl ReadoutPacket {
    pub fn pack(&self) -> [u8; READOUT_PACKET_BYTES] {
        let mut buf = [0u8; READOUT_PACKET_BYTES];
        let mut c = Cursor::new(&mut buf[..]);
        c.write_all(&self.tick.to_le_bytes()).unwrap();
        for v in &self.readout {
            c.write_all(&v.to_le_bytes()).unwrap();
        }
        for v in &self.relevance {
            c.write_all(&v.to_le_bytes()).unwrap();
        }
        buf
    }

    pub fn decode(buf: &[u8]) -> Result<Self, String> {
        if buf.len() != READOUT_PACKET_BYTES {
            return Err(format!(
                "ReadoutPacket: expected {READOUT_PACKET_BYTES} bytes, got {}",
                buf.len()
            ));
        }
        let mut c = Cursor::new(buf);
        let mut tb = [0u8; 8];
        c.read_exact(&mut tb).map_err(|e| e.to_string())?;
        let tick = i64::from_le_bytes(tb);
        let mut readout = [0f32; N_OUT];
        let mut relevance = [0f32; N_RELEVANCE];
        for r in &mut readout {
            let mut b = [0u8; 4];
            c.read_exact(&mut b).map_err(|e| e.to_string())?;
            *r = f32::from_le_bytes(b);
        }
        for r in &mut relevance {
            let mut b = [0u8; 4];
            c.read_exact(&mut b).map_err(|e| e.to_string())?;
            *r = f32::from_le_bytes(b);
        }
        for (i, &x) in readout.iter().enumerate() {
            if !x.is_finite() {
                return Err(format!("ReadoutPacket readout[{i}] is not finite: {x}"));
            }
        }
        for (i, &x) in relevance.iter().enumerate() {
            if !x.is_finite() {
                return Err(format!("ReadoutPacket relevance[{i}] is not finite: {x}"));
            }
        }
        Ok(Self {
            tick,
            readout,
            relevance,
        })
    }
}

/// Side for trade mapping (lowercase JSON / Rust Capital convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireSide {
    Buy,
    Sell,
    Neutral,
}

/// Mapped trade intent from a readout packet.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedTrade {
    pub ticker: String,
    pub side: WireSide,
    pub confidence: f32,
    pub score: f32,
    pub pair_index: usize,
    pub tick: i64,
}

/// Map readout + NERO relevance → trade side/confidence (NervousWire v1).
///
/// Finite extremes (e.g. `f32::MAX - (-f32::MAX)`) can overflow to non-finite
/// scores; those map to Neutral with zero confidence (fail closed).
pub fn readout_to_trade(packet: &ReadoutPacket) -> MappedTrade {
    let mut scores = [0f32; N_PAIRS];
    for (i, score) in scores.iter_mut().enumerate() {
        let bull = packet.readout[2 * i];
        let bear = packet.readout[2 * i + 1];
        *score = bull - bear;
    }

    // Derived overflow / NaN must not become max-confidence trades.
    if scores.iter().any(|s| !s.is_finite()) {
        return MappedTrade {
            ticker: ASSET_TICKERS[0].to_string(),
            side: WireSide::Neutral,
            confidence: 0.0,
            score: 0.0,
            pair_index: 0,
            tick: packet.tick,
        };
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
        return MappedTrade {
            ticker: if primary < ASSET_TICKERS.len() {
                ASSET_TICKERS[primary].to_string()
            } else {
                "RESIDUAL".to_string()
            },
            side: WireSide::Neutral,
            confidence: 0.0,
            score: 0.0,
            pair_index: primary,
            tick: packet.tick,
        };
    }
    let mag = l2.tanh();
    let max_rel = packet
        .relevance
        .iter()
        .cloned()
        .fold(0.0f32, f32::max)
        .clamp(0.0, 1.0);
    let confidence = (max_rel * mag).clamp(0.0, 1.0);
    if !confidence.is_finite() {
        return MappedTrade {
            ticker: if primary < ASSET_TICKERS.len() {
                ASSET_TICKERS[primary].to_string()
            } else {
                "RESIDUAL".to_string()
            },
            side: WireSide::Neutral,
            confidence: 0.0,
            score: 0.0,
            pair_index: primary,
            tick: packet.tick,
        };
    }

    let ticker = if primary < ASSET_TICKERS.len() {
        ASSET_TICKERS[primary].to_string()
    } else {
        "RESIDUAL".to_string()
    };

    MappedTrade {
        ticker,
        side,
        confidence,
        score,
        pair_index: primary,
        tick: packet.tick,
    }
}
