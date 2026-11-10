//! In-process mock of the Alpaca account for durable-book tests.
//!
//! Clones share one account, so a test can drop a book ("kill the process"),
//! open a new one on a clone ("restart"), and inspect what the venue saw.
//! Duplicate `client_order_id`s are refused with 422, as Alpaca does.

use super::*;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

/// A scripted failure for the next matching call.
#[derive(Debug, Clone, PartialEq)]
pub enum PostFault {
    /// The venue creates the order but the response never arrives.
    LoseResponse,
    /// The request never reaches the venue.
    DropRequest,
    /// The venue refuses the order.
    Reject { status: u16, body: String },
}

#[derive(Debug, Default)]
struct MockState {
    account_id: String,
    cash: f64,
    fee_ratio: f64,
    positions: HashMap<String, f64>,
    orders: Vec<VenueOrder>,
    next_id: u64,
    posts: usize,
    deletes: usize,
    post_faults: VecDeque<PostFault>,
    lookup_faults: usize,
}

#[derive(Debug, Clone)]
pub struct MockVenue {
    inner: Arc<Mutex<MockState>>,
}

impl MockVenue {
    /// `fee_ratio` is charged to cash on each fill (commission-free = 0).
    pub fn new(account_id: &str, cash: f64, fee_ratio: f64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockState {
                account_id: account_id.to_string(),
                cash,
                fee_ratio,
                next_id: 1,
                ..Default::default()
            })),
        }
    }

    fn state(&self) -> MutexGuard<'_, MockState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push_post_fault(&self, fault: PostFault) {
        self.state().post_faults.push_back(fault);
    }

    /// The next `n` client-order-id lookups fail with a transport error.
    pub fn fail_lookups(&self, n: usize) {
        self.state().lookup_faults = n;
    }

    /// Holding created outside this process (e.g. a manual trade).
    pub fn set_position(&self, symbol: &str, qty: f64) {
        self.state().positions.insert(symbol.to_string(), qty);
    }

    /// Fill `qty` more of the order with `client_order_id` at `price`.
    pub fn fill(&self, client_order_id: &str, qty: f64, price: f64) {
        let mut s = self.state();
        let fee_ratio = s.fee_ratio;
        let o = s
            .orders
            .iter_mut()
            .find(|o| o.client_order_id.as_deref() == Some(client_order_id))
            .expect("mock: fill of unknown order");
        assert!(!o.status.is_terminal(), "mock: fill of a terminal order");
        let prev_notional = o.filled_avg_price.unwrap_or(0.0) * o.filled_qty;
        o.filled_qty += qty;
        o.filled_avg_price = Some((prev_notional + qty * price) / o.filled_qty);
        o.status = if o.filled_qty + 1e-12 >= o.qty {
            VenueOrderStatus::Filled
        } else {
            VenueOrderStatus::PartiallyFilled
        };
        let (side, symbol) = (o.side, o.symbol.clone());
        let notional = qty * price;
        let fee = notional * fee_ratio;
        let pos = s.positions.entry(symbol).or_insert(0.0);
        match side {
            OrderSide::Buy => {
                *pos += qty;
                s.cash -= notional + fee;
            }
            OrderSide::Sell => {
                *pos -= qty;
                s.cash += notional - fee;
            }
        }
    }

    /// Force a status (e.g. `Expired` at the close).
    pub fn set_status(&self, client_order_id: &str, status: VenueOrderStatus) {
        let mut s = self.state();
        if let Some(o) = s
            .orders
            .iter_mut()
            .find(|o| o.client_order_id.as_deref() == Some(client_order_id))
        {
            o.status = status;
        }
    }

    /// Order placed by someone else on the same account.
    pub fn insert_foreign_order(&self, order: VenueOrder) {
        self.state().orders.push(order);
    }

    pub fn orders(&self) -> Vec<VenueOrder> {
        self.state().orders.clone()
    }

    pub fn post_count(&self) -> usize {
        self.state().posts
    }

    pub fn delete_count(&self) -> usize {
        self.state().deletes
    }

    pub fn cash(&self) -> f64 {
        self.state().cash
    }

    fn do_place(&self, order: &ApprovedOrder) -> Result<VenueOrder, VenueError> {
        let mut s = self.state();
        s.posts += 1;
        let fault = s.post_faults.pop_front();
        match &fault {
            Some(PostFault::DropRequest) => {
                return Err(VenueError::Transport("connection reset".into()))
            }
            Some(PostFault::Reject { status, body }) => {
                return Err(VenueError::Rejected {
                    status: *status,
                    body: body.clone(),
                })
            }
            _ => {}
        }
        if s.orders
            .iter()
            .any(|o| o.client_order_id.as_deref() == Some(order.client_order_id.as_str()))
        {
            return Err(VenueError::Rejected {
                status: 422,
                body: "client_order_id must be unique".into(),
            });
        }
        let venue_order = VenueOrder {
            id: format!("mock-{}", s.next_id),
            client_order_id: Some(order.client_order_id.clone()),
            symbol: order.symbol.clone(),
            side: order.side,
            qty: order.asset_qty,
            limit_price: Some(order.limit_price),
            status: VenueOrderStatus::New,
            filled_qty: 0.0,
            filled_avg_price: None,
        };
        s.next_id += 1;
        s.orders.push(venue_order.clone());
        if fault == Some(PostFault::LoseResponse) {
            return Err(VenueError::Transport("response lost".into()));
        }
        Ok(venue_order)
    }

    fn do_cancel(&self, order_id: &str) -> Result<(), VenueError> {
        let mut s = self.state();
        s.deletes += 1;
        let Some(o) = s.orders.iter_mut().find(|o| o.id == order_id) else {
            return Err(VenueError::Rejected {
                status: 404,
                body: "order not found".into(),
            });
        };
        if o.status.is_terminal() {
            return Err(VenueError::Rejected {
                status: 422,
                body: format!("order is already {:?}", o.status),
            });
        }
        o.status = VenueOrderStatus::Canceled;
        Ok(())
    }

    fn do_lookup(&self, client_order_id: &str) -> Result<Option<VenueOrder>, VenueError> {
        let mut s = self.state();
        if s.lookup_faults > 0 {
            s.lookup_faults -= 1;
            return Err(VenueError::Transport("lookup timed out".into()));
        }
        Ok(s.orders
            .iter()
            .find(|o| o.client_order_id.as_deref() == Some(client_order_id))
            .cloned())
    }
}

