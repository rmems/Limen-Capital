//! Quantity-aware adapter for the pinned legacy GhostWallet. The upstream
//! execute_* helpers choose their own sizes, so cannot enforce our policy cap.
//! Keep its ATP friction, weighted basis, PnL, and JSONL format, but size once.

use crate::{GhostWallet, TradeSide, TradeSignal};
use metabolic_ledger::{log::append_ghost_log, GhostTradeLog, METABOLIC_COST};

fn round_down(value: f64) -> f32 {
    let rounded = value as f32;
    if rounded as f64 > value {
        rounded.next_down()
    } else {
        rounded
    }
}

/// Returns the actual filled units, or None for insufficient ATP/inventory/dust.
pub(crate) fn execute_sized(
    wallet: &mut GhostWallet,
    signal: &TradeSignal,
    quantity: f64,
    step: u64,
    reason: &str,
    log_path: &str,
) -> Option<f64> {
    let asset = &signal.ticker;
    let price = signal.price as f32;
    // Cap against both the original price and the legacy f32 audit price.
    let notional = quantity * signal.price.min(price as f64);
    let (qty, value, pnl) = match signal.side {
        TradeSide::Buy => {
            let spend = round_down(notional.min(wallet.balance_atp as f64));
            if spend < 0.01 {
                return None;
            }
            let fee = spend * METABOLIC_COST;
            let net_spend = spend - fee;
            let qty = round_down(net_spend as f64 / price as f64);
            if qty <= 0.0 || !qty.is_finite() {
                return None;
            }
            let old_qty = wallet.balance(asset);
            let cost = wallet.entry_price(asset) * old_qty + net_spend;
            let new_qty = old_qty + qty;
            wallet.balances.insert(asset.clone(), new_qty);
            wallet.entry_prices.insert(asset.clone(), cost / new_qty);
            wallet.balance_atp -= spend;
            (qty, spend, -fee)
        }
        TradeSide::Sell => {
            let qty = round_down(
                quantity
                    .min(notional / price as f64)
                    .min(wallet.balance(asset) as f64),
            );
            if qty < 1e-9 {
                return None;
            }
            let proceeds = round_down(qty as f64 * price as f64);
            let fee = proceeds * METABOLIC_COST;
            let pnl = (price - wallet.entry_price(asset)) * qty - fee;
            let remaining = wallet.balance(asset) - qty;
            if remaining > 1e-9 {
                wallet.balances.insert(asset.clone(), remaining);
            } else {
                wallet.balances.remove(asset);
                wallet.entry_prices.remove(asset);
            }
            wallet.balance_atp += proceeds - fee;
            wallet.cumulative_pnl += pnl;
            *wallet.realized_pnls.entry(asset.clone()).or_default() += pnl;
            wallet.record_pnl_and_update_kelly(pnl);
            (qty, proceeds, pnl)
        }
        TradeSide::Neutral => return None,
    };
    wallet.trade_count += 1;
    append_ghost_log(
        &GhostTradeLog {
            timestamp: chrono::Utc::now().to_rfc3339(),
            step,
            action: if signal.side == TradeSide::Buy {
                "buy"
            } else {
                "sell"
            }
            .into(),
            asset: asset.clone(),
            price_usd: price,
            quantity: qty,
            trade_value_usdt: value,
            realized_pnl_usdt: pnl,
            balance_atp: wallet.balance_atp,
            cumulative_pnl: wallet.cumulative_pnl,
            reason: reason.into(),
        },
        log_path,
    );
    Some(qty as f64)
}
