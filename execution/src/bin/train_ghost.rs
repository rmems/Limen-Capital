//! Offline training prep for ghost-trade JSONL logs
//!
//! Loads historical JSONL audit logs from metabolic-ledger ghost execution,
//! computes reward signals from realized PnL, and prints a training pipeline
//! summary. Full STDP offline training is owned by Limen-Neural/plasticity-lab
//! (optional; not required for Capital research mode).
//!
//! Usage:
//!   cargo run --bin train_ghost -- ghost_trades.jsonl 100

use std::fs::File;
use std::io::{BufRead, BufReader};
use tracing::{info, warn};

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)] // fields deserialized for future offline training features
struct GhostTradeRecord {
    timestamp: String,
    step: u64,
    action: String,
    asset: String,
    price_usd: f32,
    quantity: f32,
    trade_value_usdt: f32,
    realized_pnl_usdt: f32,
    /// metabolic-ledger field (was balance_usdt in older logs)
    #[serde(alias = "balance_usdt")]
    balance_atp: f32,
    cumulative_pnl: f32,
    reason: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("Spikenaut Ghost Trader - Offline Training");

    // Parse CLI arguments
    let log_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ghost_trades.jsonl".to_string());

    let epochs = std::env::args()
        .nth(2)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100);

    info!("Loading ghost trade logs from: {}", log_path);
    info!("Training epochs: {}", epochs);

    // Load JSONL audit logs
    let trades = load_ghost_trades(&log_path)?;
    info!("Loaded {} ghost trades", trades.len());

    if trades.is_empty() {
        warn!("No trades found in log file. Run the execution engine first to generate training data.");
        return Ok(());
    }

    // Compute reward signals from PnL
    let rewards: Vec<f32> = trades
        .iter()
        .map(|t| compute_reward(t.realized_pnl_usdt))
        .collect();

    info!(
        "Reward stats: min={:.4}, max={:.4}, mean={:.4}",
        rewards.iter().cloned().fold(f32::INFINITY, f32::min),
        rewards.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        rewards.iter().sum::<f32>() / rewards.len() as f32
    );

    // Pipeline outline — full STDP offline training lives in plasticity-lab
    info!("Training pipeline structure:");
    info!("  1. Ghost trades → reward signals (PnL-based)");
    info!("  2. Encode trades as spike trains (asset, price, quantity → spikes)");
    info!("  3. Apply reward-modulated STDP (plasticity-lab + neuromod)");
    info!("  4. Export weights (optional: nir-rs / LiquidCortex)");
    info!("  5. Replay under research_helm metrics");

    for epoch in 0..epochs {
        let mut epoch_reward = 0.0f32;
        let mut epoch_trades = 0;

        for (idx, reward) in rewards.iter().enumerate() {
            epoch_reward += reward;
            epoch_trades += 1;
            if idx % 100 == 0 {
                info!(
                    "  Epoch {}/{}, Trade {}/{}, Avg Reward: {:.4}",
                    epoch + 1,
                    epochs,
                    idx,
                    trades.len(),
                    epoch_reward / (idx + 1) as f32
                );
            }
        }

        let avg_reward = epoch_reward / epoch_trades as f32;
        info!(
            "Epoch {}/{} complete: avg_reward={:.4}, trades={}",
            epoch + 1,
            epochs,
            avg_reward,
            epoch_trades
        );
    }

    info!(
        "Reward prep complete (last ATP sample: {:.2}). Wire plasticity-lab for full STDP.",
        trades.last().map(|t| t.balance_atp).unwrap_or(0.0)
    );

    Ok(())
}

fn load_ghost_trades(path: &str) -> Result<Vec<GhostTradeRecord>, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut trades = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<GhostTradeRecord>(&line) {
            Ok(trade) => trades.push(trade),
            Err(e) => warn!("Failed to parse trade record: {}", e),
        }
    }

    Ok(trades)
}

/// Compute reward signal from realized PnL
/// Positive PnL → dopamine (positive reward)
/// Negative PnL → cortisol (negative reward)
fn compute_reward(pnl: f32) -> f32 {
    // Normalize PnL to [-1.0, 1.0] reward range
    // Using tanh to bound extreme values
    (pnl / 10.0).tanh()
}