impl Venue for MockVenue {
    fn label(&self) -> String {
        "mock-alpaca".into()
    }

    fn account(&self) -> VenueFuture<'_, VenueAccount> {
        let s = self.state();
        let account = VenueAccount {
            id: s.account_id.clone(),
            status: "ACTIVE".into(),
            cash: s.cash,
            buying_power: s.cash,
        };
        Box::pin(async move { Ok(account) })
    }

    fn position<'a>(&'a self, symbol: &'a str) -> VenueFuture<'a, Option<VenuePosition>> {
        let qty = self.state().positions.get(symbol).copied().unwrap_or(0.0);
        let pos = (qty.abs() > 1e-12).then(|| VenuePosition {
            symbol: symbol.to_string(),
            qty,
        });
        Box::pin(async move { Ok(pos) })
    }

    fn open_orders<'a>(&'a self, symbol: &'a str) -> VenueFuture<'a, Vec<VenueOrder>> {
        let open = self
            .state()
            .orders
            .iter()
            .filter(|o| o.symbol == symbol && !o.status.is_terminal())
            .cloned()
            .collect();
        Box::pin(async move { Ok(open) })
    }

    fn order_by_client_id<'a>(
        &'a self,
        client_order_id: &'a str,
    ) -> VenueFuture<'a, Option<VenueOrder>> {
        let r = self.do_lookup(client_order_id);
        Box::pin(async move { r })
    }

    fn place_limit<'a>(&'a self, order: &'a ApprovedOrder) -> VenueFuture<'a, VenueOrder> {
        let r = self.do_place(order);
        Box::pin(async move { r })
    }

    fn cancel<'a>(&'a self, order_id: &'a str) -> VenueFuture<'a, ()> {
        let r = self.do_cancel(order_id);
        Box::pin(async move { r })
    }
}
