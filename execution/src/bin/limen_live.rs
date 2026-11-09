//! `limen-live` — RM-2436 live micro-capital experiment CLI.
//!
//! ```text
//! limen-live [--config PATH] [--paper] [--once] [--model ID] [--symbol PAIR]
//! ```
//!
//! Requires `--features live-robinhood`. Secrets come from the environment:
//! `ROBINHOOD_API_KEY`, `ROBINHOOD_PRIVATE_KEY` (and later `ANTHROPIC_API_KEY`).
//!
//! `--once --paper` is the read-only smoke test: prints account, trading-pair
//! rules, holdings and bid/ask, and submits nothing.

use spikenaut_execution_engine::live::config::MarketSourceConfig;
use spikenaut_execution_engine::live::{LiveConfig, RobinhoodClient, Secrets};
use std::process::ExitCode;

struct Args {
    config: Option<String>,
    paper: bool,
    once: bool,
    model: Option<String>,
    symbol: Option<String>,
}

const USAGE: &str =
    "usage: limen-live [--config PATH] [--paper] [--once] [--model ID] [--symbol PAIR]

  --config PATH   TOML config (defaults: BTC-USD, paper, 900 s ticks)
  --paper         force paper mode (never submit orders)
  --once          single read-only tick: account, pair rules, holdings, bid/ask
  --model ID      override the decider model id
  --symbol PAIR   override the trading pair (BTC-USD | ETH-USD)
";

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        config: None,
        paper: false,
        once: false,
        model: None,
        symbol: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => args.config = Some(it.next().ok_or("--config needs a path")?),
            "--paper" => args.paper = true,
            "--once" => args.once = true,
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
    cfg.validate()?;
    Ok(cfg)
}

async fn smoke(cfg: &LiveConfig, secrets: &Secrets) -> Result<(), Box<dyn std::error::Error>> {
    let (api_key, priv_key) = secrets.robinhood()?;
    let mut client = RobinhoodClient::new(api_key, priv_key, &cfg.base_url)?;
    println!(
        "mode: {}  (no orders are submitted by --once)",
        if cfg.paper { "paper" } else { "LIVE" }
    );
    println!("venue: {}", client.base_url());
    println!(
        "public key (compare with the one registered at Robinhood): {}",
        client.public_key_b64()
    );

    let account = client.get_account().await?;
    println!(
        "account {}  status={}  buying_power={:.2} {}  api_tradable={:?}",
        account.account_number,
        account.status,
        account.buying_power,
        account.buying_power_currency,
        account.is_api_tradable
    );
    if let Some(fee) = &account.fee_tier_status {
        println!(
            "fee tier: ratio={:?}  30d_volume={:?}  next_ratio={:?} at {:?}",
            fee.fee_ratio,
            fee.thirty_day_volume,
            fee.next_fee_tier_ratio,
            fee.next_fee_tier_threshold
        );
    }

    let pair = client.trading_pair(&cfg.symbol).await?;
    println!(
        "pair {}  status={}  api_tradable={}  asset_increment={}  quote_increment={}  min_order_amount={:?}  max_order_size={:?}",
        pair.symbol,
        pair.status,
        pair.is_api_tradable,
        pair.asset_increment,
        pair.quote_increment,
        pair.min_order_amount,
        pair.max_order_size
    );

    match client.holding(cfg.asset_code()).await? {
        Some(h) => println!(
            "holding {}  total={}  available={}",
            h.asset_code, h.total_quantity, h.quantity_available_for_trading
        ),
        None => println!("holding {}  none", cfg.asset_code()),
    }

    let quote = client.best_bid_ask(&cfg.symbol).await?;
    println!(
        "quote {}  bid={:.2}  ask={:.2}  mid={:.2}  spread={:.2} bps  sane={}",
        quote.symbol,
        quote.bid,
        quote.ask,
        quote.mid(),
        quote.spread_bps(),
        quote.is_sane()
    );
    Ok(())
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
    let secrets = Secrets::from_env();

    if !args.once {
        eprintln!("limen-live: the decision loop is not wired yet; run with --once --paper for the read-only smoke test");
        return ExitCode::from(2);
    }
    if matches!(cfg.market, MarketSourceConfig::Synthetic { .. }) {
        eprintln!("limen-live: --once smoke test needs the robinhood market source");
        return ExitCode::from(2);
    }
    match smoke(&cfg, &secrets).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("limen-live: {e}");
            ExitCode::FAILURE
        }
    }
}
