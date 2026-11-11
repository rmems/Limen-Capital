//! The tick loop: fetch state → decide → risk → (paper ? simulate : place)
//! → poll fills → journal → sleep.
//!
//! Live order submission is **not** wired yet (arrives in a later step behind
//! `paper = false`); starting with `paper = false` is refused here.

use crate::live::config::{DeciderKind, LiveConfig, MarketSourceConfig, Secrets};
use crate::live::decider::rules::RulesDecider;
use crate::live::decider::Decider;
use crate::live::journal::{Journal, JournalHeader, JournalRow};
use crate::live::market::{MarketSource, RobinhoodMarket, SyntheticMarket};
use crate::live::market_state::MarketState;
use crate::live::paper::{PaperBook, PaperEvent};
use crate::live::risk::{self, PairRules, RiskInputs, RiskVerdict};
use crate::live::robinhood::RobinhoodClient;
use chrono::{DateTime, Datelike, Utc};
use std::fmt;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// One tick, then exit (prints state, decision and verdict).
    pub once: bool,
    /// Stop after this many ticks (0 = unbounded).
    pub max_ticks: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunSummary {
    pub ticks: u64,
    pub orders_placed: u64,
    pub fills: u64,
    pub cancels: u64,
    pub rejects: u64,
    pub realized_pnl_usd: f64,
    pub fees_paid_usd: f64,
    pub final_equity_usd: f64,
    pub halted_reason: Option<String>,
}

#[derive(Debug)]
pub enum LoopError {
    Config(String),
    Venue(Box<crate::live::robinhood::ClientError>),
    Journal(std::io::Error),
}

impl fmt::Display for LoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoopError::Config(m) => write!(f, "{m}"),
            LoopError::Venue(e) => write!(f, "venue: {e}"),
            LoopError::Journal(e) => write!(f, "journal: {e}"),
        }
    }
}

impl std::error::Error for LoopError {}

impl From<crate::live::robinhood::ClientError> for LoopError {
    fn from(e: crate::live::robinhood::ClientError) -> Self {
        LoopError::Venue(Box::new(e))
    }
}

impl From<std::io::Error> for LoopError {
    fn from(e: std::io::Error) -> Self {
        LoopError::Journal(e)
    }
}

fn iso(ts: i64) -> String {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn utc_day(ts: i64) -> i32 {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .map(|d| d.num_days_from_ce())
        .unwrap_or(0)
}

/// Everything the loop needs, built from config + secrets.
pub struct LoopParts {
    pub market: Box<dyn MarketSource>,
    pub pair: PairRules,
    pub decider: Box<dyn Decider>,
    pub book: PaperBook,
    pub journal: Journal,
    pub notifier: Box<dyn crate::live::notify::Notifier>,
    pub account_line: Option<String>,
    /// Realized PnL baseline for today's loss counter. `None` = the book's
    /// realized PnL at start (a restart therefore resets the daily counter;
    /// persisting it across restarts is follow-up work).
    pub day_start_realized: Option<f64>,
}

impl fmt::Debug for LoopParts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopParts")
            .field("market", &self.market.label())
            .field("pair", &self.pair)
            .field("decider", &self.decider.name())
            .field("book", &self.book)
            .field("journal", &self.journal)
            .field("account_line", &self.account_line)
            .field("day_start_realized", &self.day_start_realized)
            .finish()
    }
}

mod report;
mod setup;
pub use setup::build_parts;

/// Run the loop. Returns when `once` / `max_ticks` is reached, the daily-loss
/// cap halts the day, or Ctrl-C is received.
pub async fn run(
    cfg: &LiveConfig,
    secrets: &Secrets,
    opts: &RunOptions,
) -> Result<RunSummary, LoopError> {
    let parts = build_parts(cfg, secrets).await?;
    run_with_parts(cfg, opts, parts).await
}

pub async fn run_with_parts(
    cfg: &LiveConfig,
    opts: &RunOptions,
    parts: LoopParts,
) -> Result<RunSummary, LoopError> {
    let started = Utc::now().timestamp();
    let max_ticks = if opts.once { 1 } else { opts.max_ticks };
    let mut runner = Runner {
        cfg,
        opts,
        day: utc_day(started),
        day_start_realized: parts
            .day_start_realized
            .unwrap_or(parts.book.realized_pnl_usd),
        parts,
        summary: RunSummary::default(),
        recent_mids: Vec::new(),
        last_order_secs: None,
        notifications: Vec::new(),
    };
    report::start(cfg, &mut runner.parts, started, max_ticks)?;
    loop {
        runner.summary.ticks += 1;
        runner.tick().await?;
        if runner.finished(max_ticks).await {
            break;
        }
    }
    report::done(&runner.summary);
    Ok(runner.summary)
}

