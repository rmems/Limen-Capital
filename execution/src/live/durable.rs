//! Durable live-session state, persisted atomically across restarts.
//!
//! Holds what the venue cannot tell us: the UTC day and its realized-PnL
//! baseline (so a restart does not reset the daily-loss counter), LLM spend
//! today, the last order time, every `client_order_id` this process has
//! submitted and not yet seen terminal, and the fill log that explains the
//! holding and its average cost (the venue reports holdings without a cost
//! basis this book trusts).
//!
//! Paper mode never uses this: its counters reset with the process.

use crate::live::robinhood::types::OrderSide;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const STATE_VERSION: u32 = 1;

/// Quantity below which a residual position is float dust.
pub const QTY_EPS: f64 = 1e-9;

/// Where a submitted intent stands, as far as this process knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentStatus {
    /// Persisted, POST about to be sent or in flight.
    Submitting,
    /// POST outcome unknown (lost response) and the lookup has not found it yet.
    Unconfirmed,
    /// The venue acknowledged the order.
    Acknowledged,
}

/// One order intent, keyed by the `client_order_id` minted by `risk::check`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackedOrder {
    pub client_order_id: String,
    #[serde(default)]
    pub venue_order_id: Option<String>,
    pub symbol: String,
    pub side: OrderSide,
    pub asset_qty: f64,
    pub limit_price: f64,
    pub submitted_secs: i64,
    pub status: IntentStatus,
}

/// One fill increment accounted for by this book.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillRecord {
    /// `<client_order_id>#<n>`; unique and stable across restarts.
    pub fill_id: String,
    pub client_order_id: String,
    pub symbol: String,
    pub side: OrderSide,
    pub qty: f64,
    pub price: f64,
    pub fee_usd: f64,
    pub ts_secs: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableState {
    pub version: u32,
    /// Venue account this state belongs to; set on first start.
    #[serde(default)]
    pub account_id: Option<String>,
    pub symbol: String,
    /// Days since CE (chrono `num_days_from_ce`) of the current UTC day.
    pub utc_day: i32,
    pub day_start_realized_usd: f64,
    pub llm_spend_today_usd: f64,
    #[serde(default)]
    pub last_order_secs: Option<i64>,
    #[serde(default)]
    pub open_orders: Vec<TrackedOrder>,
    #[serde(default)]
    pub fills: Vec<FillRecord>,
}

impl DurableState {
    pub fn new(symbol: &str, utc_day: i32) -> Self {
        Self {
            version: STATE_VERSION,
            account_id: None,
            symbol: symbol.to_string(),
            utc_day,
            day_start_realized_usd: 0.0,
            llm_spend_today_usd: 0.0,
            last_order_secs: None,
            open_orders: Vec::new(),
            fills: Vec::new(),
        }
    }

    /// Quantity already accounted for on one order.
    pub fn filled_qty(&self, client_order_id: &str) -> (f64, f64, usize) {
        self.fills
            .iter()
            .filter(|f| f.client_order_id == client_order_id)
            .fold((0.0, 0.0, 0), |(q, n, c), f| {
                (q + f.qty, n + f.qty * f.price, c + 1)
            })
    }
}

/// Position, average cost and realized PnL rebuilt from the fill log.
///
/// Same accounting as the paper book: buy fees fold into average cost; a
/// sell realizes `(price − avg_cost) × qty − fee`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CostBasis {
    pub position_qty: f64,
    pub avg_cost_usd: Option<f64>,
    pub realized_pnl_usd: f64,
    pub fees_paid_usd: f64,
}

impl CostBasis {
    /// Apply one fill; returns the PnL it realized. Errors on a sell larger
    /// than the logged position (the log cannot explain it).
    pub fn apply(&mut self, fill: &FillRecord) -> Result<f64, String> {
        let notional = fill.qty * fill.price;
        self.fees_paid_usd += fill.fee_usd;
        match fill.side {
            OrderSide::Buy => {
                let prev = self.avg_cost_usd.unwrap_or(0.0) * self.position_qty;
                self.position_qty += fill.qty;
                self.avg_cost_usd = Some((prev + notional + fill.fee_usd) / self.position_qty);
                Ok(0.0)
            }
            OrderSide::Sell => {
                if fill.qty > self.position_qty + QTY_EPS {
                    return Err(format!(
                        "fill {} sells {} but the log holds {}",
                        fill.fill_id, fill.qty, self.position_qty
                    ));
                }
                let cost = self.avg_cost_usd.unwrap_or(fill.price);
                let realized = (fill.price - cost) * fill.qty - fill.fee_usd;
                self.position_qty -= fill.qty;
                if self.position_qty <= QTY_EPS {
                    self.position_qty = 0.0;
                    self.avg_cost_usd = None;
                }
                self.realized_pnl_usd += realized;
                Ok(realized)
            }
        }
    }

