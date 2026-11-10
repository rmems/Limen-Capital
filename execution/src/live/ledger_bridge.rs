//! Bridge from adopted venue fills into `metabolic-ledger`'s audit log.
//!
//! Sizing authority stays single: the decider proposes, `risk::check`
//! decides, and the ledger only **records**. Fills are applied to a
//! [`GhostWallet`] directly (never through `execute_buy` / `execute_sell`,
//! which size from the ledger's own Kelly), and the wallet's adaptive Kelly
//! fraction is written to each row as an observation that nothing reads back.
//!
//! Every row carries `fill=<fill_id>` in its reason, so a restart can tell
//! which fills already have a row and append only the missing ones.

use crate::live::durable::{CostBasis, FillRecord};
use crate::live::robinhood::types::OrderSide;
use chrono::{DateTime, Utc};
use metabolic_ledger::log::{append_ghost_log, GhostTradeLog};
use metabolic_ledger::GhostWallet;
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

pub struct LedgerBridge {
    path: PathBuf,
    wallet: GhostWallet,
    basis: CostBasis,
    recorded: HashSet<String>,
    steps: u64,
}

impl std::fmt::Debug for LedgerBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LedgerBridge")
            .field("path", &self.path)
            .field("recorded", &self.recorded.len())
            .finish()
    }
}

/// `fill_id` of an audit row written by this bridge, if any.
fn row_fill_id(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let reason = v.get("reason")?.as_str()?;
    reason
        .split(';')
        .find_map(|kv| kv.strip_prefix("fill="))
        .map(str::to_string)
}

impl LedgerBridge {
    /// Open the audit log at `path`, remembering which fills already have rows.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let recorded = match std::fs::read_to_string(&path) {
            Ok(text) => text.lines().filter_map(row_fill_id).collect(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => HashSet::new(),
            Err(e) => return Err(e),
        };
        let mut wallet = GhostWallet::new();
        // The wallet's ATP is not this account's cash; it tracks net cash flow from fills.
        wallet.balance_atp = 0.0;
        Ok(Self {
            path,
            wallet,
            basis: CostBasis::default(),
            recorded,
            steps: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Ledger's adaptive Kelly fraction: observed, never applied to sizing.
    pub fn observed_kelly_fraction(&self) -> f32 {
        self.wallet.kelly_fraction()
    }

    /// Apply `fill` to the ledger wallet; append its audit row unless one exists.
    pub fn record(&mut self, fill: &FillRecord) -> Result<(), String> {
        let realized = self.basis.apply(fill)?;
        self.steps += 1;
        let notional = fill.qty * fill.price;
        let asset = fill.symbol.as_str();
        match fill.side {
            OrderSide::Buy => {
                *self.wallet.balances.entry(asset.to_string()).or_insert(0.0) += fill.qty as f32;
                if let Some(avg) = self.basis.avg_cost_usd {
                    self.wallet
                        .entry_prices
                        .insert(asset.to_string(), avg as f32);
                }
                self.wallet.balance_atp -= (notional + fill.fee_usd) as f32;
            }
            OrderSide::Sell => {
                if self.basis.position_qty > 0.0 {
                    self.wallet
                        .balances
                        .insert(asset.to_string(), self.basis.position_qty as f32);
                } else {
                    self.wallet.balances.remove(asset);
                    self.wallet.entry_prices.remove(asset);
                }
                self.wallet.balance_atp += (notional - fill.fee_usd) as f32;
                self.wallet.cumulative_pnl += realized as f32;
                *self
                    .wallet
                    .realized_pnls
                    .entry(asset.to_string())
                    .or_insert(0.0) += realized as f32;
                self.wallet.record_pnl_and_update_kelly(realized as f32);
            }
        }
        self.wallet.trade_count += 1;
        if self.recorded.contains(&fill.fill_id) {
            return Ok(());
        }
        let row = GhostTradeLog {
            timestamp: DateTime::<Utc>::from_timestamp(fill.ts_secs, 0)
                .unwrap_or_default()
                .to_rfc3339(),
            step: self.steps,
            action: fill.side.as_str().to_string(),
            asset: asset.to_string(),
            price_usd: fill.price as f32,
            quantity: fill.qty as f32,
            trade_value_usdt: notional as f32,
            realized_pnl_usdt: realized as f32,
            balance_atp: self.wallet.balance_atp,
            cumulative_pnl: self.wallet.cumulative_pnl,
            reason: format!(
                "fill={};cid={};fee_usd={:.8};realized_usd={:.8};observed_kelly={:.4};source=venue",
                fill.fill_id,
                fill.client_order_id,
                fill.fee_usd,
                realized,
                self.observed_kelly_fraction(),
            ),
        };
        append_ghost_log(&row, &self.path.to_string_lossy());
        self.recorded.insert(fill.fill_id.clone());
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
            ts_secs: 1_700_000_000,
        }
    }

    fn rows(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn replay_writes_each_fill_once_and_never_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let fills = [
            fill("a#1", OrderSide::Buy, 0.05, 100.0, 0.005),
            fill("b#1", OrderSide::Sell, 0.05, 90.0, 0.0045),
        ];
        let mut bridge = LedgerBridge::open(&path).unwrap();
        bridge.record(&fills[0]).unwrap();
        // Restart between the two fills: the replay must not duplicate row 1.
        let mut bridge = LedgerBridge::open(&path).unwrap();
        for f in &fills {
            bridge.record(f).unwrap();
        }
        let rows = rows(&path);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["action"], "buy");
        assert_eq!(rows[0]["realized_pnl_usdt"].as_f64().unwrap(), 0.0);
        // 0.05 × 90 − 0.05 × 100.1 − 0.0045 = −0.5095
        let realized = rows[1]["realized_pnl_usdt"].as_f64().unwrap();
        assert!((realized - -0.5095).abs() < 1e-5, "{realized}");
        assert!(rows[1]["reason"]
            .as_str()
            .unwrap()
            .contains("realized_usd=-0.50950000"));
        assert!((bridge.observed_kelly_fraction() - 0.08).abs() < 1e-6);
        assert!(bridge.wallet.balances.is_empty());
    }
}
