//! Paper execution: a tiny simulated account + resting-limit-order book.
//!
//! Paper mode exercises exactly the live path (decide → risk → place → poll
//! fills → cancel stale) without any order reaching the venue. Fills are
//! conservative: a resting **buy** at `limit` fills only when a later ask is
//! `≤ limit`; a resting **sell** fills only when a later bid is `≥ limit`.
//! Fills happen at the limit price and pay `fee_ratio` of notional.
//! Unfilled orders are cancelled after `stale_ticks` ticks.

use crate::live::risk::ApprovedOrder;
use crate::live::robinhood::types::{OrderSide, OrderState};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaperOrder {
    pub id: String,
    pub client_order_id: String,
    pub side: OrderSide,
    pub limit_price: f64,
    pub asset_qty: f64,
    pub placed_tick: u64,
    pub state: OrderState,
}

/// Something that happened to a paper order during `on_tick`.
#[derive(Debug, Clone, PartialEq)]
pub enum PaperEvent {
    Filled {
        order: PaperOrder,
        price: f64,
        qty: f64,
        fee_usd: f64,
        /// Realized PnL from this fill (sells only; buys realize nothing).
        realized_usd: f64,
    },
    Cancelled {
        order: PaperOrder,
        age_ticks: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaperBook {
    pub cash_usd: f64,
    pub position_qty: f64,
    /// Average cost per unit of the open position (fees included), if any.
    pub avg_cost_usd: Option<f64>,
    pub realized_pnl_usd: f64,
    pub fees_paid_usd: f64,
    pub fee_ratio: f64,
    pub open_orders: Vec<PaperOrder>,
    pub fills: u64,
    pub cancels: u64,
    next_id: u64,
}

impl PaperBook {
    pub fn new(starting_cash_usd: f64, fee_ratio: f64) -> Self {
        Self {
            cash_usd: starting_cash_usd,
            position_qty: 0.0,
            avg_cost_usd: None,
            realized_pnl_usd: 0.0,
            fees_paid_usd: 0.0,
            fee_ratio,
            open_orders: Vec::new(),
            fills: 0,
            cancels: 0,
            next_id: 1,
        }
    }

    /// Cash not reserved by resting buy orders.
    pub fn free_cash_usd(&self) -> f64 {
        let reserved: f64 = self
            .open_orders
            .iter()
            .filter(|o| o.side == OrderSide::Buy)
            .map(|o| o.limit_price * o.asset_qty * (1.0 + self.fee_ratio))
            .sum();
        (self.cash_usd - reserved).max(0.0)
    }

    /// Position not reserved by resting sell orders.
    pub fn free_position_qty(&self) -> f64 {
        let reserved: f64 = self
            .open_orders
            .iter()
            .filter(|o| o.side == OrderSide::Sell)
            .map(|o| o.asset_qty)
            .sum();
        (self.position_qty - reserved).max(0.0)
    }

    /// Record a resting paper order. Returns its paper id.
    pub fn place(&mut self, approved: &ApprovedOrder, tick: u64) -> PaperOrder {
        let order = PaperOrder {
            id: format!("paper-{}", self.next_id),
            client_order_id: approved.client_order_id.clone(),
            side: approved.side,
            limit_price: approved.limit_price,
            asset_qty: approved.asset_qty,
            placed_tick: tick,
            state: OrderState::Open,
        };
        self.next_id += 1;
        self.open_orders.push(order.clone());
        order
    }

    /// Apply a new quote: fill crossable orders, cancel stale ones.
    pub fn on_tick(&mut self, bid: f64, ask: f64, tick: u64, stale_ticks: u64) -> Vec<PaperEvent> {
        let mut events = Vec::new();
        let mut remaining = Vec::with_capacity(self.open_orders.len());
        let orders = std::mem::take(&mut self.open_orders);
        for mut o in orders {
            let crossable = match o.side {
                OrderSide::Buy => ask <= o.limit_price,
                OrderSide::Sell => bid >= o.limit_price,
            };
            if crossable {
                let price = o.limit_price;
                let qty = o.asset_qty;
                let notional = price * qty;
                let fee = notional * self.fee_ratio;
                self.fees_paid_usd += fee;
                let realized = match o.side {
                    OrderSide::Buy => {
                        let prev_qty = self.position_qty;
                        debug_assert!(
                            prev_qty == 0.0 || self.avg_cost_usd.is_some(),
                            "avg_cost_usd is None but position_qty={prev_qty} > 0"
                        );
                        let prev_cost = self.avg_cost_usd.unwrap_or(0.0) * prev_qty;
                        self.cash_usd -= notional + fee;
                        self.position_qty += qty;
                        self.avg_cost_usd = Some((prev_cost + notional + fee) / self.position_qty);
                        0.0
                    }
                    OrderSide::Sell => {
                        let cost = self.avg_cost_usd.unwrap_or(price);
                        let realized = (price - cost) * qty - fee;
                        self.cash_usd += notional - fee;
                        self.position_qty -= qty;
                        if self.position_qty <= 1e-12 {
                            self.position_qty = 0.0;
                            self.avg_cost_usd = None;
                        }
                        self.realized_pnl_usd += realized;
                        realized
                    }
                };
                self.fills += 1;
                o.state = OrderState::Filled;
                events.push(PaperEvent::Filled {
                    order: o,
                    price,
                    qty,
                    fee_usd: fee,
                    realized_usd: realized,
                });
                continue;
            }
            let age = tick.saturating_sub(o.placed_tick);
            if age >= stale_ticks {
                o.state = OrderState::Canceled;
                self.cancels += 1;
                events.push(PaperEvent::Cancelled {
                    order: o,
                    age_ticks: age,
                });
                continue;
            }
            remaining.push(o);
        }
        self.open_orders = remaining;
        events
    }

    pub fn equity_usd(&self, mid: f64) -> f64 {
        self.cash_usd + self.position_qty * mid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approved(side: OrderSide, limit: f64, qty: f64) -> ApprovedOrder {
        ApprovedOrder {
            client_order_id: "cid".into(),
            symbol: "BTC-USD".into(),
            side,
            limit_price: limit,
            asset_qty: qty,
            notional_usd: limit * qty,
            limit_price_str: format!("{limit:.2}"),
            asset_qty_str: format!("{qty:.8}"),
        }
    }

    #[test]
    fn buy_rests_until_ask_crosses_then_fills_with_fee() {
        let mut b = PaperBook::new(10.0, 0.01);
        let o = b.place(&approved(OrderSide::Buy, 100.0, 0.05), 1);
        assert_eq!(o.id, "paper-1");
        assert_eq!(b.open_orders.len(), 1);
        assert!((b.free_cash_usd() - (10.0 - 5.05)).abs() < 1e-9);
        // Ask above limit: no fill, not stale yet.
        assert!(b.on_tick(99.0, 100.5, 2, 2).is_empty());
        // Ask at limit: fills at limit.
        let ev = b.on_tick(99.0, 100.0, 3, 2);
        assert_eq!(ev.len(), 1);
        match &ev[0] {
            PaperEvent::Filled {
                price,
                qty,
                fee_usd,
                realized_usd,
                ..
            } => {
                assert_eq!(*price, 100.0);
                assert_eq!(*qty, 0.05);
                assert!((fee_usd - 0.05).abs() < 1e-9);
                assert_eq!(*realized_usd, 0.0);
            }
            other => panic!("{other:?}"),
        }
        assert!((b.cash_usd - 4.95).abs() < 1e-9);
        assert_eq!(b.position_qty, 0.05);
        assert!((b.avg_cost_usd.unwrap() - 101.0).abs() < 1e-9); // fee folded into cost
        assert!(b.open_orders.is_empty());
        assert_eq!(b.fills, 1);
    }

    #[test]
    fn stale_order_is_cancelled_after_n_ticks() {
        let mut b = PaperBook::new(10.0, 0.0);
        b.place(&approved(OrderSide::Buy, 100.0, 0.05), 5);
        assert!(b.on_tick(100.5, 101.0, 6, 2).is_empty());
        let ev = b.on_tick(100.5, 101.0, 7, 2);
        assert!(matches!(ev[0], PaperEvent::Cancelled { age_ticks: 2, .. }));
        assert!(b.open_orders.is_empty());
        assert_eq!(b.cancels, 1);
        assert_eq!(b.cash_usd, 10.0);
    }

    #[test]
    fn sell_realizes_pnl_against_average_cost() {
        let mut b = PaperBook::new(10.0, 0.0);
        b.place(&approved(OrderSide::Buy, 100.0, 0.05), 1);
        b.on_tick(99.0, 100.0, 2, 2);
        b.place(&approved(OrderSide::Sell, 102.0, 0.05), 2);
        assert_eq!(b.free_position_qty(), 0.0);
        // Bid below limit: rests.
        assert!(b.on_tick(101.0, 101.5, 3, 5).is_empty());
        let ev = b.on_tick(102.0, 102.5, 4, 5);
        match &ev[0] {
            PaperEvent::Filled { realized_usd, .. } => assert!((realized_usd - 0.1).abs() < 1e-9),
            other => panic!("{other:?}"),
        }
        assert!((b.realized_pnl_usd - 0.1).abs() < 1e-9);
        assert_eq!(b.position_qty, 0.0);
        assert_eq!(b.avg_cost_usd, None);
        assert!((b.cash_usd - 10.1).abs() < 1e-9);
        assert!((b.equity_usd(500.0) - 10.1).abs() < 1e-9);
    }
}
