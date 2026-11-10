//! Runtime tradeable universe loaded from TOML (see `config/universe.toml`).

use std::path::{Path, PathBuf};

/// FNV-1a 64-bit — must match `brain/universe.jl::symbol_table_hash`.
const FNV_OFFSET: u64 = 14695981039346656037;
const FNV_PRIME: u64 = 1099511628211;

fn fnv_byte(mut hash: u64, byte: u8) -> u64 {
    hash ^= u64::from(byte);
    hash = hash.wrapping_mul(FNV_PRIME);
    hash
}

/// Hash of ordered tickers joined by `\n` (stable across Rust/Julia).
pub fn symbol_table_hash(symbols: &[String]) -> u64 {
    let mut h = FNV_OFFSET;
    for (i, sym) in symbols.iter().enumerate() {
        if i > 0 {
            h = fnv_byte(h, b'\n');
        }
        for &b in sym.as_bytes() {
            h = fnv_byte(h, b);
        }
    }
    h
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeUniverse {
    pub symbols: Vec<String>,
    pub hash: u64,
}

fn parse_symbols(raw: &str) -> Result<Vec<String>, String> {
    let doc: toml::Value = raw.parse().map_err(|e| format!("parse universe: {e}"))?;
    let arr = symbol_array(&doc)?;
    let mut symbols = Vec::with_capacity(arr.len());
    for entry in arr {
        symbols.push(symbol_name(entry)?);
    }
    Ok(symbols)
}

fn symbol_array(doc: &toml::Value) -> Result<&Vec<toml::Value>, String> {
    doc.get("symbols")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "universe.toml missing `symbols` array".to_string())
}

fn symbol_name(entry: &toml::Value) -> Result<String, String> {
    let s = entry
        .as_str()
        .ok_or_else(|| "each symbol must be a string".to_string())?
        .trim();
    if s.is_empty() {
        return Err("empty symbol in universe".into());
    }
    Ok(s.to_string())
}

impl TradeUniverse {
    pub fn new(symbols: Vec<String>) -> Result<Self, String> {
        if symbols.is_empty() {
            return Err("universe must contain at least one symbol".into());
        }
        if symbols.len() > 256 {
            return Err(format!(
                "universe too large: {} symbols (max 256)",
                symbols.len()
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for s in &symbols {
            if !seen.insert(s.clone()) {
                return Err(format!("duplicate symbol in universe: {s}"));
            }
        }
        let hash = symbol_table_hash(&symbols);
        Ok(Self { symbols, hash })
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("read universe {}: {e}", path.display()))?;
        Self::new(parse_symbols(&raw).map_err(|e| format!("{e} ({})", path.display()))?)
    }

    pub fn resolve_path() -> Result<PathBuf, String> {
        if let Ok(raw) = std::env::var("LIMEN_UNIVERSE") {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                let path = PathBuf::from(trimmed);
                if path.is_absolute() {
                    return Ok(path);
                }
                let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
                return Ok(root.join(path));
            }
        }
        Ok(Self::default_config_path())
    }

    pub fn default_config_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../config/universe.toml")
    }

    /// Hash of the checked-in `config/universe.toml`, independent of `LIMEN_UNIVERSE`.
    pub fn canonical_v1_hash() -> Result<u64, String> {
        use std::sync::OnceLock;
        static CELL: OnceLock<u64> = OnceLock::new();
        if let Some(h) = CELL.get() {
            return Ok(*h);
        }
        let h = Self::load(&Self::default_config_path())?.hash;
        Ok(*CELL.get_or_init(|| h))
    }

    pub fn load_default() -> Result<Self, String> {
        Self::load(&Self::resolve_path()?)
    }

    pub fn n_assets(&self) -> usize {
        self.symbols.len()
    }

    /// Tradeable pairs + one residual slot.
    pub fn n_pairs(&self) -> usize {
        self.n_assets() + 1
    }

    pub fn n_out(&self) -> usize {
        2 * self.n_pairs()
    }

    pub fn ticker_for_pair(&self, pair_index: usize) -> Option<&str> {
        if pair_index < self.n_assets() {
            Some(self.symbols[pair_index].as_str())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_default_universe_seven() {
        let u = TradeUniverse::load_default().expect("default universe");
        assert_eq!(u.n_assets(), 7);
        assert_eq!(u.n_pairs(), 8);
        assert_eq!(u.n_out(), 16);
    }

    #[test]
    fn load_fixture_three_and_twelve() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../wire/fixtures");
        let u3 = TradeUniverse::load(&root.join("universe_3.toml")).unwrap();
        assert_eq!(u3.n_assets(), 3);
        assert_eq!(u3.n_out(), 8);
        let u12 = TradeUniverse::load(&root.join("universe_12.toml")).unwrap();
        assert_eq!(u12.n_assets(), 12);
        assert_eq!(u12.n_out(), 26);
    }
}
