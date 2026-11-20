use crate::live::decider::rules::RulesDecider;
use crate::live::journal::COLUMNS;
use crate::live::market::SyntheticMarket;
use crate::live::market_state::{AccountState, MarketState};
use crate::live::paper::PaperBook;
use crate::live::risk::{self, RiskInputs, RiskVerdict};
use crate::live::LiveConfig;
use chrono::DateTime;
use std::io;

pub(super) struct Analysis {
    pub evidence: String,
    pub today_spend: f64,
}

pub(crate) fn cells(line: &str) -> Vec<String> {
    let mut cells = vec![String::new()];
    let mut previous = '\0';
    for c in line.trim().trim_matches('|').chars() {
        if c == '|' && previous != '\\' {
            cells.push(String::new());
        } else {
            cells.last_mut().unwrap().push(c);
        }
        previous = c;
    }
    cells
        .into_iter()
        .map(|s| s.trim().replace("\\|", "|"))
        .collect()
}

fn number(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[derive(Default)]
struct Stats {
    today_spend: f64,
    malformed: usize,
    rejects: usize,
    parse_errors: usize,
    trades: usize,
    fees: f64,
    unknown_fees: usize,
    multiple_runs: bool,
}

impl Stats {
    fn fee(&mut self, fill: &str) {
        match fill
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .find(|w| w[0] == "fee")
            .and_then(|w| number(w[1]))
        {
            Some(fee) if fee >= 0.0 => self.fees += fee,
            _ => self.unknown_fees += 1,
        }
    }
}

/// A journal row's cells and the UTC day it is billed against.
struct Row<'a> {
    cells: &'a [String],
    day: &'a str,
}

/// Validated LLM cost and whether the row's UTC day is `day`.
fn checked_cost(r: &Row<'_>) -> io::Result<(f64, bool)> {
    let c = r.cells;
    // Unknown costs cannot be allowed to bypass the daily cap. Journals from
    // before the `llm calls` audit column are one cell shorter; `llm $` stays
    // at index 19 in both schemas.
    if c.len() != COLUMNS.len() && c.len() != COLUMNS.len() - 1 {
        return Err(io::Error::other(
            "journal row has unknown LLM cost (wrong column count)",
        ));
    }
    let cost = number(&c[19])
        .filter(|v| *v >= 0.0)
        .ok_or_else(|| io::Error::other("invalid journal LLM cost"))?;
    let ts = DateTime::parse_from_rfc3339(&c[0])
        .map_err(|_| io::Error::other("invalid journal timestamp"))?;
    Ok((
        cost,
        ts.with_timezone(&chrono::Utc)
            .format("%Y-%m-%d")
            .to_string()
            == r.day,
    ))
}

/// Numeric fields a replayable row needs.
fn wellformed(c: &[String]) -> bool {
    c[1].parse::<u64>().is_ok()
        && [3usize, 4, 7, 9, 10]
            .iter()
            .all(|i| number(&c[*i]).is_some())
}

/// Classify one journal row; returns whether it is usable for replay.
fn row_stats(r: &Row<'_>, s: &mut Stats) -> io::Result<bool> {
    let c = r.cells;
    let (cost, today) = checked_cost(r)?;
    if today {
        s.today_spend += cost;
    }
    if c[12] == "run_start" {
        s.multiple_runs = true;
        return Ok(false);
    }
    s.rejects += usize::from(c[15].starts_with("rejected:"));
    s.parse_errors += usize::from(c[14].contains("parse_error"));
    if c[17] != "-" && !c[17].is_empty() {
        s.trades += 1;
        s.fee(&c[17]);
    }
    if wellformed(c) {
        return Ok(true);
    }
    s.malformed += 1;
    Ok(false)
}

fn evidence(s: &Stats, comparison: &str) -> String {
    format!(
        "## Recorded evidence\n\n- Trades (fill rows): {}; fees recorded: ${:.4}; fills without fee data: {}.\n- Rejects: {}; decider parse errors: {}; malformed numeric rows: {}.\n- {comparison}\n- Replay assumptions: supplied config rules, fees and caps; synthetic pair increments/minimum; recorded rounded bid/ask; resting limit fills on later crossing quotes only. No claim about actual venue fills. Journal omits original rules config and pair metadata; rerun with the original --config. Unknown fees are not zero.\n",
        s.trades, s.fees, s.unknown_fees, s.rejects, s.parse_errors, s.malformed,
    )
}

