//! Compact snapshot of market + account state handed to the decider and the
//! risk module each tick.

use serde::{Deserialize, Serialize};

/// How many recent mids are kept for the decider.
pub const RECENT_MIDS_LEN: usize = 12;

/// Available account balances, excluding assets reserved by resting orders.
#[derive(Debug, Clone, Copy)]
pub struct AccountState {
    pub position_qty: f64,
    pub avg_cost_usd: Option<f64>,
    pub cash_usd: f64,
    pub realized_pnl_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketState {
    /// Unix seconds of the quote.
    pub ts: i64,
    /// Tick counter within this run (1-based).
    pub tick: u64,
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub spread_bps: f64,
    /// Asset units held (never negative: no shorts).
    pub position_qty: f64,
    /// `position_qty × mid`.
    pub position_usd: f64,
    /// Average cost of the open position, if any.
    pub avg_cost_usd: Option<f64>,
    /// Cash available to buy.
    pub cash_usd: f64,
    pub realized_pnl_usd: f64,
    pub unrealized_pnl_usd: f64,
    /// Last `RECENT_MIDS_LEN` mids, oldest first, including the current one.
    pub recent_mids: Vec<f64>,
}

impl MarketState {
    /// Build from a quote and the book. `recent_mids` should already include
    /// prior mids; the current mid is appended and the window trimmed.
    pub fn new(
        ts: i64,
        tick: u64,
        symbol: &str,
        bid: f64,
        ask: f64,
        account: AccountState,
        mut recent_mids: Vec<f64>,
    ) -> Self {
        let AccountState {
            position_qty,
            avg_cost_usd,
            cash_usd,
            realized_pnl_usd,
        } = account;
        let mid = (bid + ask) / 2.0;
        let spread_bps = if mid > 0.0 {
            (ask - bid) / mid * 10_000.0
        } else {
            0.0
        };
        recent_mids.push(mid);
        if recent_mids.len() > RECENT_MIDS_LEN {
            let drop = recent_mids.len() - RECENT_MIDS_LEN;
            recent_mids.drain(..drop);
        }
        let position_usd = position_qty * mid;
        let unrealized_pnl_usd = match avg_cost_usd {
            Some(c) if position_qty > 0.0 => (mid - c) * position_qty,
            _ => 0.0,
        };
        Self {
            ts,
            tick,
            symbol: symbol.to_string(),
            bid,
            ask,
            mid,
            spread_bps,
            position_qty,
            position_usd,
            avg_cost_usd,
            cash_usd,
            realized_pnl_usd,
            unrealized_pnl_usd,
            recent_mids,
        }
    }

    /// Cash + marked position.
    pub fn equity_usd(&self) -> f64 {
        self.cash_usd + self.position_usd
    }

    /// Quote sanity: finite, positive, not crossed.
    pub fn quote_is_sane(&self) -> bool {
        self.bid.is_finite() && self.ask.is_finite() && self.bid > 0.0 && self.ask >= self.bid
    }

    /// Return of the current mid vs. the oldest mid in the window, in bps.
    pub fn window_return_bps(&self) -> f64 {
        match (self.recent_mids.first(), self.recent_mids.last()) {
            (Some(first), Some(last)) if *first > 0.0 => (last / first - 1.0) * 10_000.0,
            _ => 0.0,
        }
    }

    /// Mean of the window (the current mid included).
    pub fn window_mean(&self) -> f64 {
        if self.recent_mids.is_empty() {
            return self.mid;
        }
        self.recent_mids.iter().sum::<f64>() / self.recent_mids.len() as f64
    }

    /// Compact JSON for the decider prompt (rounded to keep tokens down).
    pub fn to_compact_json(&self) -> String {
        let round = |x: f64, d: i32| {
            let p = 10f64.powi(d);
            (x * p).round() / p
        };
        let v = serde_json::json!({
            "ts": self.ts,
            "tick": self.tick,
            "symbol": self.symbol,
            "bid": round(self.bid, 2),
            "ask": round(self.ask, 2),
            "mid": round(self.mid, 2),
            "spread_bps": round(self.spread_bps, 2),
            "position_qty": self.position_qty,
            "position_usd": round(self.position_usd, 4),
            "avg_cost_usd": self.avg_cost_usd.map(|c| round(c, 2)),
            "cash_usd": round(self.cash_usd, 4),
            "realized_pnl_usd": round(self.realized_pnl_usd, 4),
            "unrealized_pnl_usd": round(self.unrealized_pnl_usd, 4),
            "recent_mids": self.recent_mids.iter().map(|m| round(*m, 2)).collect::<Vec<_>>(),
        });
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_mid_spread_position_and_unrealized() {
        let s = MarketState::new(
            1_700_000_000,
            3,
            "BTC-USD",
            99_990.0,
            100_010.0,
            AccountState {
                position_qty: 0.00005,
                avg_cost_usd: Some(90_000.0),
                cash_usd: 5.0,
                realized_pnl_usd: 0.1,
            },
            vec![99_000.0, 99_500.0],
        );
        assert_eq!(s.mid, 100_000.0);
        assert!((s.spread_bps - 2.0).abs() < 1e-9);
        assert!((s.position_usd - 5.0).abs() < 1e-9);
        assert!((s.unrealized_pnl_usd - 0.5).abs() < 1e-9);
        assert!((s.equity_usd() - 10.0).abs() < 1e-9);
        assert_eq!(s.recent_mids, vec![99_000.0, 99_500.0, 100_000.0]);
        assert!(s.window_return_bps() > 100.0);
        assert!(s.quote_is_sane());
    }

    #[test]
    fn window_is_trimmed_to_recent_len() {
        let prior: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        let account = AccountState {
            position_qty: 0.0,
            avg_cost_usd: None,
            cash_usd: 10.0,
            realized_pnl_usd: 0.0,
        };
        let s = MarketState::new(0, 1, "BTC-USD", 120.0, 120.0, account, prior);
        assert_eq!(s.recent_mids.len(), RECENT_MIDS_LEN);
        assert_eq!(*s.recent_mids.last().unwrap(), 120.0);
        assert_eq!(s.unrealized_pnl_usd, 0.0);
    }

    #[test]
    fn compact_json_is_valid_and_small() {
        let account = AccountState {
            position_qty: 0.0,
            avg_cost_usd: None,
            cash_usd: 10.0,
            realized_pnl_usd: 0.0,
        };
        let s = MarketState::new(1, 1, "BTC-USD", 1.0, 1.1, account, vec![]);
        let j = s.to_compact_json();
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["symbol"], "BTC-USD");
        assert!(v["avg_cost_usd"].is_null());
        assert!(j.len() < 400);
    }
}
