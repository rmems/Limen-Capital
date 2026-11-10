//! Small cursor helpers so pack/decode methods stay under complexity limits.

use super::{WIRE_MAGIC, WIRE_VERSION_V2};
use std::io::{Cursor, Read, Write};

pub(crate) fn peek_magic(buf: &[u8]) -> u32 {
    if buf.len() < 4 {
        return 0;
    }
    u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
}

pub(crate) fn write_bytes(c: &mut Cursor<&mut [u8]>, bytes: &[u8]) -> Result<(), String> {
    c.write_all(bytes).map_err(|e| e.to_string())
}

pub(crate) fn write_header(c: &mut Cursor<&mut [u8]>, frame_type: u8) -> Result<(), String> {
    write_bytes(c, &WIRE_MAGIC.to_le_bytes())?;
    write_bytes(c, &[WIRE_VERSION_V2, frame_type, 0, 0])
}

pub(crate) fn read_header(c: &mut Cursor<&[u8]>) -> Result<u8, String> {
    let magic: [u8; 4] = read_array(c)?;
    if u32::from_le_bytes(magic) != WIRE_MAGIC {
        return Err("invalid wire magic".into());
    }
    let rest: [u8; 4] = read_array(c)?;
    if rest[0] != WIRE_VERSION_V2 {
        return Err(format!("unsupported wire version {}", rest[0]));
    }
    require_reserved_zero(&rest[2..])?;
    Ok(rest[1])
}

pub(crate) fn read_reserved_u16(c: &mut Cursor<&[u8]>) -> Result<(), String> {
    require_reserved_zero(&read_array::<2>(c)?)
}

fn require_reserved_zero(bytes: &[u8]) -> Result<(), String> {
    if bytes.iter().any(|&b| b != 0) {
        return Err("reserved wire bytes must be zero".into());
    }
    Ok(())
}

pub(crate) fn read_array<const N: usize>(c: &mut Cursor<&[u8]>) -> Result<[u8; N], String> {
    let mut b = [0u8; N];
    c.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

pub(crate) fn read_u64(c: &mut Cursor<&[u8]>) -> Result<u64, String> {
    Ok(u64::from_le_bytes(read_array(c)?))
}

pub(crate) fn read_i64(c: &mut Cursor<&[u8]>) -> Result<i64, String> {
    Ok(i64::from_le_bytes(read_array(c)?))
}

pub(crate) fn read_u16(c: &mut Cursor<&[u8]>) -> Result<u16, String> {
    Ok(u16::from_le_bytes(read_array(c)?))
}

pub(crate) fn read_f32(c: &mut Cursor<&[u8]>) -> Result<f32, String> {
    Ok(f32::from_le_bytes(read_array(c)?))
}

pub(crate) fn read_f32s(c: &mut Cursor<&[u8]>, n: usize) -> Result<Vec<f32>, String> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(read_f32(c)?);
    }
    Ok(out)
}

pub(crate) fn write_f32(c: &mut Cursor<&mut [u8]>, v: f32) -> Result<(), String> {
    write_bytes(c, &v.to_le_bytes())
}

pub(crate) fn write_f32s(c: &mut Cursor<&mut [u8]>, values: &[f32]) -> Result<(), String> {
    for v in values {
        write_f32(c, *v)?;
    }
    Ok(())
}

pub(crate) fn write_price_vol(
    c: &mut Cursor<&mut [u8]>,
    prices: &[f32],
    vols: &[f32],
) -> Result<(), String> {
    for i in 0..prices.len() {
        write_f32(c, prices[i])?;
        write_f32(c, vols[i])?;
    }
    Ok(())
}

pub(crate) fn read_price_vol(
    c: &mut Cursor<&[u8]>,
    n: usize,
) -> Result<(Vec<f32>, Vec<f32>), String> {
    let mut prices = Vec::with_capacity(n);
    let mut vols = Vec::with_capacity(n);
    for _ in 0..n {
        prices.push(read_f32(c)?);
        vols.push(read_f32(c)?);
    }
    Ok((prices, vols))
}

pub(crate) fn split_price_vol(words: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let prices = words.iter().step_by(2).copied().collect();
    let vols = words.iter().skip(1).step_by(2).copied().collect();
    (prices, vols)
}

pub(crate) fn require_len(actual: usize, expected: usize, what: &str) -> Result<(), String> {
    if actual != expected {
        return Err(format!("{what}: expected {expected} bytes, got {actual}"));
    }
    Ok(())
}

pub(crate) fn require_count(n: usize, max: usize, label: &str) -> Result<(), String> {
    if n == 0 || n > max {
        return Err(format!("invalid {label} {n}"));
    }
    Ok(())
}
