use super::*;
use crate::live::kill::HaltFile;
use crate::live::risk::{self, RiskInputs, RiskLimits, RiskReject};
use crate::live::robinhood::types::OrderSide;
use crate::live::venue::mock::{MockVenue, PostFault};
use crate::live::venue::VenueOrderStatus;
use std::path::Path;

/// 2026-11-10T14:00:00Z
const T0: i64 = 1_794_405_600;
const SYMBOL: &str = "SPY";

fn book_cfg(dir: &Path) -> LiveBookConfig {
    LiveBookConfig {
        symbol: SYMBOL.into(),
        state_path: dir.join("live-state.json"),
        ledger_path: dir.join("ledger-audit.jsonl"),
        fee_ratio: 0.001,
        unconfirmed_grace_secs: 120,
    }
}

fn order(cid: &str, side: OrderSide, qty: f64, limit: f64) -> ApprovedOrder {
    ApprovedOrder {
        client_order_id: cid.into(),
        symbol: SYMBOL.into(),
        side,
        limit_price: limit,
        asset_qty: qty,
        notional_usd: qty * limit,
        limit_price_str: format!("{limit:.2}"),
        asset_qty_str: format!("{qty}"),
    }
}

async fn open(venue: &MockVenue, dir: &Path, now: i64) -> Result<LiveBook, BookError> {
    LiveBook::open(Box::new(venue.clone()), book_cfg(dir), now).await
}

