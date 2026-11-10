//! Call-path tests: retries, timeouts, staleness, usage/pricing halts,
//! budget gates, and credit expiry.

use super::super::*;
use super::common::*;
use crate::live::decider::budget::{BudgetError, ReservationState};
use crate::live::test_support::claude::*;
use chrono::{TimeZone, Utc};

#[tokio::test]
async fn server_errors_retry_then_recover() {
    let mut h = harness(
        vec![(500, "{}".into()), (200, decision("buy", 1.5, "recovered"))],
        |c| c.llm.max_retries = 1,
    )
    .await;
    let d = h.decider.decide(&fresh_state(), &[]).await.unwrap();
    assert_eq!(d, Decision::buy(1.5, "recovered"));
    assert_eq!(h.decider.take_llm_reports()[0].attempts, 2);
}

#[tokio::test]
async fn rate_limit_and_overloaded_exhaust_bounded_retries() {
    let mut h = harness(
        vec![
            (
                429,
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow"}}"#.into(),
            ),
            (
                529,
                r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#.into(),
            ),
        ],
        |c| c.llm.max_retries = 1,
    )
    .await;
    let err = h.decider.decide(&fresh_state(), &[]).await.unwrap_err();
    assert!(
        matches!(err, DeciderError::Http(ref m) if m.contains("529") && m.contains("2 attempt")),
        "{err}"
    );
    assert_eq!(h.server.requests().await.len(), 2, "bounded retries");
    let snap = h.decider.ledger().snapshot();
    assert!(
        snap.reservations.is_empty(),
        "error responses are not billed"
    );
    assert_eq!(snap.committed_usd.total(), 0.0);
}

