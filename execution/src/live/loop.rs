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

/// Resolve market source, pair rules, decider and paper book.
pub async fn build_parts(cfg: &LiveConfig, secrets: &Secrets) -> Result<LoopParts, LoopError> {
    cfg.validate()
        .map_err(|e| LoopError::Config(e.to_string()))?;
    if !cfg.paper {
        return Err(LoopError::Config(
            "live order submission is not implemented yet; run with paper = true / --paper".into(),
        ));
    }

    let (market, pair, account_line): (Box<dyn MarketSource>, PairRules, Option<String>) =
        match &cfg.market {
            MarketSourceConfig::Synthetic {
                seed,
                start_price,
                vol,
                half_spread_bps,
            } => (
                Box::new(SyntheticMarket::new(
                    *seed,
                    *start_price,
                    *vol,
                    *half_spread_bps,
                )),
                SyntheticMarket::pair_rules(&cfg.symbol),
                None,
            ),
            MarketSourceConfig::Robinhood => {
                let (api_key, priv_key) = secrets
                    .robinhood()
                    .map_err(|e| LoopError::Config(e.to_string()))?;
                let mut client = RobinhoodClient::new(api_key, priv_key, &cfg.base_url)?;
                let account = client.get_account().await?;
                let pair = client.trading_pair(&cfg.symbol).await?;
                let holding = client.holding(cfg.asset_code()).await?;
                let line = format!(
                    "account {} status={} buying_power={:.2} {} api_tradable={:?} | holding {}={} | pair {} api_tradable={} asset_inc={} quote_inc={} min_order={:?}",
                    account.account_number,
                    account.status,
                    account.buying_power,
                    account.buying_power_currency,
                    account.is_api_tradable,
                    cfg.asset_code(),
                    holding.map(|h| h.total_quantity).unwrap_or(0.0),
                    pair.symbol,
                    pair.is_api_tradable,
                    pair.asset_increment,
                    pair.quote_increment,
                    pair.min_order_amount,
                );
                (
                    Box::new(RobinhoodMarket { client }),
                    PairRules::from(&pair),
                    Some(line),
                )
            }
        };

    let decider: Box<dyn Decider> = match cfg.decider {
        DeciderKind::Rules => Box::new(RulesDecider::new(cfg.rules.clone())),
        DeciderKind::Claude => {
            return Err(LoopError::Config(
                "decider = \"claude\" is not implemented yet; use decider = \"rules\"".into(),
            ))
        }
    };

    Ok(LoopParts {
        market,
        pair,
        decider,
        book: PaperBook::new(cfg.paper_starting_cash_usd, cfg.paper_fee_ratio),
        journal: Journal::new(&cfg.journal_path),
        account_line,
        day_start_realized: None,
    })
}

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
    let LoopParts {
        mut market,
        pair,
        mut decider,
        mut book,
        mut journal,
        account_line,
        day_start_realized,
    } = parts;
    let limits = &cfg.risk;
    let max_ticks = if opts.once { 1 } else { opts.max_ticks };

    let started = Utc::now().timestamp();
    journal.ensure_header(&JournalHeader {
        started_utc: iso(started),
        mode: if cfg.paper {
            "paper".into()
        } else {
            "LIVE".into()
        },
        market_source: market.label(),
        symbol: cfg.symbol.clone(),
        decider: decider.name().to_string(),
        model: match cfg.decider {
            DeciderKind::Claude => Some(cfg.model.clone()),
            DeciderKind::Rules => None,
        },
        limits: limits.clone(),
    })?;

    info!(
        "limen-live start: mode={} market={} symbol={} decider={} journal={} tick_interval={}s max_ticks={}",
        if cfg.paper { "paper" } else { "LIVE" },
        market.label(),
        cfg.symbol,
        decider.name(),
        journal.path().display(),
        cfg.tick_interval_secs,
        if max_ticks == 0 { "∞".to_string() } else { max_ticks.to_string() }
    );
    if let Some(line) = &account_line {
        info!("{line}");
    }

    let mut summary = RunSummary::default();
    let mut recent_mids: Vec<f64> = Vec::new();
    let mut last_order_secs: Option<i64> = None;
    let mut day = utc_day(started);
    let mut day_start_realized = day_start_realized.unwrap_or(book.realized_pnl_usd);
    let mut tick: u64 = 0;

    loop {
        tick += 1;
        let now = Utc::now().timestamp();
        if utc_day(now) != day {
            day = utc_day(now);
            day_start_realized = book.realized_pnl_usd;
            last_order_secs = None;
            info!("new UTC day: daily loss counter reset");
        }

        let quote = match market.quote(&cfg.symbol).await {
            Ok(q) => q,
            Err(e) => {
                warn!("quote failed: {e}");
                summary.ticks = tick;
                summary.realized_pnl_usd = book.realized_pnl_usd;
                summary.fees_paid_usd = book.fees_paid_usd;
                if max_ticks != 0 && tick >= max_ticks {
                    break;
                }
                if !sleep_or_ctrl_c(cfg.tick_interval_secs).await {
                    info!("ctrl-c: stopping");
                    summary.halted_reason = Some("ctrl_c".into());
                    break;
                }
                continue;
            }
        };

        // 0. A crossed / non-positive / non-finite quote touches nothing: no
        //    paper fills, no decision, and it is kept out of the mids window.
        if !quote.is_sane() {
            warn!(
                "bad quote bid={} ask={}: skipping tick {tick}",
                quote.bid, quote.ask
            );
            let row = JournalRow {
                ts_utc: iso(now),
                tick,
                symbol: cfg.symbol.clone(),
                bid: quote.bid,
                ask: quote.ask,
                mid: quote.mid(),
                spread_bps: quote.spread_bps(),
                position_qty: book.position_qty,
                position_usd: 0.0,
                cash_usd: book.cash_usd,
                realized_pnl_usd: book.realized_pnl_usd,
                decider: decider.name().to_string(),
                action: "none".into(),
                reason: "bad_quote".into(),
                risk: "rejected:bad_quote".into(),
                ..Default::default()
            };
            journal.append(&row)?;
            summary.ticks = tick;
            if max_ticks != 0 && tick >= max_ticks {
                break;
            }
            if !sleep_or_ctrl_c(cfg.tick_interval_secs).await {
                info!("ctrl-c: stopping");
                summary.halted_reason = Some("ctrl_c".into());
                break;
            }
            continue;
        }

        // 1. Resting paper orders react to the new quote first.
        let events = book.on_tick(quote.bid, quote.ask, tick, limits.stale_order_ticks);
        let mut order_col = String::new();
        let mut fill_col = String::new();
        let mut pnl_tick = 0.0;
        for ev in &events {
            match ev {
                PaperEvent::Filled {
                    order,
                    price,
                    qty,
                    fee_usd,
                    realized_usd,
                } => {
                    summary.fills += 1;
                    pnl_tick += realized_usd;
                    order_col = format!("{}:{}", order.id, order.state);
                    fill_col = format!("{} {qty:.8} @ {price:.2} fee {fee_usd:.4}", order.side);
                    info!(
                        "FILL {} {} {:.8} @ {:.2} fee={:.4} realized={:+.4} cash={:.4} pos={:.8}",
                        order.id,
                        order.side,
                        qty,
                        price,
                        fee_usd,
                        realized_usd,
                        book.cash_usd,
                        book.position_qty
                    );
                }
                PaperEvent::Cancelled { order, age_ticks } => {
                    summary.cancels += 1;
                    order_col = format!("{}:{} (stale {age_ticks} ticks)", order.id, order.state);
                    info!("CANCEL {} stale after {age_ticks} ticks", order.id);
                }
            }
        }

        // 2. Build the state the decider and risk see. Cash is what can be
        //    spent on notional *after* the fill fee, so a max-size buy can
        //    never drive the book negative.
        let spendable_cash = book.free_cash_usd() / (1.0 + book.fee_ratio);
        let state = MarketState::new(
            now,
            tick,
            &cfg.symbol,
            quote.bid,
            quote.ask,
            book.free_position_qty(),
            book.avg_cost_usd,
            spendable_cash,
            book.realized_pnl_usd,
            recent_mids.clone(),
        );
        recent_mids = state.recent_mids.clone();

        let inputs = RiskInputs {
            now_secs: now,
            last_order_secs,
            open_orders: book.open_orders.len(),
            daily_pnl_usd: book.realized_pnl_usd - day_start_realized,
            llm_cost_today_usd: decider.llm_cost_today_usd(),
            resting_notional_usd: book.resting_notional_usd(),
        };

        let mut row = JournalRow {
            ts_utc: iso(now),
            tick,
            symbol: cfg.symbol.clone(),
            bid: state.bid,
            ask: state.ask,
            mid: state.mid,
            spread_bps: state.spread_bps,
            position_qty: book.position_qty,
            position_usd: book.position_qty * state.mid,
            cash_usd: book.cash_usd,
            realized_pnl_usd: book.realized_pnl_usd,
            decider: decider.name().to_string(),
            order: order_col,
            fill: fill_col,
            pnl_usd: pnl_tick,
            ..Default::default()
        };

        // 3. Pre-tick halts (no decider call when the day is over or the LLM budget is spent).
        let mut halt = false;
        let mut idle = false;
        if let Some(r) = risk::pre_tick_check(limits, &inputs) {
            row.action = "none".into();
            row.reason = r.to_string();
            row.risk = format!("rejected:{}", r.tag());
            if r.halts_day() {
                halt = true;
                summary.halted_reason = Some(r.tag().to_string());
            } else {
                idle = true;
            }
            warn!("pre-tick: {r}");
        } else {
            // 4. Decide.
            let recent = journal.tail(5)?;
            let llm_before = decider.llm_cost_today_usd();
            let decision = match decider.decide(&state, &recent).await {
                Ok(d) => d,
                Err(e) => {
                    warn!("decider error, holding: {e}");
                    crate::live::decider::Decision::hold(format!("decider_error: {e}"))
                }
            };
            row.llm_cost_usd = decider.llm_cost_today_usd() - llm_before;
            row.action = decision.action.to_string();
            row.size_usd = decision.size_usd;
            row.reason = decision.reason.clone();

            // 5. Risk.
            let verdict = risk::check(&decision, &state, &pair, limits, &inputs);
            row.risk = verdict.label();
            match &verdict {
                RiskVerdict::Approved(order) => {
                    // 6. Paper: record a resting order. (Live submission: later step.)
                    let placed = book.place(order, tick);
                    last_order_secs = Some(now);
                    summary.orders_placed += 1;
                    // Keep any fill/cancel recorded earlier this tick alongside the new order.
                    let placed_col = format!(
                        "{}:{} cid={}",
                        placed.id,
                        placed.state,
                        short_id(&order.client_order_id)
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
                        order.client_order_id
                    );
                }
                RiskVerdict::Rejected(r) => {
                    summary.rejects += 1;
                    info!("RISK REJECT {}: {r}", r.tag());
                }
                RiskVerdict::Hold => {}
            }
        }

        info!(
            "tick {} {} bid={:.2} ask={:.2} spread={:.2}bps pos={:.8} (${:.4}) cash={:.4} realized={:+.4} | {} {} ${:.4} \"{}\" | {}",
            tick,
            cfg.symbol,
            state.bid,
            state.ask,
            state.spread_bps,
            book.position_qty,
            book.position_qty * state.mid,
            book.cash_usd,
            book.realized_pnl_usd,
            row.decider,
            row.action,
            row.size_usd,
            row.reason,
            row.risk
        );
        journal.append(&row)?;
        if opts.once {
            println!("{}", row.to_markdown());
        }

        summary.ticks = tick;
        summary.realized_pnl_usd = book.realized_pnl_usd;
        summary.fees_paid_usd = book.fees_paid_usd;
        summary.final_equity_usd = book.equity_usd(state.mid);

        if halt {
            warn!("daily loss cap reached; stopping for the UTC day");
            break;
        }
        if max_ticks != 0 && tick >= max_ticks {
            break;
        }
        let _ = idle;
        if !sleep_or_ctrl_c(cfg.tick_interval_secs).await {
            info!("ctrl-c: stopping");
            summary.halted_reason = Some("ctrl_c".into());
            break;
        }
    }

    info!(
        "limen-live done: ticks={} orders={} fills={} cancels={} rejects={} realized={:+.4} fees={:.4} equity={:.4} halted={:?}",
        summary.ticks,
        summary.orders_placed,
        summary.fills,
        summary.cancels,
        summary.rejects,
        summary.realized_pnl_usd,
        summary.fees_paid_usd,
        summary.final_equity_usd,
        summary.halted_reason
    );
    Ok(summary)
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
mod tests {
    use super::*;
    use crate::live::config::MarketSourceConfig;

