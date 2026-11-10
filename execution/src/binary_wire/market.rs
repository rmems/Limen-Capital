use super::cursor_io::{
    read_f32s, read_header, read_price_vol, read_u16, read_u64, require_count, require_len,
    split_price_vol, write_bytes, write_f32s, write_header, write_price_vol,
};
use super::{FRAME_MARKET_PULSE, MARKET_PULSE_V1_BYTES, TRAILING_SIGNALS, WIRE_HEADER_BYTES};
use crate::universe::TradeUniverse;
use std::io::Cursor;

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
        market_v2_len(self.n_assets())
    }

    pub fn pack_v2(&self) -> Result<Vec<u8>, String> {
        let n = self.n_assets();
        if self.vols.len() != n {
            return Err("MarketPulse prices/vols length mismatch".into());
        }
        let mut buf = vec![0u8; self.frame_len_v2()];
        let mut c = Cursor::new(&mut buf[..]);
        write_market_prefix(&mut c, self, n)?;
        write_price_vol(&mut c, &self.prices, &self.vols)?;
        write_f32s(&mut c, &trailing_slice(self))?;
        Ok(buf)
    }

    pub fn pack_v1_legacy(&self) -> Result<[u8; MARKET_PULSE_V1_BYTES], String> {
        if self.n_assets() != 7 {
            return Err("v1 MarketPulse requires exactly 7 assets".into());
        }
        let mut buf = [0u8; MARKET_PULSE_V1_BYTES];
        let mut c = Cursor::new(&mut buf[..]);
        write_bytes(&mut c, &self.timestamp_ns.to_le_bytes()).unwrap();
        write_price_vol(&mut c, &self.prices, &self.vols).unwrap();
        write_f32s(&mut c, &trailing_slice(self)).unwrap();
        Ok(buf)
    }

    pub fn decode(buf: &[u8], expected: Option<&TradeUniverse>) -> Result<Self, String> {
        if super::cursor_io::peek_magic(buf) == super::WIRE_MAGIC && buf.len() >= WIRE_HEADER_BYTES
        {
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
        let (timestamp_ns, symbol_table_hash, n_assets) = read_market_prefix(&mut c)?;
        require_len(buf.len(), market_v2_len(n_assets), "MarketPulse v2")?;
        require_market_universe(expected, symbol_table_hash, n_assets)?;
        let (prices, vols) = read_price_vol(&mut c, n_assets)?;
        let trailing = read_f32s(&mut c, TRAILING_SIGNALS)?;
        let mut pulse = from_parts(timestamp_ns, symbol_table_hash, prices, vols, &trailing);
        pulse.validate_and_clamp()?;
        Ok(pulse)
    }

    fn decode_v1(buf: &[u8], expected: Option<&TradeUniverse>) -> Result<Self, String> {
        require_v1_universe(expected)?;
        let mut c = Cursor::new(buf);
        let timestamp_ns = read_u64(&mut c)?;
        let words = read_f32s(&mut c, 14)?;
        let trailing = read_f32s(&mut c, TRAILING_SIGNALS)?;
        let (prices, vols) = split_price_vol(&words);
        let hash = expected.map(|u| u.hash).unwrap_or(0);
        let mut pulse = from_parts(timestamp_ns, hash, prices, vols, &trailing);
        pulse.validate_and_clamp()?;
        Ok(pulse)
    }

    pub fn validate_and_clamp(&mut self) -> Result<(), String> {
        reject_non_finite(&self.prices, "price")?;
        reject_non_finite(&self.vols, "vol")?;
        reject_trailing(self)?;
        reject_non_positive(&self.prices)?;
        for v in &mut self.vols {
            *v = v.clamp(0.0, 1.0);
        }
        Ok(())
    }
}

fn market_v2_len(n_assets: usize) -> usize {
    WIRE_HEADER_BYTES + 8 + 8 + 4 + n_assets * 8 + TRAILING_SIGNALS * 4
}

fn write_market_prefix(
    c: &mut Cursor<&mut [u8]>,
    pulse: &MarketPulse,
    n: usize,
) -> Result<(), String> {
    write_header(c, FRAME_MARKET_PULSE)?;
    write_bytes(c, &pulse.timestamp_ns.to_le_bytes())?;
    write_bytes(c, &pulse.symbol_table_hash.to_le_bytes())?;
    write_bytes(c, &(n as u16).to_le_bytes())?;
    write_bytes(c, &0u16.to_le_bytes())
}

fn read_market_prefix(c: &mut Cursor<&[u8]>) -> Result<(u64, u64, usize), String> {
    let frame_type = read_header(c)?;
    if frame_type != FRAME_MARKET_PULSE {
        return Err(format!("expected MarketPulse frame, got type {frame_type}"));
    }
    let timestamp_ns = read_u64(c)?;
    let symbol_table_hash = read_u64(c)?;
    let n_assets = usize::from(read_u16(c)?);
    let _reserved = read_u16(c)?;
    require_count(n_assets, 256, "n_assets")?;
    Ok((timestamp_ns, symbol_table_hash, n_assets))
}

fn require_market_universe(
    expected: Option<&TradeUniverse>,
    hash: u64,
    n_assets: usize,
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
    if n_assets != u.n_assets() {
        return Err(format!(
            "n_assets {n_assets} != universe size {}",
            u.n_assets()
        ));
    }
    Ok(())
}

fn require_v1_universe(expected: Option<&TradeUniverse>) -> Result<(), String> {
    if let Some(u) = expected {
        if u.n_assets() != 7 {
            return Err("v1 MarketPulse requires a 7-symbol universe".into());
        }
    }
    Ok(())
}

fn from_parts(
    timestamp_ns: u64,
    symbol_table_hash: u64,
    prices: Vec<f32>,
    vols: Vec<f32>,
    trailing: &[f32],
) -> MarketPulse {
    let mut pulse = MarketPulse {
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
    let mut arr = [0f32; TRAILING_SIGNALS];
    arr.copy_from_slice(&trailing[..TRAILING_SIGNALS]);
    apply_trailing(&mut pulse, arr);
    pulse
}

fn reject_non_finite(values: &[f32], label: &str) -> Result<(), String> {
    for (i, &v) in values.iter().enumerate() {
        if !v.is_finite() {
            return Err(format!("MarketPulse {label}[{i}] is not finite: {v}"));
        }
    }
    Ok(())
}

fn reject_non_positive(prices: &[f32]) -> Result<(), String> {
    for (i, &p) in prices.iter().enumerate() {
        if p <= 0.0 {
            return Err(format!("MarketPulse price[{i}] must be > 0, got {p}"));
        }
    }
    Ok(())
}

fn reject_trailing(p: &MarketPulse) -> Result<(), String> {
    for (name, x) in trailing_named(p) {
        if !x.is_finite() {
            return Err(format!("MarketPulse {name} is not finite: {x}"));
        }
    }
    Ok(())
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