#[tokio::test]
async fn client_errors_are_not_retried() {
    let mut h = harness(
        vec![(400, r#"{"error":{"type":"invalid_request_error"}}"#.into())],
        |c| c.llm.max_retries = 3,
    )
    .await;
    let err = h.decider.decide(&fresh_state(), &[]).await.unwrap_err();
    assert!(
        matches!(err, DeciderError::Http(ref m) if m.contains("invalid_request_error")),
        "{err}"
    );
    assert_eq!(h.server.requests().await.len(), 1, "4xx is not retried");
}

#[tokio::test]
async fn each_retry_goes_back_through_the_budget_gate() {
    let mut h = harness(
        vec![(503, "{}".into()), (200, decision("buy", 1.0, "x"))],
        |c| {
            c.llm.max_retries = 3;
            c.llm.retry_backoff_ms = 400;
            c.risk.max_llm_cost_usd_per_day = 0.5;
            c.llm.max_llm_cost_usd_total = Some(1.0);
        },
    )
    .await;
    // Another spender takes the rest of the day while this call backs off.
    let ledger = h.decider.ledger().clone();
    let now = h.clock.clock()();
    let rival = tokio::spawn(async move {
        while ledger.snapshot().reservations.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        while !ledger.snapshot().reservations.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        ledger
            .reserve(Role::Decider, SONNET, 0.5, now, now)
            .unwrap()
    });
    let err = h.decider.decide(&fresh_state(), &[]).await.unwrap_err();
    rival.await.unwrap();
    assert!(
        matches!(err, DeciderError::Budget(BudgetError::Exhausted { .. })),
        "{err}"
    );
    assert_eq!(
        h.server.requests().await.len(),
        1,
        "retry refused before dispatch"
    );
    assert!(h.decider.ledger().snapshot().used_day_usd("2026-11-10") <= 0.5 + 1e-12);
}

#[tokio::test]
async fn timeout_holds_and_keeps_the_charge_reserved() {
    let route = messages_route(vec![(200, decision("buy", 1.0, "late"))]).with_delay(2_500);
    let mut h = harness_with(route, |c| c.llm.request_timeout_secs = 1).await;
    let err = h.decider.decide(&fresh_state(), &[]).await.unwrap_err();
    assert!(matches!(err, DeciderError::Timeout { .. }), "{err}");
    let snap = h.decider.ledger().snapshot();
    assert_eq!(snap.reservations.len(), 1);
    assert_eq!(snap.reservations[0].state, ReservationState::Ambiguous);
    assert!(snap.halted.is_none());
    assert!(h.decider.ledger().remaining_usd() < 200.0);
    assert_eq!(h.decider.take_llm_reports()[0].outcome, "error:timeout");
}

#[tokio::test]
async fn missing_usage_halts_all_new_paid_calls() {
    let no_usage = message(
        SONNET,
        "tool_use",
        vec![tool_block(DECISION_TOOL, input_json("buy", 1.0, "a"))],
        None,
    );
    let mut h = harness(
        vec![
            (200, no_usage),
            (200, decision("buy", 1.0, "never requested")),
        ],
        |_| {},
    )
    .await;
    let err = h.decider.decide(&fresh_state(), &[]).await.unwrap_err();
    assert!(matches!(err, DeciderError::MissingUsage(_)), "{err}");
    let snap = h.decider.ledger().snapshot();
    assert_eq!(snap.halted.as_deref(), Some("missing_usage"));
    assert_eq!(snap.reservations[0].state, ReservationState::Ambiguous);
    let err = h.decider.decide(&fresh_state(), &[]).await.unwrap_err();
    assert!(
        matches!(err, DeciderError::Budget(BudgetError::Halted(_))),
        "{err}"
    );
    assert_eq!(h.server.requests().await.len(), 1);
}

#[tokio::test]
async fn unpriced_response_model_halts_without_downgrade() {
    let wrong = message(
        HAIKU,
        "tool_use",
        vec![tool_block(DECISION_TOOL, input_json("buy", 1.0, "a"))],
        Some(usage(10, 10)),
    );
    let mut h = harness(vec![(200, wrong)], |_| {}).await;
    let err = h.decider.decide(&fresh_state(), &[]).await.unwrap_err();
    assert!(matches!(err, DeciderError::Pricing(_)), "{err}");
    assert_eq!(
        h.decider.ledger().halted().as_deref(),
        Some("missing_pricing")
    );
    assert!(h.decider.decide(&fresh_state(), &[]).await.is_err());
    assert_eq!(models(&h.server.requests().await), vec![SONNET.to_string()]);
}

#[tokio::test]
async fn stale_quote_holds_without_a_paid_call() {
    let mut h = harness(vec![(200, decision("buy", 1.0, "x"))], |c| {
        c.llm.decision_deadline_secs = 30
    })
    .await;
    let old = state_at(Utc::now().timestamp() - 600);
    let err = h.decider.decide(&old, &[]).await.unwrap_err();
    assert!(matches!(err, DeciderError::Stale { .. }), "{err}");
    assert!(h.server.requests().await.is_empty());
}

#[tokio::test]
async fn affordable_small_budget_passes_and_unaffordable_is_refused() {
    // Bounded output keeps the worst case (~$0.02) inside a $0.04 day.
    let mut h = harness(vec![(200, decision("hold", 0.0, "ok"))], |c| {
        c.llm.max_llm_cost_usd_total = Some(0.05);
        c.risk.max_llm_cost_usd_per_day = 0.04;
        c.llm.decider_max_output_tokens = 1_000;
    })
    .await;
    assert!(h
        .decider
        .decide(&fresh_state(), &[])
        .await
        .unwrap()
        .is_hold());
    // The default 8192-token reservation (~$0.09) does not fit: refused first.
    let mut h = harness(vec![(200, decision("hold", 0.0, "never"))], |c| {
        c.llm.max_llm_cost_usd_total = Some(0.05);
        c.risk.max_llm_cost_usd_per_day = 0.04;
    })
    .await;
    let err = h.decider.decide(&fresh_state(), &[]).await.unwrap_err();
    assert!(
        matches!(err, DeciderError::Budget(BudgetError::Exhausted { .. })),
        "{err}"
    );
    assert!(h.server.requests().await.is_empty());
    assert_eq!(h.decider.ledger().snapshot().cap_total_usd, 0.05);
}

#[tokio::test]
async fn credit_expiry_blocks_paid_calls() {
    let mut h = harness(vec![(200, decision("buy", 1.0, "x"))], |_| {}).await;
    h.clock
        .set(Utc.with_ymd_and_hms(2026, 11, 13, 8, 0, 0).unwrap());
    let err = h.decider.decide(&fresh_state(), &[]).await.unwrap_err();
    assert_eq!(err.kind(), "expired", "{err}");
    // Seconds before the cutoff, a call that could outlive it is refused too.
    h.clock
        .set(Utc.with_ymd_and_hms(2026, 11, 13, 7, 59, 57).unwrap());
    let err = h.decider.decide(&fresh_state(), &[]).await.unwrap_err();
    assert_eq!(err.kind(), "expired", "{err}");
    assert!(h.server.requests().await.is_empty());
}

#[tokio::test]
async fn prompt_over_the_input_bound_is_refused_unsent() {
    let mut h = harness(vec![(200, decision("buy", 1.0, "x"))], |c| {
        c.llm.max_input_tokens = 1_500
    })
    .await;
    let rows: Vec<String> = (0..5)
        .map(|i| format!("| {} | {} |", i, "x".repeat(400)))
        .collect();
    let err = h.decider.decide(&fresh_state(), &rows).await.unwrap_err();
    assert!(matches!(err, DeciderError::PromptTooLarge { .. }), "{err}");
    assert!(h.server.requests().await.is_empty());
}
