//! Halt file, instance lock and exit cancels: every way out leaves nothing
//! resting.
use super::*;

/// Warm up, dip (buy rests at 99.0), then asks that never reach the limit.
const RESTING_BUY: &[(f64, f64)] = &[
    (100.0, 100.1),
    (100.0, 100.1),
    (100.0, 100.1),
    (99.0, 99.1),
    (99.5, 99.6),
    (99.5, 99.6),
];

fn resting_cfg(p: &std::path::Path) -> LiveConfig {
    let mut c = cfg(p, 1);
    c.risk.min_tick_interval_secs = 1;
    c.risk.stale_order_ticks = 50;
    c.rules.entry_bps = 1.0;
    c
}

#[tokio::test]
async fn exit_cancels_the_resting_order() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("exit.md");
    let c = resting_cfg(&p);
    let parts = scripted_parts(&c, RESTING_BUY, 10.0).await;
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
    assert_eq!((s.orders_placed, s.fills, s.cancels), (1, 0, 1), "{s:?}");
    assert_eq!(s.halted_reason, None);
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("stop:exit"), "{text}");
    assert!(text.contains("paper-1:canceled"), "{text}");
}

/// Scripted quotes; touches the halt file when the given quote is served.
struct HaltingMarket {
    quotes: std::collections::VecDeque<(f64, f64)>,
    halt_at: usize,
    served: usize,
    halt: std::path::PathBuf,
}

impl MarketSource for HaltingMarket {
    fn label(&self) -> String {
        "halting".into()
    }
    fn quote<'a>(&'a mut self, symbol: &'a str) -> crate::live::market::QuoteFuture<'a> {
        self.served += 1;
        if self.served == self.halt_at {
            std::fs::write(&self.halt, "").unwrap();
        }
        // Once the halt file exists the served quote crosses the resting buy:
        // a halted tick that still processed it would fill the order.
        let (bid, ask) = if self.served >= self.halt_at {
            (98.0, 98.1)
        } else {
            self.quotes.pop_front().unwrap_or((99.5, 99.6))
        };
        let q = crate::live::alpaca::types::BestBidAsk {
            symbol: symbol.to_string(),
            bid,
            ask,
        };
        Box::pin(async move { Ok(q) })
    }
}

#[tokio::test]
async fn halt_file_cancels_resting_order_stops_and_blocks_restart() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("halt-file.md");
    let c = resting_cfg(&p);
    let mut parts = scripted_parts(&c, &[], 10.0).await;
    parts.market = Box::new(HaltingMarket {
        quotes: RESTING_BUY.iter().copied().collect(),
        halt_at: 5,
        served: 0,
        halt: c.halt_path(),
    });
    let s = run_with_parts(
        &c,
        &RunOptions {
            once: false,
            max_ticks: 20,
        },
        parts,
    )
    .await
    .unwrap();
    assert_eq!(s.ticks, 5, "{s:?}");
    assert_eq!(s.halted_reason.as_deref(), Some("halt_file"));
    // The crossing quote served at the halted tick fills nothing.
    assert_eq!((s.orders_placed, s.fills, s.cancels), (1, 0, 1), "{s:?}");
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("stop:halt_file"), "{text}");
    let err = build_parts(&c, &Secrets::default()).await.unwrap_err();
    assert!(err.to_string().contains("halt file"), "{err}");
    std::fs::remove_file(c.halt_path()).unwrap();
    build_parts(&c, &Secrets::default()).await.unwrap();
}

#[tokio::test]
async fn daily_loss_halt_cancels_the_resting_order() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("loss-halt.md");
    let mut c = resting_cfg(&p);
    c.risk.max_daily_loss_usd = 0.5;
    let mut parts = scripted_parts(&c, &[(99.5, 99.6)], 10.0).await;
    let order = crate::live::risk::ApprovedOrder {
        client_order_id: "resting".into(),
        symbol: c.symbol.clone(),
        side: crate::live::alpaca::types::OrderSide::Buy,
        limit_price: 90.0,
        asset_qty: 0.01,
        notional_usd: 0.9,
        limit_price_str: "90.00".into(),
        asset_qty_str: "0.01".into(),
    };
    parts.book.place(&order, 0);
    parts.book.realized_pnl_usd = -0.6;
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
    assert_eq!(s.halted_reason.as_deref(), Some("daily_loss_breached"));
    assert_eq!(s.cancels, 1, "{s:?}");
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("stop:daily_loss_breached"), "{text}");
}

#[tokio::test]
async fn second_instance_on_the_same_state_dir_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("lock.md");
    let c = cfg(&p, 1);
    let first = build_parts(&c, &Secrets::default()).await.unwrap();
    let err = build_parts(&c, &Secrets::default()).await.unwrap_err();
    assert!(
        err.to_string().contains("another limen-live instance"),
        "{err}"
    );
    drop(first);
    build_parts(&c, &Secrets::default()).await.unwrap();
}
