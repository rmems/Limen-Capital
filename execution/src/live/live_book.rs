//! Durable, venue-reconciled book for live mode.
//!
//! The venue (Alpaca account) is the book: cash and the holding come from
//! it. Locally, [`DurableState`] remembers submitted `client_order_id`s, the
//! fill log, and the daily-loss baseline, so a restart:
//!
//! * re-reads cash and the holding, and refuses to start if the fill log
//!   cannot explain the holding (the venue gives no cost basis we trust);
//! * looks every persisted `client_order_id` up at the venue and adopts it,
//!   including one whose POST response was lost — it never mints a second
//!   id for the same intent and never re-sends a POST;
//! * refuses to start beside open orders it did not place;
//! * keeps today's daily-loss baseline (only a new UTC day resets it).
//!
//! Adopted fills are mirrored into the `metabolic-ledger` audit log.

use crate::live::durable::{
    CostBasis, DurableState, FillRecord, IntentStatus, StateStore, TrackedOrder, QTY_EPS,
};
use crate::live::ledger_bridge::LedgerBridge;
use crate::live::r#loop::utc_day;
use crate::live::risk::ApprovedOrder;
use crate::live::venue::{Venue, VenueError, VenueOrder};
use std::fmt;
use std::io;
use std::path::PathBuf;
use tracing::{info, warn};

/// How long an intent whose POST outcome is unknown may stay unfound at the
/// venue before it is treated as never having arrived.
pub const DEFAULT_UNCONFIRMED_GRACE_SECS: i64 = 120;

#[derive(Debug, Clone, PartialEq)]
pub struct LiveBookConfig {
    pub symbol: String,
    pub state_path: PathBuf,
    pub ledger_path: PathBuf,
    /// Fee charged per fill as a fraction of notional (Alpaca equities: 0).
    pub fee_ratio: f64,
    pub unconfirmed_grace_secs: i64,
}

#[derive(Debug)]
pub enum BookError {
    Venue(VenueError),
    Io(io::Error),
    /// The book will not start or continue: state and venue disagree.
    Refused(String),
}

impl fmt::Display for BookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BookError::Venue(e) => write!(f, "{e}"),
            BookError::Io(e) => write!(f, "durable state: {e}"),
            BookError::Refused(m) => write!(f, "refusing: {m}"),
        }
    }
}

impl std::error::Error for BookError {}

impl From<VenueError> for BookError {
    fn from(e: VenueError) -> Self {
        BookError::Venue(e)
    }
}

impl From<io::Error> for BookError {
    fn from(e: io::Error) -> Self {
        BookError::Io(e)
    }
}

/// What happened to one `place` call.
#[derive(Debug, Clone, PartialEq)]
pub enum PlaceOutcome {
    /// The POST answered with the order.
    Acknowledged(VenueOrder),
    /// The POST answer was lost or refused, but the lookup by
    /// `client_order_id` found the order: adopted, not resent.
    Adopted(VenueOrder),
    /// Outcome unknown and the lookup did not find it yet. The intent stays
    /// open (blocking new orders) and is looked up again on every poll.
    Unconfirmed,
    /// The venue refused the order and has no order under this id.
    Rejected(VenueError),
}

pub struct LiveBook {
    venue: Box<dyn Venue>,
    cfg: LiveBookConfig,
    store: StateStore,
    state: DurableState,
    basis: CostBasis,
    ledger: LedgerBridge,
    ledger_cursor: usize,
    cash_usd: f64,
    buying_power_usd: f64,
}

impl fmt::Debug for LiveBook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveBook")
            .field("venue", &self.venue.label())
            .field("state", &self.state)
            .field("basis", &self.basis)
            .field("cash_usd", &self.cash_usd)
            .finish()
    }
}

fn same_qty(a: f64, b: f64) -> bool {
    (a - b).abs() <= QTY_EPS.max(1e-9 * a.abs().max(b.abs()))
}