/// Journal text and the UTC day it is billed against.
pub(super) struct Doc<'a> {
    pub(super) text: &'a str,
    pub(super) day: &'a str,
}

/// Rows usable for replay, with `s` accumulating row statistics.
fn usable_rows(doc: &Doc<'_>, s: &mut Stats) -> io::Result<Vec<Vec<String>>> {
    let mut rows = Vec::new();
    for line in doc.text.lines().filter(|l| l.starts_with("| ")) {
        let c = cells(line);
        if c.first().map(String::as_str) == Some(COLUMNS[0]) {
            continue;
        }
        let r = Row {
            cells: &c,
            day: doc.day,
        };
        if row_stats(&r, s)? {
            rows.push(c);
        }
    }
    Ok(rows)
}

pub(super) fn analyze(doc: &Doc<'_>, cfg: &LiveConfig) -> io::Result<Analysis> {
    if !doc.text.starts_with("# limen-live journal") {
        return Err(io::Error::other("not a limen-live journal"));
    }
    let mut s = Stats::default();
    let rows = usable_rows(doc, &mut s)?;
    if rows.is_empty() {
        return Err(io::Error::other("journal has no usable ticks"));
    }
    let comparison = if s.malformed > 0 || s.multiple_runs {
        "Rules comparison: inconclusive (malformed ticks or multiple runs).".into()
    } else {
        replay(&rows, cfg)?
    };
    Ok(Analysis {
        evidence: evidence(&s, &comparison),
        today_spend: s.today_spend,
    })
}

struct Sim<'a> {
    cfg: &'a LiveConfig,
    book: PaperBook,
    rules: RulesDecider,
    mids: Vec<f64>,
    last_order: Option<i64>,
    prev_tick: u64,
    prev_ts: i64,
    day: String,
    baseline: f64,
    halted: bool,
    last_mid: f64,
    observed_equity: f64,
}

impl Sim<'_> {
    fn roll_day(&mut self, ts: DateTime<chrono::FixedOffset>) {
        let current = ts
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d")
            .to_string();
        if self.day != current {
            self.day = current;
            self.baseline = self.book.realized_pnl_usd;
            self.last_order = None;
            self.halted = false;
        }
    }

    /// One recorded sane quote: mark, roll the day, then act unless halted.
    fn tick(&mut self, quote: &Tick) {
        let Tick {
            n: tick,
            ts,
            bid,
            ask,
        } = quote;
        self.last_mid = (bid + ask) / 2.0;
        self.roll_day(*ts);
        if self.halted {
            return;
        }
        self.book
            .on_tick(*bid, *ask, *tick, self.cfg.risk.stale_order_ticks);
        let state = self.market(quote);
        let inputs = RiskInputs {
            now_secs: state.ts,
            last_order_secs: self.last_order,
            open_orders: self.book.open_orders.len(),
            daily_pnl_usd: self.book.realized_pnl_usd - self.baseline,
            llm_cost_today_usd: 0.0,
            resting_notional_usd: self.book.resting_notional_usd(),
        };
        if risk::pre_tick_check(&self.cfg.risk, &inputs).is_some() {
            self.halted = true;
            return;
        }
        self.decide(&state, &inputs, *tick);
    }

    /// Market state for one quote, keeping the rolling mids.
    fn market(&mut self, quote: &Tick) -> MarketState {
        let Tick { n, ts, bid, ask } = quote;
        let state = MarketState::new(
            ts.timestamp(),
            *n,
            &self.cfg.symbol,
            *bid,
            *ask,
            AccountState {
                position_qty: self.book.free_position_qty(),
                avg_cost_usd: self.book.avg_cost_usd,
                cash_usd: self.book.free_cash_usd() / (1.0 + self.book.fee_ratio),
                realized_pnl_usd: self.book.realized_pnl_usd,
            },
            std::mem::take(&mut self.mids),
        );
        self.mids = state.recent_mids.clone();
        state
    }

    /// Decide and place any approved order.
    fn decide(&mut self, state: &MarketState, inputs: &RiskInputs, tick: u64) {
        let decision = self.rules.decide_sync(state);
        if let RiskVerdict::Approved(order) = risk::check(
            &decision,
            state,
            &SyntheticMarket::pair_rules(&self.cfg.symbol),
            &self.cfg.risk,
            inputs,
        ) {
            self.book.place(&order, tick);
            self.last_order = Some(state.ts);
        }
    }

    /// Returns an inconclusive reason, or None when the row was stepped.
    fn step(&mut self, row: &[String]) -> Option<&'static str> {
        let n: u64 = row[1].parse().unwrap();
        let ts = DateTime::parse_from_rfc3339(&row[0]).unwrap();
        // Ticks are recorded sparsely: a failed quote writes no row, and the
        // shutdown row reuses the last tick number.
        if n < self.prev_tick || ts.timestamp() < self.prev_ts {
            return Some("tick ordering or symbol mismatch");
        }
        if row[2] != self.cfg.symbol {
            return Some("tick ordering or symbol mismatch");
        }
        self.prev_tick = n;
        self.prev_ts = ts.timestamp();
        let bid = number(&row[3]).unwrap();
        let ask = number(&row[4]).unwrap();
        if bid <= 0.0 || ask < bid {
            return None;
        }
        self.observed_equity =
            number(&row[9]).unwrap() + number(&row[7]).unwrap() * (bid + ask) / 2.0;
        self.tick(&Tick { n, ts, bid, ask });
        None
    }
}

