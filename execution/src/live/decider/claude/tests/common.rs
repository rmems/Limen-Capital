//! Shared fixtures for the Claude decider tests: a loopback mock server, a
//! decider with an in-test ledger and clock, and response builders.

use super::super::*;
use crate::live::decider::budget::FileStore;
use crate::live::market_state::AccountState;
use crate::live::test_support::claude::*;
use crate::live::test_support::{MockServer, RecordedRequest};

pub(super) fn state_at(ts: i64) -> MarketState {
    MarketState::new(
        ts,
        1,
        "BTC-USD",
        100_000.0,
        100_010.0,
        AccountState {
            position_qty: 0.00005,
            avg_cost_usd: Some(99_000.0),
            cash_usd: 5.0,
            realized_pnl_usd: 0.0,
        },
        vec![99_900.0],
    )
}

pub(super) fn fresh_state() -> MarketState {
    state_at(chrono::Utc::now().timestamp())
}

pub(super) struct Harness {
    pub server: MockServer,
    pub decider: ClaudeDecider,
    pub clock: TestClock,
    _dir: tempfile::TempDir,
}

pub(super) async fn harness(
    responses: Vec<(u16, String)>,
    tweak: impl FnOnce(&mut LiveConfig),
) -> Harness {
    harness_with(messages_route(responses), tweak).await
}

pub(super) async fn harness_with(
    route: crate::live::test_support::Route,
    tweak: impl FnOnce(&mut LiveConfig),
) -> Harness {
    let server = MockServer::start(vec![route]).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = claude_cfg(&server.base_url(), dir.path());
    tweak(&mut cfg);
    let clock = TestClock::new();
    let store = FileStore::open(&cfg.llm.ledger_path).unwrap();
    let decider =
        ClaudeDecider::with_store(&cfg, TEST_KEY, Box::new(store), clock.clock()).unwrap();
    Harness {
        server,
        decider,
        clock,
        _dir: dir,
    }
}

pub(super) fn body(r: &RecordedRequest) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

pub(super) fn models(reqs: &[RecordedRequest]) -> Vec<String> {
    reqs.iter()
        .map(|r| body(r)["model"].as_str().unwrap_or("").to_string())
        .collect()
}

pub(super) async fn wait_for_requests(server: &MockServer, n: usize) {
    for _ in 0..200 {
        if server.requests().await.len() >= n {
            // Let the helper task finish settling.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("expected {n} requests");
}

pub(super) fn tool_response(blocks: Vec<Value>, stop_reason: &str) -> String {
    message(SONNET, stop_reason, blocks, Some(usage(100, 10)))
}

pub(super) fn tool_call(input: Value) -> String {
    tool_response(vec![tool_block(DECISION_TOOL, input)], "tool_use")
}

pub(super) fn input_json(action: &str, size_usd: f64, reason: &str) -> Value {
    json!({"action": action, "size_usd": size_usd, "reason": reason})
}
