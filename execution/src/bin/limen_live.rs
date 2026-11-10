//! `limen-live` — RM-2436 live micro-capital experiment CLI.
//!
//! ```text
//! limen-live [--config PATH] [--paper] [--once] [--max-ticks N] [--journal PATH]
//!            [--model ID] [--symbol PAIR] [--postmortem]
//! ```
//!
//! Requires `--features live-robinhood`. Secrets come from the environment:
//! `ROBINHOOD_API_KEY`, `ROBINHOOD_PRIVATE_KEY`, `ANTHROPIC_API_KEY`.
//! `--postmortem` needs only the Anthropic key and never enters the tick loop.
//!
//! `--once --paper` runs a single read-only tick: account, pair rules,
//! holdings, bid/ask, decision and risk verdict, with no order submission.

use limen_execution::live::{self, LiveConfig, RunOptions, Secrets};
use std::process::ExitCode;

#[derive(Default)]
struct Args {
    config: Option<String>,
    paper: bool,
    once: bool,
    max_ticks: u64,
    journal: Option<String>,
    model: Option<String>,
    symbol: Option<String>,
    postmortem: bool,
}

const USAGE: &str = "usage: limen-live [--config PATH] [--paper] [--once] [--max-ticks N] [--journal PATH] [--model ID] [--symbol PAIR] [--postmortem]

  --config PATH    TOML config (defaults: BTC-USD, paper, 900 s ticks, rules decider)
  --paper          force paper mode (never submit orders)
  --once           single tick: account, pair rules, bid/ask, decision, risk verdict; no orders
  --max-ticks N    stop after N ticks (default: run until Ctrl-C or the daily-loss halt)
  --journal PATH   override journal_path
  --model ID       override the decider model id
  --symbol PAIR    override the trading pair (BTC-USD | ETH-USD)
  --postmortem     analyze an existing --journal with Sonnet 5.5; no ticks or orders
";

fn parse_args(it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = it;
    let mut tick_limit_given = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--paper" => args.paper = true,
            "--once" => args.once = true,
            "--postmortem" => args.postmortem = true,
            "--max-ticks" => {
                tick_limit_given = true;
                args.set_value(&a, &mut it)?;
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => args.set_value(other, &mut it)?,
        }
    }
    if args.postmortem && (args.once || tick_limit_given || args.model.is_some()) {
        return Err(
            "--postmortem cannot use --once, --max-ticks, or --model (Sonnet 5.5 only)".into(),
        );
    }
    Ok(args)
}

impl Args {
    fn set_value(
        &mut self,
        flag: &str,
        it: &mut impl Iterator<Item = String>,
    ) -> Result<(), String> {
        let description = match flag {
            "--config" | "--journal" => "a path",
            "--model" => "an id",
            "--symbol" => "a pair",
            "--max-ticks" => "a number",
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        };
        let value = it
            .next()
            .ok_or_else(|| format!("{flag} needs {description}"))?;
        self.assign_value(flag, value)
    }

    fn assign_value(&mut self, flag: &str, value: String) -> Result<(), String> {
        match flag {
            "--config" => self.config = Some(value),
            "--journal" => self.journal = Some(value),
            "--model" => self.model = Some(value),
            "--symbol" => self.symbol = Some(value),
            "--max-ticks" => {
                self.max_ticks = value
                    .parse()
                    .map_err(|_| format!("--max-ticks: not a number: {value:?}"))?;
            }
            _ => unreachable!("flag validated by set_value"),
        }
        Ok(())
    }
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

    // Arguments configure the paper run only; they never carry credentials
    // or drive authorization decisions.
    // nosemgrep
    let argv = std::env::args_os()
        .skip(1)
        .map(|a| a.into_string())
        .collect::<Result<Vec<_>, _>>();
    let args = match argv
        .map_err(|_| "arguments must be valid Unicode".to_string())
        .and_then(|a| parse_args(a.into_iter()))
    {
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
    if args.postmortem {
        return match live::postmortem::run(&cfg, &Secrets::from_env()).await {
            Ok(path) => {
                println!("post-mortem: {}", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("limen-live post-mortem: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if !cfg.paper {
        // Belt and braces: live submission is not wired yet, and when it is,
        // it must be an explicit config choice (`paper = false`), never a default.
        eprintln!("limen-live: live mode is not available yet; pass --paper");
        return ExitCode::from(2);
    }
    execute(&cfg, &args).await
}

async fn execute(cfg: &LiveConfig, args: &Args) -> ExitCode {
    let secrets = Secrets::from_env();
    let opts = RunOptions {
        once: args.once,
        max_ticks: args.max_ticks,
    };
    match live::run(cfg, &secrets, &opts).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(values: &[&str]) -> Result<Args, String> {
        parse_args(values.iter().map(|v| v.to_string()))
    }

    #[test]
    fn accepts_flags_values_and_last_override() {
        let args = parse(&[
            "--config",
            "run.toml",
            "--paper",
            "--once",
            "--max-ticks",
            "7",
            "--journal",
            "paper.md",
            "--model",
            "model-id",
            "--symbol",
            "BTC-USD",
            "--symbol",
            "eth-usd",
        ])
        .unwrap();
        assert_eq!(args.config.as_deref(), Some("run.toml"));
        assert!(args.paper);
        assert!(args.once);
        assert_eq!(args.max_ticks, 7);
        assert_eq!(args.journal.as_deref(), Some("paper.md"));
        assert_eq!(args.model.as_deref(), Some("model-id"));
        assert_eq!(args.symbol.as_deref(), Some("eth-usd"));
    }

    #[test]
    fn preserves_defaults_and_validates_overrides() {
        let args = parse(&[]).unwrap();
        assert_eq!(args.max_ticks, 0);
        assert!(!args.paper);
        assert!(!args.once);
        assert_eq!(load_config(&args).unwrap(), LiveConfig::default());
        let args = parse(&["--symbol", "eth-usd", "--journal", "paper.md"]).unwrap();
        let cfg = load_config(&args).unwrap();
        assert_eq!(cfg.symbol, "ETH-USD");
        assert_eq!(cfg.journal_path, "paper.md");
        let args = parse(&["--symbol", "DOGE-USD"]).unwrap();
        assert!(load_config(&args).is_err());
    }

    #[test]
    fn postmortem_is_explicit_and_cannot_override_model_or_run_ticks() {
        assert!(
            parse(&["--postmortem", "--journal", "fixture.md"])
                .unwrap()
                .postmortem
        );
        for flags in [
            vec!["--postmortem", "--model", "claude-haiku-5-5"],
            vec!["--postmortem", "--once"],
            vec!["--postmortem", "--max-ticks", "1"],
            vec!["--max-ticks", "0", "--postmortem"],
        ] {
            assert!(parse(&flags).is_err());
        }
    }

    #[test]
    fn rejects_missing_values_invalid_numbers_and_unknown_flags() {
        for flag in [
            "--config",
            "--journal",
            "--model",
            "--symbol",
            "--max-ticks",
        ] {
            let error = parse(&[flag]).err().unwrap();
            assert!(error.starts_with(&format!("{flag} needs ")));
        }
        for value in ["-1", "1.5", "many", "18446744073709551616"] {
            let error = parse(&["--max-ticks", value]).err().unwrap();
            assert!(error.starts_with("--max-ticks: not a number:"));
        }
        assert!(parse(&["--unknown"])
            .err()
            .unwrap()
            .contains("unknown argument"));
        assert_eq!(parse(&["--help"]).err().unwrap(), USAGE);
        assert_eq!(parse(&["-h"]).err().unwrap(), USAGE);
    }
}