/// One recorded tick's quote fields.
struct Tick {
    n: u64,
    ts: DateTime<chrono::FixedOffset>,
    bid: f64,
    ask: f64,
}

/// A reconstructable run starts flat: no holdings, no fills, and no order
/// cancelled before the journal's first row (a split mid-run journal). An
/// order placed on tick 1 is replayed by the sim itself, not reconstructed.
fn starts_flat(first: &[String]) -> bool {
    [
        number(&first[7]) == Some(0.0),
        first[17] == "-",
        !first[16].contains("cancelled"),
    ]
    .into_iter()
    .all(|v| v)
}

fn replay(rows: &[Vec<String>], cfg: &LiveConfig) -> io::Result<String> {
    let first = &rows[0];
    if !starts_flat(first) {
        return Ok("Rules comparison: inconclusive (initial holdings or resting orders cannot be reconstructed).".into());
    }
    let book = PaperBook::new(number(&first[9]).unwrap(), cfg.paper_fee_ratio);
    let initial = book.cash_usd;
    let mut sim = Sim {
        cfg,
        book,
        rules: RulesDecider::new(cfg.rules.clone()),
        mids: Vec::new(),
        last_order: None,
        prev_tick: 0,
        prev_ts: i64::MIN,
        day: String::new(),
        baseline: 0.0,
        halted: false,
        last_mid: 0.0,
        observed_equity: initial,
    };
    for row in rows {
        if let Some(reason) = sim.step(row) {
            return Ok(format!("Rules comparison: inconclusive ({reason})."));
        }
    }
    if sim.last_mid == 0.0 {
        return Ok("Rules comparison: inconclusive (no sane quotes).".into());
    }
    Ok(report(initial, &sim))
}

fn report(initial: f64, sim: &Sim) -> String {
    let equity = sim.book.equity_usd(sim.last_mid);
    let delta = equity - sim.observed_equity;
    // Journal cash/qty and prices are rounded; don't claim a winner within noise.
    let verdict = if delta > 0.001 {
        "rules would have done better under these assumptions"
    } else if delta < -0.001 {
        "rules would not have done better under these assumptions"
    } else {
        "no material difference at journal precision"
    };
    format!("Rules comparison: {verdict}. Starting equity ${initial:.4}; rules final equity ${equity:.4}, recorded final equity ${:.4}, difference ${delta:+.4}; rules fills {}, fees ${:.4}. Equity includes marked open positions, excludes LLM spend; unfilled orders remain unfilled.", sim.observed_equity, sim.book.fills, sim.book.fees_paid_usd)
}
