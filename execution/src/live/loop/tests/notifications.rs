use super::*;
use crate::live::notify::{Notifier, NotifyFuture};
use crate::live::paper::PaperOrder;
use crate::live::robinhood::types::{OrderSide, OrderState};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct RecordingNotifier {
    messages: Arc<Mutex<Vec<(String, String)>>>,
    fail: bool,
}

impl Notifier for RecordingNotifier {
    fn notify<'a>(&'a self, title: &'a str, body: &'a str) -> NotifyFuture<'a> {
        Box::pin(async move {
            self.messages
                .lock()
                .unwrap()
                .push((title.into(), body.into()));
            if self.fail {
                Err(std::io::Error::other("desktop unavailable"))
            } else {
                Ok(())
            }
        })
    }
}

struct ErrorDecider;

impl Decider for ErrorDecider {
    fn name(&self) -> &str {
        "error"
    }

    fn decide<'a>(
        &'a mut self,
        _: &'a MarketState,
        _: &'a [String],
    ) -> crate::live::decider::DecideFuture<'a> {
        Box::pin(async {
            Err(crate::live::decider::DeciderError::Other(
                "test failure".into(),
            ))
        })
    }
}

fn resting(id: &str, price: f64) -> PaperOrder {
    PaperOrder {
        id: id.into(),
        client_order_id: id.into(),
        side: OrderSide::Buy,
        limit_price: price,
        asset_qty: 0.01,
        placed_tick: 0,
        state: OrderState::Open,
    }
}

#[tokio::test]
async fn each_fill_and_cancel_notifies_once_even_on_the_same_tick() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("events.md");
    let mut c = cfg(&p, 1);
    c.risk.stale_order_ticks = 1;
    let mut parts = scripted_parts(&c, &[(99.0, 100.0), (99.0, 100.0)], 10.0).await;
    parts.book.open_orders = vec![
        resting("filled-1", 100.0),
        resting("filled-2", 100.0),
        resting("cancelled", 90.0),
    ];
    let notifier = RecordingNotifier::default();
    parts.notifier = Box::new(notifier.clone());
    let s = run_with_parts(
        &c,
        &RunOptions {
            once: false,
            max_ticks: 2,
        },
        parts,
    )
    .await
    .unwrap();
    assert_eq!((s.fills, s.cancels), (2, 1));
    let messages = notifier.messages.lock().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].0, "limen-live paper: fill");
    assert!(messages[0].1.contains("filled-1 buy 0.01000000 @ 100.00"));
    assert!(messages[1].1.contains("filled-2"));
    assert_eq!(messages[2].0, "limen-live paper: cancel");
    assert!(messages[2].1.contains("cancelled buy stale after 1 ticks"));
    for (_, body) in messages.iter() {
        assert!(body.contains("BTC-USD tick 1"));
        assert!(body.contains(p.to_str().unwrap()));
    }
    assert_eq!(Journal::new(p).row_count().unwrap(), 2);
}

#[tokio::test]
async fn forced_halt_notifies_once_and_keeps_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("halt.md");
    let c = cfg(&p, 1);
    let mut parts = scripted_parts(&c, &[(100.0, 100.1)], 10.0).await;
    parts.book.realized_pnl_usd = -c.risk.max_daily_loss_usd;
    parts.day_start_realized = Some(0.0);
    let notifier = RecordingNotifier::default();
    parts.notifier = Box::new(notifier.clone());
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
    assert_eq!(s.ticks, 1);
    assert_eq!(s.halted_reason.as_deref(), Some("daily_loss_breached"));
    let messages = notifier.messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].0, "limen-live paper: halt");
    assert!(std::fs::read_to_string(p)
        .unwrap()
        .contains("rejected:daily_loss_breached"));
}

#[tokio::test]
async fn decider_errors_notify_per_error_and_delivery_failure_does_not_stop_loop() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("error.md");
    let c = cfg(&p, 1);
    let mut parts = scripted_parts(&c, &[(100.0, 100.1); 2], 10.0).await;
    parts.decider = Box::new(ErrorDecider);
    let notifier = RecordingNotifier {
        fail: true,
        ..Default::default()
    };
    parts.notifier = Box::new(notifier.clone());
    let s = run_with_parts(
        &c,
        &RunOptions {
            once: false,
            max_ticks: 2,
        },
        parts,
    )
    .await
    .unwrap();
    assert_eq!(s.ticks, 2);
    let messages = notifier.messages.lock().unwrap();
    assert_eq!(messages.len(), 2);
    assert!(messages
        .iter()
        .all(|(title, body)| title == "limen-live paper: decider error"
            && body.contains("test failure")));
    assert_eq!(
        std::fs::read_to_string(p)
            .unwrap()
            .matches("decider_error:")
            .count(),
        2
    );
}

#[tokio::test]
async fn quiet_hold_does_not_notify() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("hold.md");
    let c = cfg(&p, 1);
    let mut parts = scripted_parts(&c, &[(100.0, 100.1)], 10.0).await;
    let notifier = RecordingNotifier::default();
    parts.notifier = Box::new(notifier.clone());
    run_with_parts(
        &c,
        &RunOptions {
            once: true,
            max_ticks: 0,
        },
        parts,
    )
    .await
    .unwrap();
    assert!(notifier.messages.lock().unwrap().is_empty());
    assert!(std::fs::read_to_string(p).unwrap().contains("warming_up"));
}