fn ledger_rows(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("ledger-audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn refused(r: Result<LiveBook, BookError>) -> String {
    match r {
        Err(BookError::Refused(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Round trip: buy 0.05 @ 100, sell 0.05 @ 90, fee 0.1% → realized −0.5095.
async fn losing_round_trip(book: &mut LiveBook, venue: &MockVenue, now: i64) {
    book.place(&order("buy-1", OrderSide::Buy, 0.05, 100.0), now)
        .await
        .unwrap();
    venue.fill("buy-1", 0.05, 100.0);
    book.poll(now + 1).await.unwrap();
    book.place(&order("sell-1", OrderSide::Sell, 0.05, 90.0), now + 2)
        .await
        .unwrap();
    venue.fill("sell-1", 0.05, 90.0);
    book.poll(now + 3).await.unwrap();
}

/// The issue's acceptance scenario, end to end against the mock venue.
#[tokio::test]
async fn kill_and_restart_mid_order_adopts_one_order_keeps_baseline_and_halts() {
    let dir = tempfile::tempdir().unwrap();
    let venue = MockVenue::new("ACC-1", 100.0, 0.001);

    // Session 1: trade a losing round trip, then submit a buy whose POST
    // response is lost and whose immediate lookup also times out, and die.
    let mut book = open(&venue, dir.path(), T0).await.unwrap();
    assert_eq!(book.day_start_realized_usd(), 0.0);
    losing_round_trip(&mut book, &venue, T0).await;
    // Hand computation: cost 0.05×100 + 0.005 fee = 5.005; proceeds 4.5;
    // sell fee 0.0045 → realized 4.5 − 5.005 − 0.0045 = −0.5095.
    let hand = 0.05 * 90.0 - (0.05 * 100.0 + 0.005) - 0.0045;
    assert!((book.realized_pnl_usd() - hand).abs() < 1e-12);
    assert!((book.daily_pnl_usd() - hand).abs() < 1e-12);
    venue.push_post_fault(PostFault::LoseResponse);
    venue.fail_lookups(1);
    let outcome = book
        .place(&order("buy-2", OrderSide::Buy, 0.02, 95.0), T0 + 10)
        .await
        .unwrap();
    assert_eq!(outcome, PlaceOutcome::Unconfirmed);
    drop(book); // kill -9: no cleanup runs

    // Session 2 (same UTC day): one venue order for the intent, adopted.
    let mut book = open(&venue, dir.path(), T0 + 60).await.unwrap();
    assert_eq!(venue.post_count(), 3, "no POST is resent on restart");
    let buy2: Vec<_> = venue
        .orders()
        .into_iter()
        .filter(|o| o.client_order_id.as_deref() == Some("buy-2"))
        .collect();
    assert_eq!(buy2.len(), 1);
    assert_eq!(book.open_orders().len(), 1);
    let tracked = &book.open_orders()[0];
    assert_eq!(tracked.client_order_id, "buy-2");
    assert_eq!(tracked.venue_order_id.as_deref(), Some(buy2[0].id.as_str()));
    assert_eq!(tracked.status, IntentStatus::Acknowledged);
    // Same daily-loss baseline: the restart did not reset the counter.
    assert_eq!(book.day_start_realized_usd(), 0.0);
    assert!((book.daily_pnl_usd() - hand).abs() < 1e-12);
    assert_eq!(book.last_order_secs(), Some(T0 + 10));

    // The daily-loss gate halts the day; the halt cancels the resting order.
    let limits = RiskLimits {
        max_daily_loss_usd: 0.5,
        ..RiskLimits::default()
    };
    let inputs = RiskInputs {
        now_secs: T0 + 61,
        last_order_secs: book.last_order_secs(),
        open_orders: book.open_orders().len(),
        daily_pnl_usd: book.daily_pnl_usd(),
        llm_cost_today_usd: book.llm_spend_today_usd(),
        resting_notional_usd: book.resting_notional_usd(),
    };
    let reason = risk::pre_tick_check(&limits, &inputs).unwrap();
    assert!(matches!(reason, RiskReject::DailyLossBreached { .. }));
    assert!(reason.halts_day());
    assert_eq!(book.cancel_all(T0 + 61).await.unwrap(), 1);
    assert_eq!(venue.delete_count(), 1);
    let buy2 = venue
        .orders()
        .into_iter()
        .find(|o| o.client_order_id.as_deref() == Some("buy-2"))
        .unwrap();
    assert_eq!(buy2.status, VenueOrderStatus::Canceled);
    assert!(book.open_orders().is_empty());

    // Ledger audit: one row per fill, never duplicated by the restart, and
    // the sell row's realized PnL is the hand computation.
    let rows = ledger_rows(dir.path());
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["action"], "buy");
    assert_eq!(rows[1]["action"], "sell");
    let realized = rows[1]["realized_pnl_usdt"].as_f64().unwrap();
    assert!((realized - hand).abs() < 1e-5, "{realized} vs {hand}");
    assert!(rows[1]["reason"]
        .as_str()
        .unwrap()
        .contains("realized_usd=-0.50950000"));
    // Cash is the venue's, not a seeded paper balance.
    assert!((book.cash_usd() - venue.cash()).abs() < 1e-12);
    assert!((book.cash_usd() - (100.0 + hand)).abs() < 1e-9);
}

#[tokio::test]
async fn lost_response_with_working_lookup_adopts_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let venue = MockVenue::new("ACC-1", 100.0, 0.0);
    let mut book = open(&venue, dir.path(), T0).await.unwrap();
    venue.push_post_fault(PostFault::LoseResponse);
    let outcome = book
        .place(&order("cid-1", OrderSide::Buy, 0.01, 100.0), T0)
        .await
        .unwrap();
    assert!(
        matches!(outcome, PlaceOutcome::Adopted(ref o) if o.id == "mock-1"),
        "{outcome:?}"
    );
    assert_eq!(venue.post_count(), 1);
    assert_eq!(
        book.open_orders()[0].venue_order_id.as_deref(),
        Some("mock-1")
    );
    // The same intent can never be submitted again.
    let err = book
        .place(&order("cid-1", OrderSide::Buy, 0.01, 100.0), T0 + 1)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already submitted"), "{err}");
    assert_eq!(venue.post_count(), 1);
}

#[tokio::test]
async fn rejected_post_is_dropped_and_dropped_request_expires_after_grace() {
    let dir = tempfile::tempdir().unwrap();
    let venue = MockVenue::new("ACC-1", 100.0, 0.0);
    let mut book = open(&venue, dir.path(), T0).await.unwrap();
    venue.push_post_fault(PostFault::Reject {
        status: 403,
        body: "insufficient buying power".into(),
    });
    let outcome = book
        .place(&order("rej", OrderSide::Buy, 0.01, 100.0), T0)
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        PlaceOutcome::Rejected(VenueError::Rejected { status: 403, .. })
    ));
    assert!(book.open_orders().is_empty());

    venue.push_post_fault(PostFault::DropRequest);
    let outcome = book
        .place(&order("lost", OrderSide::Buy, 0.01, 100.0), T0 + 1)
        .await
        .unwrap();
    assert_eq!(outcome, PlaceOutcome::Unconfirmed);
    book.poll(T0 + 30).await.unwrap();
    assert_eq!(book.open_orders().len(), 1, "kept within the grace period");
    book.poll(T0 + 1 + 120).await.unwrap();
    assert!(book.open_orders().is_empty(), "never reached the venue");
    assert_eq!(venue.post_count(), 2, "nothing was resent");
}

#[tokio::test]
async fn partial_fills_sum_and_expiry_is_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let venue = MockVenue::new("ACC-1", 100.0, 0.001);
    let mut book = open(&venue, dir.path(), T0).await.unwrap();
    book.place(&order("p", OrderSide::Buy, 0.04, 100.0), T0)
        .await
        .unwrap();
    venue.fill("p", 0.01, 100.0);
    assert_eq!(book.poll(T0 + 1).await.unwrap().len(), 1);
    venue.fill("p", 0.02, 98.0);
    let fills = book.poll(T0 + 2).await.unwrap();
    assert_eq!(fills.len(), 1);
    assert!((fills[0].qty - 0.02).abs() < 1e-12);
    assert!((fills[0].price - 98.0).abs() < 1e-9, "incremental price");
    assert!(
        book.poll(T0 + 3).await.unwrap().is_empty(),
        "no double count"
    );
    assert_eq!(book.open_orders().len(), 1);
    venue.set_status("p", VenueOrderStatus::Expired);
    book.poll(T0 + 4).await.unwrap();
    assert!(book.open_orders().is_empty());
    assert!((book.position_qty() - 0.03).abs() < 1e-12);
    // (0.01 × 100 + 0.02 × 98) × 1.001 fee / 0.03
    let avg = 2.96 * 1.001 / 0.03;
    assert!((book.avg_cost_usd().unwrap() - avg).abs() < 1e-9);
    // Restart rebuilds the same cost basis from the fill log.
    drop(book);
    let book = open(&venue, dir.path(), T0 + 5).await.unwrap();
    assert!((book.avg_cost_usd().unwrap() - avg).abs() < 1e-9);
    assert_eq!(ledger_rows(dir.path()).len(), 2);
}