impl LiveBook {
    /// Reconcile durable state against the venue. See the module docs.
    pub async fn open(
        venue: Box<dyn Venue>,
        cfg: LiveBookConfig,
        now: i64,
    ) -> Result<Self, BookError> {
        let store = StateStore::new(&cfg.state_path);
        let state = store
            .load()?
            .unwrap_or_else(|| DurableState::new(&cfg.symbol, utc_day(now)));
        if state.symbol != cfg.symbol {
            return Err(BookError::Refused(format!(
                "{} belongs to {}, not {}",
                store.path().display(),
                state.symbol,
                cfg.symbol
            )));
        }
        let basis = CostBasis::from_fills(&state.fills).map_err(BookError::Refused)?;
        let ledger = LedgerBridge::open(&cfg.ledger_path)?;
        let mut book = Self {
            venue,
            cfg,
            store,
            state,
            basis,
            ledger,
            ledger_cursor: 0,
            cash_usd: 0.0,
            buying_power_usd: 0.0,
        };
        book.bind_account().await?;
        book.roll_day(now)?;
        book.refresh_tracked(now).await?;
        book.refuse_foreign_orders().await?;
        book.sync_account().await?;
        book.persist()?;
        info!(
            "live book: account reconciled; {} open order(s), position {} {}, day baseline {:+.4}",
            book.state.open_orders.len(),
            book.basis.position_qty,
            book.cfg.symbol,
            book.state.day_start_realized_usd,
        );
        Ok(book)
    }

    async fn bind_account(&mut self) -> Result<(), BookError> {
        let account = self.venue.account().await?;
        match &self.state.account_id {
            Some(id) if *id != account.id => {
                return Err(BookError::Refused(format!(
                    "{} belongs to account {id}, venue reports {}",
                    self.store.path().display(),
                    account.id
                )))
            }
            Some(_) => {}
            None => self.state.account_id = Some(account.id.clone()),
        }
        self.cash_usd = account.cash;
        self.buying_power_usd = account.buying_power;
        Ok(())
    }

    /// Start a new UTC day if `now` is past the persisted one. Returns `true`
    /// when the daily counters were reset.
    pub fn roll_day(&mut self, now: i64) -> Result<bool, BookError> {
        let today = utc_day(now);
        if today == self.state.utc_day {
            return Ok(false);
        }
        self.state.utc_day = today;
        self.state.day_start_realized_usd = self.basis.realized_pnl_usd;
        self.state.llm_spend_today_usd = 0.0;
        self.state.last_order_secs = None;
        self.persist()?;
        info!("live book: new UTC day, daily loss baseline reset");
        Ok(true)
    }

    fn persist(&mut self) -> Result<(), BookError> {
        self.store.save(&self.state)?;
        // State first, then the audit log: a crash in between leaves a fill
        // without a row, which the next open appends (rows are keyed by fill id).
        while let Some(fill) = self.state.fills.get(self.ledger_cursor) {
            self.ledger.record(fill).map_err(BookError::Refused)?;
            self.ledger_cursor += 1;
        }
        Ok(())
    }

    /// Account for venue fills not yet in the log (partial fills sum).
    fn absorb(&mut self, order: &VenueOrder, cid: &str, now: i64) -> Result<(), BookError> {
        let (logged_qty, logged_notional, count) = self.state.filled_qty(cid);
        let delta = order.filled_qty - logged_qty;
        if delta < -QTY_EPS {
            return Err(BookError::Refused(format!(
                "venue reports {} filled on {cid}, the fill log has {logged_qty}",
                order.filled_qty
            )));
        }
        if delta <= QTY_EPS {
            return Ok(());
        }
        let price = match order.filled_avg_price {
            Some(avg) => (avg * order.filled_qty - logged_notional) / delta,
            None => order
                .limit_price
                .ok_or_else(|| BookError::Refused(format!("fill on {cid} without a price")))?,
        };
        let fill = FillRecord {
            fill_id: format!("{cid}#{}", count + 1),
            client_order_id: cid.to_string(),
            symbol: order.symbol.clone(),
            side: order.side,
            qty: delta,
            price,
            fee_usd: delta * price * self.cfg.fee_ratio,
            ts_secs: now,
        };
        let realized = self.basis.apply(&fill).map_err(BookError::Refused)?;
        info!(
            "FILL {} {} {:.8} @ {:.4} fee={:.4} realized={:+.4}",
            fill.fill_id, fill.side, fill.qty, fill.price, fill.fee_usd, realized
        );
        self.state.fills.push(fill);
        Ok(())
    }

    /// Take the venue's view of one tracked order; returns whether it is
    /// still open afterwards.
    fn adopt(&mut self, idx: usize, order: &VenueOrder, now: i64) -> Result<bool, BookError> {
        let cid = self.state.open_orders[idx].client_order_id.clone();
        if order.side != self.state.open_orders[idx].side || order.symbol != self.cfg.symbol {
            return Err(BookError::Refused(format!(
                "venue order {} under {cid} does not match the persisted intent",
                order.id
            )));
        }
        let tracked = &mut self.state.open_orders[idx];
        tracked.venue_order_id = Some(order.id.clone());
        tracked.status = IntentStatus::Acknowledged;
        self.absorb(order, &cid, now)?;
        Ok(!order.status.is_terminal())
    }

