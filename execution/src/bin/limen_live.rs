//! `limen-live` — RM-2436 live micro-capital experiment CLI.
//!
//! ```text
//! limen-live [--config PATH] [--paper] [--once] [--max-ticks N] [--journal PATH]
//!            [--model ID] [--symbol PAIR]
//! ```
//!
//! Requires `--features live-robinhood`. Secrets come from the environment:
//! `ROBINHOOD_API_KEY`, `ROBINHOOD_PRIVATE_KEY` (and later `ANTHROPIC_API_KEY`).
//!
//! `--once --paper` runs a single read-only tick: account, pair rules,
//! holdings, bid/ask, decision and risk verdict, with no order submission.

use spikenaut_execution_engine::live::{self, LiveConfig, RunOptions, Secrets};
use std::process::ExitCode;

struct Args {
    config: Option<String>,
    paper: bool,
    once: bool,
    max_ticks: u64,
    journal: Option<String>,
    model: Option<String>,
    symbol: Option<String>,
}

const USAGE: &str = "usage: limen-live [--config PATH] [--paper] [--once] [--max-ticks N] [--journal PATH] [--model ID] [--symbol PAIR]

  --config PATH    TOML config (defaults: BTC-USD, paper, 900 s ticks, rules decider)
  --paper          force paper mode (never submit orders)
  --once           single tick: account, pair rules, bid/ask, decision, risk verdict; no orders
  --max-ticks N    stop after N ticks (default: run until Ctrl-C or the daily-loss halt)
  --journal PATH   override journal_path
  --model ID       override the decider model id
  --symbol PAIR    override the trading pair (BTC-USD | ETH-USD)
";

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        config: None,
        paper: false,
        once: false,
        max_ticks: 0,
        journal: None,
        model: None,
        symbol: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => args.config = Some(it.next().ok_or("--config needs a path")?),
            "--paper" => args.paper = true,
            "--once" => args.once = true,
            "--max-ticks" => {
                let v = it.next().ok_or("--max-ticks needs a number")?;
                args.max_ticks = v
                    .parse()
                    .map_err(|_| format!("--max-ticks: not a number: {v:?}"))?;
            }
            "--journal" => args.journal = Some(it.next().ok_or("--journal needs a path")?),
            "--model" => args.model = Some(it.next().ok_or("--model needs an id")?),
            "--symbol" => args.symbol = Some(it.next().ok_or("--symbol needs a pair")?),
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
    }
    Ok(args)
}

fn load_config(args: &Args) -> Result<LiveConfig, Box<dyn std::error::Error>> {
    let mut cfg = match &args.config {
        Some(p) => LiveConfig::load(p)?,
        None => LiveConfig::default(),
    };
    if args.paper {
        cfg.paper = true;
    }
    if let Some(m) = &args.model {
        cfg.model = m.clone();
    }
    if let Some(s) = &args.symbol {
        cfg.symbol = s.to_ascii_uppercase();
    }
    if let Some(j) = &args.journal {
        cfg.journal_path = j.clone();
    }
    cfg.validate()?;
    Ok(cfg)
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    let cfg = match load_config(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("limen-live: {e}");
            return ExitCode::from(2);
        }
    };
    if !cfg.paper {
        // Belt and braces: live submission is not wired yet, and when it is,
        // it must be an explicit config choice (`paper = false`), never a default.
        eprintln!("limen-live: live mode is not available yet; pass --paper");
        return ExitCode::from(2);
    }
    let secrets = Secrets::from_env();
    let opts = RunOptions {
        once: args.once,
        max_ticks: args.max_ticks,
    };
    match live::run(&cfg, &secrets, &opts).await {
        Ok(summary) => {
            println!(
                "done: ticks={} orders={} fills={} cancels={} rejects={} realized=${:+.4} fees=${:.4} equity=${:.4}{}",
                summary.ticks,
                summary.orders_placed,
                summary.fills,
                summary.cancels,
                summary.rejects,
                summary.realized_pnl_usd,
                summary.fees_paid_usd,
                summary.final_equity_usd,
                summary
                    .halted_reason
                    .as_ref()
                    .map(|r| format!(" halted={r}"))
                    .unwrap_or_default()
            );
            println!("journal: {}", cfg.journal_path);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("limen-live: {e}");
            ExitCode::FAILURE
        }
    }
}