/// Mutable session state shared by the ordered tick phases.
struct Runner<'a> {
    cfg: &'a LiveConfig,
    opts: &'a RunOptions,
    parts: LoopParts,
    summary: RunSummary,
    recent_mids: Vec<f64>,
    last_order_secs: Option<i64>,
    day: i32,
    day_start_realized: f64,
    notifications: Vec<(&'static str, String)>,
}

impl Runner<'_> {
    async fn tick(&mut self) -> Result<(), LoopError> {
        let now = Utc::now().timestamp();
        self.reset_day(now);
        let quote = match self.parts.market.quote(&self.cfg.symbol).await {
            Ok(quote) => quote,
            Err(e) => {
                warn!("quote failed: {e}");
                self.update_account_summary();
                return Ok(());
            }
        };
        // Invalid quotes must not affect fills, decisions, or the mids window.
        if !quote.is_sane() {
            return report::bad_quote(self.cfg, &mut self.parts, &quote, now, self.summary.ticks);
        }
        self.process_quote(&quote, now).await
    }

    fn reset_day(&mut self, now: i64) {
        if utc_day(now) != self.day {
            self.day = utc_day(now);
            self.day_start_realized = self.parts.book.realized_pnl_usd;
            self.last_order_secs = None;
            info!("new UTC day: daily loss counter reset");
        }
    }

    async fn process_quote(
        &mut self,
        quote: &crate::live::robinhood::types::BestBidAsk,
        now: i64,
    ) -> Result<(), LoopError> {
        // Resting orders react before the decider sees available balances.
        let events = self.parts.book.on_tick(
            quote.bid,
            quote.ask,
            self.summary.ticks,
            self.cfg.risk.stale_order_ticks,
        );
        let state = self.market_state(quote, now);
        self.recent_mids = state.recent_mids.clone();
        let inputs = RiskInputs {
            now_secs: now,
            last_order_secs: self.last_order_secs,
            open_orders: self.parts.book.open_orders.len(),
            daily_pnl_usd: self.parts.book.realized_pnl_usd - self.day_start_realized,
            llm_cost_today_usd: self.parts.decider.llm_cost_today_usd(),
            resting_notional_usd: self.parts.book.resting_notional_usd(),
        };
        let mut row = report::row(&state, &self.parts);
        report::events(&events, &mut row, &mut self.summary);
        for event in &events {
            self.notifications.push(match event {
                PaperEvent::Filled { order, price, qty, fee_usd, realized_usd } => (
                    "fill",
                    format!("{} {} {qty:.8} @ {price:.2}; fee ${fee_usd:.4}; realized ${realized_usd:+.4}",
                        order.id, order.side),
                ),
                PaperEvent::Cancelled { order, age_ticks } => (
                    "cancel",
                    format!("{} {} stale after {age_ticks} ticks", order.id, order.side),
                ),
            });
        }
        if let Some(reason) = risk::pre_tick_check(&self.cfg.risk, &inputs) {
            self.pre_tick_rejection(reason, &mut row);
        } else {
            self.decide(&state, &inputs, &mut row).await?;
        }
        report::tick(&row);
        self.parts.journal.append(&row)?;
        for (event, detail) in std::mem::take(&mut self.notifications) {
            self.notify(event, &detail).await;
        }
        if self.opts.once {
            println!("{}", row.to_markdown());
        }
        self.update_account_summary();
        self.summary.final_equity_usd = self.parts.book.equity_usd(state.mid);
        Ok(())
    }

    fn market_state(
        &self,
        quote: &crate::live::robinhood::types::BestBidAsk,
        now: i64,
    ) -> MarketState {
        let book = &self.parts.book;
        let account = crate::live::market_state::AccountState {
            position_qty: book.free_position_qty(),
            avg_cost_usd: book.avg_cost_usd,
            // Spendable notional excludes both reservations and the fill fee.
            cash_usd: book.free_cash_usd() / (1.0 + book.fee_ratio),
            realized_pnl_usd: book.realized_pnl_usd,
        };
        MarketState::new(
            now,
            self.summary.ticks,
            &self.cfg.symbol,
            quote.bid,
            quote.ask,
            account,
            self.recent_mids.clone(),
        )
    }

    fn pre_tick_rejection(&mut self, reason: risk::RiskReject, row: &mut JournalRow) {
        row.action = "none".into();
        row.reason = reason.to_string();
        row.risk = format!("rejected:{}", reason.tag());
        if reason.halts_day() {
            self.summary.halted_reason = Some(reason.tag().to_string());
            self.notifications.push(("halt", reason.to_string()));
        }
        warn!("pre-tick: {reason}");
    }

    async fn decide(
        &mut self,
        state: &MarketState,
        inputs: &RiskInputs,
        row: &mut JournalRow,
    ) -> Result<(), LoopError> {
        let recent = self.parts.journal.tail(5)?;
        let decider = &mut self.parts.decider;
        let llm_before = decider.llm_cost_today_usd();
        let decision = match decider.decide(state, &recent).await {
            Ok(decision) => decision,
            Err(e) => {
                warn!("decider error, holding: {e}");
                self.notifications.push(("decider error", e.to_string()));
                crate::live::decider::Decision::hold(format!("decider_error: {e}"))
            }
        };
        row.llm_cost_usd = decider.llm_cost_today_usd() - llm_before;
        row.action = decision.action.to_string();
        row.size_usd = decision.size_usd;
        row.reason = decision.reason.clone();
        let verdict = risk::check(&decision, state, &self.parts.pair, &self.cfg.risk, inputs);
        row.risk = verdict.label();
        self.apply_verdict(verdict, state.ts, row);
        Ok(())
    }

    fn apply_verdict(&mut self, verdict: RiskVerdict, now: i64, row: &mut JournalRow) {
        match verdict {
            RiskVerdict::Approved(order) => {
                let placed = self.parts.book.place(&order, self.summary.ticks);
                self.last_order_secs = Some(now);
                self.summary.orders_placed += 1;
                let placed_col = format!(
                    "{}:{} cid={}",
                    placed.id,
                    placed.state,
                    short_id(&order.client_order_id),
                );
                row.order = if row.order.is_empty() {
                    placed_col
                } else {
                    format!("{}; {placed_col}", row.order)
                };
                info!(
                    "PAPER ORDER {} {} {} @ {} (${:.4}) cid={}",
                    placed.id,
                    order.side,
                    order.asset_qty_str,
                    order.limit_price_str,
                    order.notional_usd,
                    order.client_order_id,
                );
            }
            RiskVerdict::Rejected(reason) => {
                self.summary.rejects += 1;
                info!("RISK REJECT {}: {reason}", reason.tag());
            }
            RiskVerdict::Hold => {}
        }
    }

    fn update_account_summary(&mut self) {
        self.summary.realized_pnl_usd = self.parts.book.realized_pnl_usd;
        self.summary.fees_paid_usd = self.parts.book.fees_paid_usd;
    }

    async fn notify(&self, event: &str, detail: &str) {
        let mode = if self.cfg.paper { "paper" } else { "LIVE" };
        let title = format!("limen-live {mode}: {event}");
        let body = format!(
            "{} tick {}: {detail}\nJournal: {}",
            self.cfg.symbol,
            self.summary.ticks,
            self.parts.journal.path().display()
        );
        if let Err(e) = self.parts.notifier.notify(&title, &body).await {
            warn!("local notification failed ({event}): {e}; see journal");
        }
    }

    async fn finished(&mut self, max_ticks: u64) -> bool {
        if self.summary.halted_reason.is_some() {
            warn!("daily loss cap reached; stopping for the UTC day");
            return true;
        }
        if max_ticks != 0 && self.summary.ticks >= max_ticks {
            return true;
        }
        if !sleep_or_ctrl_c(self.cfg.tick_interval_secs).await {
            info!("ctrl-c: stopping");
            self.summary.halted_reason = Some("ctrl_c".into());
            self.notify("halt", "ctrl_c: stopping").await;
            return true;
        }
        false
    }
}
/// First 8 characters of an id (or the whole id when shorter); never panics.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Sleep for the tick interval; returns `false` if Ctrl-C arrived.
///
/// If the signal handler cannot be installed, fall back to a plain sleep so
/// the loop keeps its tick spacing instead of spinning against the venue.
async fn sleep_or_ctrl_c(secs: u64) -> bool {
    let dur = Duration::from_secs(secs);
    tokio::select! {
        _ = tokio::time::sleep(dur) => true,
        r = tokio::signal::ctrl_c() => match r {
            Ok(()) => false,
            Err(e) => {
                warn!("ctrl-c handler unavailable ({e}); sleeping without it");
                tokio::time::sleep(dur).await;
                true
            }
        },
    }
}

#[cfg(test)]
mod tests;