    /// Look every tracked intent up by `client_order_id`.
    async fn refresh_tracked(&mut self, now: i64) -> Result<(), BookError> {
        let mut idx = 0;
        while idx < self.state.open_orders.len() {
            let tracked = self.state.open_orders[idx].clone();
            let keep = match self
                .venue
                .order_by_client_id(&tracked.client_order_id)
                .await?
            {
                Some(order) => self.adopt(idx, &order, now)?,
                None if tracked.status == IntentStatus::Acknowledged => {
                    return Err(BookError::Refused(format!(
                        "acknowledged order {} is missing at the venue",
                        tracked.client_order_id
                    )))
                }
                None if now - tracked.submitted_secs >= self.cfg.unconfirmed_grace_secs => {
                    warn!(
                        "intent {} never reached the venue; dropping it (not resubmitting)",
                        tracked.client_order_id
                    );
                    false
                }
                None => {
                    self.state.open_orders[idx].status = IntentStatus::Unconfirmed;
                    true
                }
            };
            if keep {
                idx += 1;
            } else {
                self.state.open_orders.remove(idx);
            }
        }
        Ok(())
    }

    async fn refuse_foreign_orders(&self) -> Result<(), BookError> {
        for order in self.venue.open_orders(&self.cfg.symbol).await? {
            let ours = order.client_order_id.as_deref().is_some_and(|cid| {
                self.state
                    .open_orders
                    .iter()
                    .any(|t| t.client_order_id == cid)
            });
            if !ours {
                return Err(BookError::Refused(format!(
                    "open venue order {} (client_order_id {:?}) was not placed by this book",
                    order.id, order.client_order_id
                )));
            }
        }
        Ok(())
    }

    /// Re-read cash and the holding; the fill log must explain the holding.
    async fn sync_account(&mut self) -> Result<(), BookError> {
        let account = self.venue.account().await?;
        self.cash_usd = account.cash;
        self.buying_power_usd = account.buying_power;
        let held = self
            .venue
            .position(&self.cfg.symbol)
            .await?
            .map(|p| p.qty)
            .unwrap_or(0.0);
        if !same_qty(held, self.basis.position_qty) {
            return Err(BookError::Refused(format!(
                "venue holds {held} {} but the fill log explains {}",
                self.cfg.symbol, self.basis.position_qty
            )));
        }
        Ok(())
    }

    /// Submit a risk-approved order exactly once. The intent is persisted
    /// before the POST so a crash mid-request is adopted on restart.
    pub async fn place(
        &mut self,
        order: &ApprovedOrder,
        now: i64,
    ) -> Result<PlaceOutcome, BookError> {
        if order.symbol != self.cfg.symbol {
            return Err(BookError::Refused(format!(
                "order for {} on a {} book",
                order.symbol, self.cfg.symbol
            )));
        }
        if self
            .state
            .open_orders
            .iter()
            .any(|t| t.client_order_id == order.client_order_id)
            || self
                .state
                .fills
                .iter()
                .any(|f| f.client_order_id == order.client_order_id)
        {
            return Err(BookError::Refused(format!(
                "client_order_id {} was already submitted",
                order.client_order_id
            )));
        }
        self.state.open_orders.push(TrackedOrder {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: None,
            symbol: order.symbol.clone(),
            side: order.side,
            asset_qty: order.asset_qty,
            limit_price: order.limit_price,
            submitted_secs: now,
            status: IntentStatus::Submitting,
        });
        self.state.last_order_secs = Some(now);
        self.persist()?;
        let idx = self.state.open_orders.len() - 1;
        let outcome = match self.venue.place_limit(order).await {
            Ok(venue_order) => {
                if !self.adopt(idx, &venue_order, now)? {
                    self.state.open_orders.remove(idx);
                }
                PlaceOutcome::Acknowledged(venue_order)
            }
            Err(post_err) => {
                warn!(
                    "POST {} failed: {post_err}; looking it up",
                    order.client_order_id
                );
                self.resolve_failed_post(idx, post_err, now).await?
            }
        };
        self.persist()?;
        Ok(outcome)
    }

