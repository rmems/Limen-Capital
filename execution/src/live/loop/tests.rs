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

/// One successful quote followed by errors, to cover the early-return path.
struct FailingMarket(bool);

impl MarketSource for FailingMarket {
    fn label(&self) -> String {
        "failing".into()
    }

    fn quote<'a>(&'a mut self, symbol: &'a str) -> crate::live::market::QuoteFuture<'a> {
        let first = std::mem::replace(&mut self.0, false);
        Box::pin(async move {
            if first {
                Ok(crate::live::robinhood::types::BestBidAsk {
                    symbol: symbol.into(),
                    bid: 100.0,
                    ask: 100.1,
                })
            } else {
                Err(crate::live::robinhood::ClientError::NotFound(
                    "quote".into(),
                ))
            }
        })
    }
}

#[tokio::test]
async fn final_quote_failure_preserves_account_summary_and_last_mark() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("quote-error.md");
    let c = cfg(&p, 1);
    let mut parts = build_parts(&c, &Secrets::default()).await.unwrap();
    parts.market = Box::new(FailingMarket(true));
    parts.book.realized_pnl_usd = -0.2;
    parts.book.fees_paid_usd = 0.1;
    let summary = run_with_parts(
        &c,
        &RunOptions {
            once: false,
            max_ticks: 2,
        },
        parts,
    )
    .await
    .unwrap();
    assert_eq!(summary.ticks, 2);
    assert_eq!(summary.realized_pnl_usd, -0.2);
    assert_eq!(summary.fees_paid_usd, 0.1);
    assert_eq!(summary.final_equity_usd, c.paper_starting_cash_usd);
    assert_eq!(summary.halted_reason, None);
    assert_eq!(Journal::new(&p).row_count().unwrap(), 1);
}

#[tokio::test]
async fn once_quote_failure_stops_without_a_journal_row() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("once-error.md");
    let c = cfg(&p, 1);
    let mut parts = build_parts(&c, &Secrets::default()).await.unwrap();
    parts.market = Box::new(FailingMarket(false));
    parts.book.realized_pnl_usd = -0.2;
    let summary = run_with_parts(
        &c,
        &RunOptions {
            once: true,
            max_ticks: 0,
        },
        parts,
    )
    .await
    .unwrap();
    assert_eq!(summary.ticks, 1);
    assert_eq!(summary.realized_pnl_usd, -0.2);
    assert_eq!(Journal::new(&p).row_count().unwrap(), 0);
}

struct ExhaustedDecider;

impl Decider for ExhaustedDecider {
    fn name(&self) -> &str {
        "exhausted"
    }

    fn decide<'a>(
        &'a mut self,
        _state: &'a MarketState,
        _recent: &'a [String],
    ) -> crate::live::decider::DecideFuture<'a> {
        panic!("pre-tick budget check must skip the decider")
    }

    fn llm_cost_today_usd(&self) -> f64 {
        1.0
    }
}

#[tokio::test]
async fn exhausted_budget_idles_without_halting_or_calling_decider() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("budget.md");
    let c = cfg(&p, 1);
    let mut parts = build_parts(&c, &Secrets::default()).await.unwrap();
    parts.decider = Box::new(ExhaustedDecider);
    let summary = run_with_parts(
        &c,
        &RunOptions {
            once: false,
            max_ticks: 2,
        },
        parts,
    )
    .await
    .unwrap();
    assert_eq!(summary.ticks, 2);
    assert_eq!(summary.orders_placed, 0);
    assert_eq!(summary.halted_reason, None);
    let text = std::fs::read_to_string(&p).unwrap();
    assert_eq!(text.matches("rejected:llm_budget_exhausted").count(), 2);
}

#[tokio::test]
async fn bad_quote_touches_nothing_and_stays_out_of_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("badquote.md");
    let mut c = cfg(&p, 1);
    c.risk.min_tick_interval_secs = 1;
    c.rules.entry_bps = 1.0;
    // tick 1 sane, tick 2 crossed, tick 3 sane: tick 3 must say warming_up 2/3.
    let parts = scripted_parts(&c, &[(100.0, 100.1), (100.2, 100.0), (100.0, 100.1)], 10.0).await;
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
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("fee.md");
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
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("refuse.md");
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
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("secrets.md");
    let mut c = cfg(&p, 1);
    c.market = MarketSourceConfig::Robinhood;
    let err = build_parts(&c, &Secrets::default()).await.unwrap_err();
    assert!(err.to_string().contains("ROBINHOOD_API_KEY"), "{err}");
}

#[tokio::test]
async fn once_writes_header_and_one_row() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("once.md");
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
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("trades.md");
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
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("halt.md");
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