    pub fn from_fills(fills: &[FillRecord]) -> Result<Self, String> {
        let mut basis = Self::default();
        for fill in fills {
            basis.apply(fill)?;
        }
        Ok(basis)
    }
}

/// Atomic JSON persistence: write a temp file, fsync, rename over the old
/// file, fsync the directory. A crash leaves either the old or the new state.
#[derive(Debug, Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> io::Result<Option<DurableState>> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let state: DurableState = serde_json::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if state.version != STATE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}: state version {} (expected {STATE_VERSION})",
                    self.path.display(),
                    state.version
                ),
            ));
        }
        Ok(Some(state))
    }

    pub fn save(&self, state: &DurableState) -> io::Result<()> {
        let dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(dir)?;
        let body = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        let mut tmp_name = self.path.as_os_str().to_owned();
        tmp_name.push(".tmp");
        let tmp = PathBuf::from(tmp_name);
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)?;
            f.write_all(&body)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        // Directory fsync makes the rename itself durable (POSIX).
        #[cfg(unix)]
        File::open(dir)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(id: &str, side: OrderSide, qty: f64, price: f64, fee: f64) -> FillRecord {
        FillRecord {
            fill_id: id.into(),
            client_order_id: id.split('#').next().unwrap().into(),
            symbol: "SPY".into(),
            side,
            qty,
            price,
            fee_usd: fee,
            ts_secs: 0,
        }
    }

    #[test]
    fn cost_basis_matches_hand_computation() {
        // Buy 0.05 @ 100 fee 0.005 → cost 5.005, avg 100.1.
        // Buy 0.05 @ 110 fee 0.0055 → cost 10.5105, avg 105.105.
        // Sell 0.1 @ 90 fee 0.009 → realized 9 − 10.5105 − 0.009 = −1.5195.
        let fills = [
            fill("a#1", OrderSide::Buy, 0.05, 100.0, 0.005),
            fill("b#1", OrderSide::Buy, 0.05, 110.0, 0.0055),
            fill("c#1", OrderSide::Sell, 0.1, 90.0, 0.009),
        ];
        let mut basis = CostBasis::default();
        basis.apply(&fills[0]).unwrap();
        assert!((basis.avg_cost_usd.unwrap() - 100.1).abs() < 1e-9);
        basis.apply(&fills[1]).unwrap();
        assert!((basis.avg_cost_usd.unwrap() - 105.105).abs() < 1e-9);
        let realized = basis.apply(&fills[2]).unwrap();
        assert!((realized - -1.5195).abs() < 1e-9, "{realized}");
        assert_eq!(basis.position_qty, 0.0);
        assert_eq!(basis.avg_cost_usd, None);
        assert_eq!(CostBasis::from_fills(&fills).unwrap(), basis);
        assert!((basis.fees_paid_usd - 0.0195).abs() < 1e-12);
    }

    #[test]
    fn oversell_cannot_be_explained() {
        let fills = [
            fill("a#1", OrderSide::Buy, 0.05, 100.0, 0.0),
            fill("b#1", OrderSide::Sell, 0.06, 100.0, 0.0),
        ];
        assert!(CostBasis::from_fills(&fills).unwrap_err().contains("b#1"));
    }

    #[test]
    fn store_roundtrips_atomically_and_rejects_bad_versions() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::new(dir.path().join("nested/state.json"));
        assert_eq!(store.load().unwrap(), None);
        let mut s = DurableState::new("SPY", 739_000);
        s.account_id = Some("ACC".into());
        s.day_start_realized_usd = -0.25;
        s.last_order_secs = Some(17);
        s.fills.push(fill("a#1", OrderSide::Buy, 0.05, 100.0, 0.0));
        store.save(&s).unwrap();
        assert_eq!(store.load().unwrap(), Some(s.clone()));
        assert!(!dir.path().join("nested/state.json.tmp").exists());
        s.llm_spend_today_usd = 0.5;
        store.save(&s).unwrap();
        assert_eq!(store.load().unwrap().unwrap().llm_spend_today_usd, 0.5);
        s.version = 99;
        std::fs::write(store.path(), serde_json::to_string(&s).unwrap()).unwrap();
        assert!(store.load().unwrap_err().to_string().contains("version"));
        std::fs::write(store.path(), "{not json").unwrap();
        assert!(store.load().is_err());
    }
}
