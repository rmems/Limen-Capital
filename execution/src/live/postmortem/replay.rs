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

pub(super) fn analyze(text: &str, cfg: &LiveConfig, day: &str) -> io::Result<Analysis> {
    if !text.starts_with("# limen-live journal") {
        return Err(io::Error::other("not a limen-live journal"));
    }
    let mut today_spend = 0.0;
    let mut rows = Vec::new();
    let (mut malformed, mut rejects, mut parse_errors, mut trades) = (0, 0, 0, 0);
    let mut fees = 0.0;
    let mut unknown_fees = 0;
    let mut multiple_runs = false;
    for line in text.lines().filter(|l| l.starts_with("| ")) {
        let c = cells(line);
        if c.first().map(String::as_str) == Some(COLUMNS[0]) {
            continue;
        }
        // Unknown costs cannot be allowed to bypass the daily cap.
        if c.len() != COLUMNS.len() {
            return Err(io::Error::other(
                "journal row has unknown LLM cost (wrong column count)",
            ));
        }
        let cost = number(&c[19])
            .filter(|v| *v >= 0.0)
            .ok_or_else(|| io::Error::other("invalid journal LLM cost"))?;
        let ts = DateTime::parse_from_rfc3339(&c[0])
            .map_err(|_| io::Error::other("invalid journal timestamp"))?;
        if ts
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d")
            .to_string()
            == day
        {
            today_spend += cost;
        }
        if c[12] == "run_start" {
            multiple_runs = true;
            continue;
        }
        rejects += usize::from(c[15].starts_with("rejected:"));
        parse_errors += usize::from(c[14].contains("parse_error"));
        if c[17] != "-" && !c[17].is_empty() {
            trades += 1;
            match c[17]
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .find(|w| w[0] == "fee")
                .and_then(|w| number(w[1]))
            {
                Some(fee) if fee >= 0.0 => fees += fee,
                _ => unknown_fees += 1,
            }
        }
        let bid = number(&c[3]);
        let ask = number(&c[4]);
        if bid.is_none()
            || ask.is_none()
            || c[1].parse::<u64>().is_err()
            || [7, 9, 10].iter().any(|i| number(&c[*i]).is_none())
        {
            malformed += 1;
            continue;
        }
        rows.push(c);
    }
    if rows.is_empty() {
        return Err(io::Error::other("journal has no usable ticks"));
    }
    let comparison = if malformed > 0 || multiple_runs {
        "Rules comparison: inconclusive (malformed ticks or multiple runs).".into()
    } else {
        replay(&rows, cfg)?
    };
    Ok(Analysis {
        evidence: format!(
            "## Recorded evidence\n\n- Trades (fill rows): {trades}; fees recorded: ${fees:.4}; fills without fee data: {unknown_fees}.\n- Rejects: {rejects}; decider parse errors: {parse_errors}; malformed numeric rows: {malformed}.\n- {comparison}\n- Replay assumptions: supplied config rules, fees and caps; synthetic pair increments/minimum; recorded rounded bid/ask; resting limit fills on later crossing quotes only. No claim about actual venue fills. Journal omits original rules config and pair metadata; rerun with the original --config. Unknown fees are not zero.\n",
        ),
        today_spend,
    })
}

fn replay(rows: &[Vec<String>], cfg: &LiveConfig) -> io::Result<String> {
    let first = &rows[0];
    if number(&first[7]) != Some(0.0) || first[16] != "-" || first[17] != "-" {
        return Ok("Rules comparison: inconclusive (initial holdings or resting orders cannot be reconstructed).".into());
    }
    let mut book = PaperBook::new(number(&first[9]).unwrap(), cfg.paper_fee_ratio);
    let initial = book.cash_usd;
    let rules = RulesDecider::new(cfg.rules.clone());
    let mut mids = Vec::new();
    let mut last_order = None;
    let mut prev_tick = 0;
    let mut prev_ts = i64::MIN;
    let mut day = String::new();
    let mut baseline = 0.0;
    let mut halted = false;
    let mut last_mid = 0.0;
    let mut observed_equity = initial;
    for row in rows {
        let tick: u64 = row[1].parse().unwrap();
        let ts = DateTime::parse_from_rfc3339(&row[0]).unwrap();
        if tick != prev_tick + 1 || ts.timestamp() < prev_ts || row[2] != cfg.symbol {
            return Ok("Rules comparison: inconclusive (tick ordering or symbol mismatch).".into());
        }
        prev_tick = tick;
        prev_ts = ts.timestamp();
        let bid = number(&row[3]).unwrap();
        let ask = number(&row[4]).unwrap();
        if bid <= 0.0 || ask < bid {
            continue;
        }
        last_mid = (bid + ask) / 2.0;
        observed_equity = number(&row[9]).unwrap() + number(&row[7]).unwrap() * last_mid;
        let current_day = ts
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d")
            .to_string();
        if day != current_day {
            day = current_day;
            baseline = book.realized_pnl_usd;
            last_order = None;
            halted = false;
        }
        if halted {
            continue;
        }
        book.on_tick(bid, ask, tick, cfg.risk.stale_order_ticks);
        let state = MarketState::new(
            ts.timestamp(),
            tick,
            &cfg.symbol,
            bid,
            ask,
            AccountState {
                position_qty: book.free_position_qty(),
                avg_cost_usd: book.avg_cost_usd,
                cash_usd: book.free_cash_usd() / (1.0 + book.fee_ratio),
                realized_pnl_usd: book.realized_pnl_usd,
            },
            mids,
        );
        mids = state.recent_mids.clone();
        let inputs = RiskInputs {
            now_secs: state.ts,
            last_order_secs: last_order,
            open_orders: book.open_orders.len(),
            daily_pnl_usd: book.realized_pnl_usd - baseline,
            llm_cost_today_usd: 0.0,
            resting_notional_usd: book.resting_notional_usd(),
        };
        if risk::pre_tick_check(&cfg.risk, &inputs).is_some() {
            halted = true;
            continue;
        }
        let decision = rules.decide_sync(&state);
        if let RiskVerdict::Approved(order) = risk::check(
            &decision,
            &state,
            &SyntheticMarket::pair_rules(&cfg.symbol),
            &cfg.risk,
            &inputs,
        ) {
            book.place(&order, tick);
            last_order = Some(state.ts);
        }
    }
    if last_mid == 0.0 {
        return Ok("Rules comparison: inconclusive (no sane quotes).".into());
    }
    let equity = book.equity_usd(last_mid);
    let delta = equity - observed_equity;
    // Journal cash/qty and prices are rounded; don't claim a winner within noise.
    let verdict = if delta > 0.001 {
        "rules would have done better under these assumptions"
    } else if delta < -0.001 {
        "rules would not have done better under these assumptions"
    } else {
        "no material difference at journal precision"
    };
    Ok(format!("Rules comparison: {verdict}. Starting equity ${initial:.4}; rules final equity ${equity:.4}, recorded final equity ${observed_equity:.4}, difference ${delta:+.4}; rules fills {}, fees ${:.4}. Equity includes marked open positions, excludes LLM spend; unfilled orders remain unfilled.", book.fills, book.fees_paid_usd))
}