    fn cfg(journal: &std::path::Path, seed: u64) -> LiveConfig {
        LiveConfig {
            tick_interval_secs: 1,
            journal_path: journal.to_string_lossy().into_owned(),
            market: MarketSourceConfig::Synthetic {
                seed,
                start_price: 100_000.0,
                vol: 0.004,
                half_spread_bps: 1.0,
            },
            ..LiveConfig::default()
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let p =
            std::env::temp_dir().join(format!("limen_live_loop_{name}_{}.md", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn short_id_never_panics() {
        assert_eq!(short_id("c8f53536-d7b4-44fd-8076-cac31e8f3880"), "c8f53536");
        assert_eq!(short_id("abc"), "abc");
        assert_eq!(short_id(""), "");
    }

    /// Scripted quotes for deterministic loop tests.
    struct ScriptedMarket(std::collections::VecDeque<(f64, f64)>);

    impl MarketSource for ScriptedMarket {
        fn label(&self) -> String {
            "scripted".into()
        }
        fn quote<'a>(&'a mut self, symbol: &'a str) -> crate::live::market::QuoteFuture<'a> {
            let (bid, ask) = self.0.pop_front().unwrap_or((100.0, 100.1));
            let q = crate::live::robinhood::types::BestBidAsk {
                symbol: symbol.to_string(),
                bid,
                ask,
            };
            Box::pin(async move { Ok(q) })
        }
    }

    async fn scripted_parts(c: &LiveConfig, quotes: &[(f64, f64)], cash: f64) -> LoopParts {
        let mut parts = build_parts(c, &Secrets::default()).await.unwrap();
        parts.market = Box::new(ScriptedMarket(quotes.iter().copied().collect()));
        parts.book = PaperBook::new(cash, c.paper_fee_ratio);
        parts
    }

    #[tokio::test]
    async fn bad_quote_touches_nothing_and_stays_out_of_the_window() {
        let p = tmp("badquote");
        let mut c = cfg(&p, 1);
        c.risk.min_tick_interval_secs = 1;
        c.rules.entry_bps = 1.0;
        // tick 1 sane, tick 2 crossed, tick 3 sane: tick 3 must say warming_up 2/3.
        let parts =
            scripted_parts(&c, &[(100.0, 100.1), (100.2, 100.0), (100.0, 100.1)], 10.0).await;
        let s = run_with_parts(
            &c,
            &RunOptions {
                once: false,
                max_ticks: 3,
            },
            parts,
        )
        .await
        .unwrap();
        assert_eq!(s.ticks, 3);
        assert_eq!(s.orders_placed, 0);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("rejected:bad_quote"), "{text}");
        assert!(text.contains("warming_up 2/3"), "{text}");
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn max_size_buy_never_drives_paper_cash_negative() {
        let p = tmp("fee");
        let mut c = cfg(&p, 1);
        c.risk.min_tick_interval_secs = 1;
        c.rules.entry_bps = 1.0;
        c.rules.order_usd = 50.0; // decider wants more than the $5 cap; cash is the binding limit
        c.risk.max_order_usd = 10.0;
        // Three flat quotes to warm up, a dip to trigger the buy, then a lower ask to fill it.
        let parts = scripted_parts(
            &c,
            &[
                (100.0, 100.1),
                (100.0, 100.1),
                (100.0, 100.1),
                (99.0, 99.1),
                (98.0, 98.1),
            ],
            5.0,
        )
        .await;
        let s = run_with_parts(
            &c,
            &RunOptions {
                once: false,
                max_ticks: 5,
            },
            parts,
        )
        .await
        .unwrap();
        assert_eq!(s.orders_placed, 1, "{s:?}");
        assert_eq!(s.fills, 1, "{s:?}");
        let text = std::fs::read_to_string(&p).unwrap();
        // Cash column of the last row is non-negative and the fee was charged.
        let last = text.lines().last().unwrap();
        let cash: f64 = last.split(" | ").nth(9).unwrap().parse().unwrap();
        assert!(cash >= 0.0, "cash went negative: {last}");
        assert!(s.fees_paid_usd > 0.0);
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn refuses_live_mode_and_claude_decider_for_now() {
        let p = tmp("refuse");
        let mut c = cfg(&p, 1);
        c.paper = false;
        c.market = MarketSourceConfig::Robinhood;
        let err = build_parts(&c, &Secrets::default()).await.unwrap_err();
        assert!(err.to_string().contains("not implemented"), "{err}");
        let mut c = cfg(&p, 1);
        c.decider = DeciderKind::Claude;
        let err = build_parts(&c, &Secrets::default()).await.unwrap_err();
        assert!(err.to_string().contains("claude"), "{err}");
    }

    #[tokio::test]
    async fn robinhood_source_needs_secrets() {
        let p = tmp("secrets");
        let mut c = cfg(&p, 1);
        c.market = MarketSourceConfig::Robinhood;
        let err = build_parts(&c, &Secrets::default()).await.unwrap_err();
        assert!(err.to_string().contains("ROBINHOOD_API_KEY"), "{err}");
    }

    #[tokio::test]
    async fn once_writes_header_and_one_row() {
        let p = tmp("once");
        let c = cfg(&p, 3);
        let s = run(
            &c,
            &Secrets::default(),
            &RunOptions {
                once: true,
                max_ticks: 0,
            },
        )
        .await
        .unwrap();
        assert_eq!(s.ticks, 1);
        assert_eq!(s.orders_placed, 0, "warming up: no order on tick 1");
        let j = Journal::new(&p);
        assert_eq!(j.row_count().unwrap(), 1);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("mode: **paper**"));
        assert!(text.contains("warming_up"));
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn paper_run_trades_and_journals_with_caps_respected() {
        let p = tmp("trades");
        let mut c = cfg(&p, 3);
        c.risk.min_tick_interval_secs = 1; // ticks are 1 s apart in this test
        c.rules.entry_bps = 1.0; // trigger on small dips so a 12-tick run trades
        c.rules.exit_bps = 1.0;
        let s = run(
            &c,
            &Secrets::default(),
            &RunOptions {
                once: false,
                max_ticks: 12,
            },
        )
        .await
        .unwrap();
        assert_eq!(s.ticks, 12);
        assert!(
            s.orders_placed >= 1,
            "expected at least one paper order: {s:?}"
        );
        let j = Journal::new(&p);
        assert_eq!(j.row_count().unwrap(), 12);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("approved buy"), "{text}");
        // Caps: every approved order ≤ max_order_usd and no shorts.
        for line in text.lines().filter(|l| l.contains("approved")) {
            let usd: f64 = line
                .split("($")
                .nth(1)
                .and_then(|s| s.split(')').next())
                .and_then(|s| s.parse().ok())
                .unwrap();
            assert!(usd <= c.risk.max_order_usd + 1e-9, "{line}");
        }
        assert!(s.final_equity_usd > 0.0);
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn daily_loss_cap_halts_the_run() {
        let p = tmp("halt");
        let mut c = cfg(&p, 5);
        c.risk.max_daily_loss_usd = 0.5;
        let mut parts = build_parts(&c, &Secrets::default()).await.unwrap();
        parts.book.realized_pnl_usd = -0.6; // losses earlier today
        parts.day_start_realized = Some(0.0);
        let s = run_with_parts(
            &c,
            &RunOptions {
                once: false,
                max_ticks: 5,
            },
            parts,
        )
        .await
        .unwrap();
        assert_eq!(s.ticks, 1, "halts on the first tick");
        assert_eq!(s.halted_reason.as_deref(), Some("daily_loss_breached"));
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("rejected:daily_loss_breached"));
        let _ = std::fs::remove_file(&p);
    }
}