    async fn resolve_failed_post(
        &mut self,
        idx: usize,
        post_err: VenueError,
        now: i64,
    ) -> Result<PlaceOutcome, BookError> {
        let cid = self.state.open_orders[idx].client_order_id.clone();
        match self.venue.order_by_client_id(&cid).await {
            Ok(Some(venue_order)) => {
                if !self.adopt(idx, &venue_order, now)? {
                    self.state.open_orders.remove(idx);
                }
                Ok(PlaceOutcome::Adopted(venue_order))
            }
            Ok(None) if matches!(post_err, VenueError::Rejected { .. }) => {
                self.state.open_orders.remove(idx);
                Ok(PlaceOutcome::Rejected(post_err))
            }
            Ok(None) | Err(_) => {
                self.state.open_orders[idx].status = IntentStatus::Unconfirmed;
                Ok(PlaceOutcome::Unconfirmed)
            }
        }
    }

    /// Refresh tracked orders, absorb fills, re-read cash and the holding.
    /// Returns the fills logged by this call.
    pub async fn poll(&mut self, now: i64) -> Result<Vec<FillRecord>, BookError> {
        let before = self.state.fills.len();
        let refreshed = self.refresh_tracked(now).await;
        // Persist whatever was absorbed even if a later lookup failed.
        self.persist()?;
        refreshed?;
        self.sync_account().await?;
        Ok(self.state.fills[before..].to_vec())
    }

    /// Cancel every resting order (halt, Ctrl-C, exit), then poll so fills
    /// that raced the cancel are accounted for. Returns DELETEs sent.
    pub async fn cancel_all(&mut self, now: i64) -> Result<usize, BookError> {
        let mut sent = 0;
        let mut first_err = None;
        for tracked in self.state.open_orders.clone() {
            let id = match tracked.venue_order_id {
                Some(id) => Some(id),
                None => match self
                    .venue
                    .order_by_client_id(&tracked.client_order_id)
                    .await
                {
                    Ok(found) => found.filter(|o| !o.status.is_terminal()).map(|o| o.id),
                    Err(e) => {
                        first_err.get_or_insert(BookError::Venue(e));
                        None
                    }
                },
            };
            let Some(id) = id else { continue };
            sent += 1;
            match self.venue.cancel(&id).await {
                Ok(()) => info!("CANCEL {id} ({})", tracked.client_order_id),
                // Already terminal (filled / expired) — the poll below accounts for it.
                Err(VenueError::Rejected { status: 422, .. }) => {}
                Err(e) => {
                    warn!("cancel {id} failed: {e}");
                    first_err.get_or_insert(BookError::Venue(e));
                }
            }
        }
        self.poll(now).await?;
        match first_err {
            Some(e) => Err(e),
            None => Ok(sent),
        }
    }

    /// Persist the decider's LLM spend for today.
    pub fn record_llm_spend(&mut self, spend_today_usd: f64) -> Result<(), BookError> {
        self.state.llm_spend_today_usd = spend_today_usd;
        self.persist()
    }

    pub fn symbol(&self) -> &str {
        &self.cfg.symbol
    }
    pub fn cash_usd(&self) -> f64 {
        self.cash_usd
    }
    pub fn buying_power_usd(&self) -> f64 {
        self.buying_power_usd
    }
    pub fn position_qty(&self) -> f64 {
        self.basis.position_qty
    }
    pub fn avg_cost_usd(&self) -> Option<f64> {
        self.basis.avg_cost_usd
    }
    pub fn realized_pnl_usd(&self) -> f64 {
        self.basis.realized_pnl_usd
    }
    pub fn fees_paid_usd(&self) -> f64 {
        self.basis.fees_paid_usd
    }
    pub fn day_start_realized_usd(&self) -> f64 {
        self.state.day_start_realized_usd
    }
    /// Realized PnL since the persisted UTC-day baseline.
    pub fn daily_pnl_usd(&self) -> f64 {
        self.basis.realized_pnl_usd - self.state.day_start_realized_usd
    }
    pub fn llm_spend_today_usd(&self) -> f64 {
        self.state.llm_spend_today_usd
    }
    pub fn last_order_secs(&self) -> Option<i64> {
        self.state.last_order_secs
    }
    pub fn open_orders(&self) -> &[TrackedOrder] {
        &self.state.open_orders
    }
    pub fn fills(&self) -> &[FillRecord] {
        &self.state.fills
    }
    /// Notional of every tracked, unfinished order (counts toward the position cap).
    pub fn resting_notional_usd(&self) -> f64 {
        self.state
            .open_orders
            .iter()
            .map(|t| {
                let (filled, _, _) = self.state.filled_qty(&t.client_order_id);
                (t.asset_qty - filled).max(0.0) * t.limit_price
            })
            .sum()
    }
    /// Ledger's adaptive Kelly fraction (observed only).
    pub fn observed_kelly_fraction(&self) -> f32 {
        self.ledger.observed_kelly_fraction()
    }
}

#[cfg(test)]
mod tests;