#[tokio::test]
async fn fills_while_down_are_adopted_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let venue = MockVenue::new("ACC-1", 100.0, 0.0);
    let mut book = open(&venue, dir.path(), T0).await.unwrap();
    book.place(&order("f", OrderSide::Buy, 0.01, 100.0), T0)
        .await
        .unwrap();
    drop(book);
    venue.fill("f", 0.01, 100.0);
    let book = open(&venue, dir.path(), T0 + 5).await.unwrap();
    assert!(book.open_orders().is_empty());
    assert!((book.position_qty() - 0.01).abs() < 1e-12);
    assert_eq!(book.fills().len(), 1);
}

#[tokio::test]
async fn unexplained_holding_foreign_orders_and_other_accounts_refuse_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let venue = MockVenue::new("ACC-1", 100.0, 0.0);
    venue.set_position(SYMBOL, 0.5);
    let msg = refused(open(&venue, dir.path(), T0).await);
    assert!(msg.contains("fill log explains 0"), "{msg}");
    venue.set_position(SYMBOL, 0.0);

    venue.insert_foreign_order(VenueOrder {
        id: "manual".into(),
        client_order_id: Some("someone-else".into()),
        symbol: SYMBOL.into(),
        side: OrderSide::Buy,
        qty: 1.0,
        limit_price: Some(1.0),
        status: VenueOrderStatus::New,
        filled_qty: 0.0,
        filled_avg_price: None,
    });
    let msg = refused(open(&venue, dir.path(), T0).await);
    assert!(msg.contains("not placed by this book"), "{msg}");

    let other = tempfile::tempdir().unwrap();
    open(&MockVenue::new("ACC-1", 1.0, 0.0), other.path(), T0)
        .await
        .unwrap();
    let msg = refused(open(&MockVenue::new("ACC-2", 1.0, 0.0), other.path(), T0).await);
    assert!(msg.contains("belongs to account ACC-1"), "{msg}");
}

#[tokio::test]
async fn only_a_new_utc_day_resets_the_daily_counters() {
    let dir = tempfile::tempdir().unwrap();
    let venue = MockVenue::new("ACC-1", 100.0, 0.001);
    let mut book = open(&venue, dir.path(), T0).await.unwrap();
    losing_round_trip(&mut book, &venue, T0).await;
    book.record_llm_spend(0.3).unwrap();
    drop(book);
    let book = open(&venue, dir.path(), T0 + 3_600).await.unwrap();
    assert_eq!(book.day_start_realized_usd(), 0.0);
    assert_eq!(book.llm_spend_today_usd(), 0.3);
    assert!(book.last_order_secs().is_some());
    drop(book);
    let book = open(&venue, dir.path(), T0 + 86_400).await.unwrap();
    assert!((book.day_start_realized_usd() - book.realized_pnl_usd()).abs() < 1e-12);
    assert_eq!(book.daily_pnl_usd(), 0.0);
    assert_eq!(book.llm_spend_today_usd(), 0.0);
    assert_eq!(book.last_order_secs(), None);
}

#[tokio::test]
async fn halt_file_is_independent_of_the_book() {
    // The halt switch lives in the state dir next to the book's files.
    let dir = tempfile::tempdir().unwrap();
    let halt = HaltFile::new(dir.path().join("HALT"));
    let venue = MockVenue::new("ACC-1", 100.0, 0.0);
    let mut book = open(&venue, dir.path(), T0).await.unwrap();
    book.place(&order("h", OrderSide::Buy, 0.01, 100.0), T0)
        .await
        .unwrap();
    std::fs::write(halt.path(), "").unwrap();
    assert!(halt.is_set());
    assert_eq!(book.cancel_all(T0 + 1).await.unwrap(), 1);
    assert!(book.open_orders().is_empty());
    // Cancelling again sends nothing.
    assert_eq!(book.cancel_all(T0 + 2).await.unwrap(), 0);
    assert_eq!(venue.delete_count(), 1);
}
