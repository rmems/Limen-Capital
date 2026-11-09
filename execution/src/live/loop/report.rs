//! Journal and logging presentation, separate from tick sequencing.

use super::*;
use crate::live::robinhood::types::BestBidAsk;

pub(super) fn start(
    cfg: &LiveConfig,
    parts: &mut LoopParts,
    started: i64,
    max_ticks: u64,
) -> Result<(), LoopError> {
    parts.journal.ensure_header(&JournalHeader {
        started_utc: iso(started),
        mode: if cfg.paper {
            "paper".into()
        } else {
            "LIVE".into()
        },
        market_source: parts.market.label(),
        symbol: cfg.symbol.clone(),
        decider: parts.decider.name().to_string(),
        model: match cfg.decider {
            DeciderKind::Claude => Some(cfg.model.clone()),
            DeciderKind::Rules => None,
        },
        limits: cfg.risk.clone(),
    })?;
    info!(
        "limen-live start: mode={} market={} symbol={} decider={} journal={} tick_interval={}s max_ticks={}",
        if cfg.paper { "paper" } else { "LIVE" },
        parts.market.label(), cfg.symbol, parts.decider.name(), parts.journal.path().display(),
        cfg.tick_interval_secs,
        if max_ticks == 0 { "∞".to_string() } else { max_ticks.to_string() },
    );
    if let Some(line) = &parts.account_line {
        info!("{line}");
    }
    Ok(())
}

pub(super) fn bad_quote(
    cfg: &LiveConfig,
    parts: &mut LoopParts,
    quote: &BestBidAsk,
    now: i64,
    tick: u64,
) -> Result<(), LoopError> {
    warn!(
        "bad quote bid={} ask={}: skipping tick {tick}",
        quote.bid, quote.ask
    );
    parts.journal.append(&JournalRow {
        ts_utc: iso(now),
        tick,
        symbol: cfg.symbol.clone(),
        bid: quote.bid,
        ask: quote.ask,
        mid: quote.mid(),
        spread_bps: quote.spread_bps(),
        position_qty: parts.book.position_qty,
        position_usd: 0.0,
        cash_usd: parts.book.cash_usd,
        realized_pnl_usd: parts.book.realized_pnl_usd,
        decider: parts.decider.name().to_string(),
        action: "none".into(),
        reason: "bad_quote".into(),
        risk: "rejected:bad_quote".into(),
        ..Default::default()
    })?;
    Ok(())
}

pub(super) fn row(state: &MarketState, parts: &LoopParts) -> JournalRow {
    JournalRow {
        ts_utc: iso(state.ts),
        tick: state.tick,
        symbol: state.symbol.clone(),
        bid: state.bid,
        ask: state.ask,
        mid: state.mid,
        spread_bps: state.spread_bps,
        position_qty: parts.book.position_qty,
        position_usd: parts.book.position_qty * state.mid,
        cash_usd: parts.book.cash_usd,
        realized_pnl_usd: parts.book.realized_pnl_usd,
        decider: parts.decider.name().to_string(),
        ..Default::default()
    }
}

pub(super) fn events(events: &[PaperEvent], row: &mut JournalRow, summary: &mut RunSummary) {
    for event in events {
        match event {
            PaperEvent::Filled {
                order,
                price,
                qty,
                fee_usd,
                realized_usd,
            } => {
                summary.fills += 1;
                row.pnl_usd += realized_usd;
                row.order = format!("{}:{}", order.id, order.state);
                row.fill = format!("{} {qty:.8} @ {price:.2} fee {fee_usd:.4}", order.side);
                info!(
                    "FILL {} {} {:.8} @ {:.2} fee={:.4} realized={:+.4} cash={:.4} pos={:.8}",
                    order.id,
                    order.side,
                    qty,
                    price,
                    fee_usd,
                    realized_usd,
                    row.cash_usd,
                    row.position_qty,
                );
            }
            PaperEvent::Cancelled { order, age_ticks } => {
                summary.cancels += 1;
                row.order = format!("{}:{} (stale {age_ticks} ticks)", order.id, order.state);
                info!("CANCEL {} stale after {age_ticks} ticks", order.id);
            }
        }
    }
}

pub(super) fn tick(row: &JournalRow) {
    info!(
        "tick {} {} bid={:.2} ask={:.2} spread={:.2}bps pos={:.8} (${:.4}) cash={:.4} realized={:+.4} | {} {} ${:.4} \"{}\" | {}",
        row.tick, row.symbol, row.bid, row.ask, row.spread_bps,
        row.position_qty, row.position_usd, row.cash_usd, row.realized_pnl_usd,
        row.decider, row.action, row.size_usd, row.reason, row.risk,
    );
}

pub(super) fn done(summary: &RunSummary) {
    info!(
        "limen-live done: ticks={} orders={} fills={} cancels={} rejects={} realized={:+.4} fees={:.4} equity={:.4} halted={:?}",
        summary.ticks, summary.orders_placed, summary.fills, summary.cancels, summary.rejects,
        summary.realized_pnl_usd, summary.fees_paid_usd, summary.final_equity_usd, summary.halted_reason,
    );
}
