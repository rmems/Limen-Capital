use super::*;
use crate::live::test_support::{MockServer, Route};

const FIXTURE: &str = include_str!("../../../tests/fixtures/postmortem.md");

fn config(dir: &Path) -> LiveConfig {
    let path = dir.join("fixture.md");
    fs::write(&path, FIXTURE).unwrap();
    LiveConfig {
        journal_path: path.to_string_lossy().into(),
        ..LiveConfig::default()
    }
}

fn response() -> String {
    json!({
        "model": MODEL, "stop_reason": "end_turn",
        "content": [{"type":"text", "text":"Two trades, $0.045 fees, one reject and one parse error. Rules did better on the same quotes under the stated assumptions."}],
        "usage": {"input_tokens": 1000, "output_tokens": 100}
    }).to_string()
}

async fn server(status: u16, body: &str) -> MockServer {
    MockServer::start(vec![
        Route::ok(
            "POST",
            "/v1/messages/count_tokens",
            r#"{"input_tokens":1000}"#,
        ),
        Route::status("POST", "/v1/messages", status, body),
    ])
    .await
}

#[tokio::test]
async fn fixture_produces_note_with_sonnet_only_and_no_venue_requests() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let server = server(200, &response()).await;
    let path = run_at(
        &cfg,
        "test-only-key",
        &reqwest::Client::new(),
        &format!("{}/v1/messages", server.base_url()),
    )
    .await
    .unwrap();
    let note = fs::read_to_string(path).unwrap();
    for expected in [
        "Trades (fill rows): 2",
        "fees recorded: $0.0450",
        "Rejects: 1",
        "decider parse errors: 1",
        "rules would have done better",
        "## Sonnet note",
        MODEL,
        "No orders placed",
    ] {
        assert!(note.contains(expected), "{expected}: {note}");
    }
    assert_eq!(fs::read_to_string(&cfg.journal_path).unwrap(), FIXTURE);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    for req in &requests {
        assert!(req.path.starts_with("/v1/messages"));
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["model"], MODEL);
        assert!(body.get("tools").is_none());
    }
    let day = Utc::now().format("%Y-%m-%d").to_string();
    assert!((cost::spent(Path::new(&cfg.journal_path), &day).unwrap() - 0.003).abs() < 1e-10);
    // Repeated passes create separate notes and accumulate rather than reset spend.
    run_at(
        &cfg,
        "key",
        &reqwest::Client::new(),
        &format!("{}/v1/messages", server.base_url()),
    )
    .await
    .unwrap();
    assert!((cost::spent(Path::new(&cfg.journal_path), &day).unwrap() - 0.006).abs() < 1e-10);
}

#[test]
fn replay_has_independent_equity_and_fee_expectations() {
    let cfg = LiveConfig {
        paper_fee_ratio: 0.0,
        ..LiveConfig::default()
    };
    let a = replay::analyze(FIXTURE, &cfg, "2026-11-09").unwrap();
    // Buy floor(5/99, 1e-8) at 99, sell at 100 on a later quote.
    assert!(
        a.evidence.contains("rules final equity $10.0505"),
        "{}",
        a.evidence
    );
    assert!(a.evidence.contains("recorded final equity $9.9550"));
    assert!(a.evidence.contains("rules fills 2, fees $0.0000"));
    assert!((a.today_spend - 0.006).abs() < 1e-10);
    assert_eq!(
        replay::analyze(FIXTURE, &cfg, "2026-11-10")
            .unwrap()
            .today_spend,
        0.0
    );
    let bad = FIXTURE.replace("| 98 | 98 |", "| NaN | 98 |");
    assert!(replay::analyze(&bad, &cfg, "2026-11-09")
        .unwrap()
        .evidence
        .contains("inconclusive"));
    assert!(replay::analyze(&FIXTURE.replace("0.001 |", "NaN |"), &cfg, "2026-11-09").is_err());
    let runs = FIXTURE.replace("| hold | 0 | done", "| run_start | 0 | done");
    assert!(replay::analyze(&runs, &cfg, "2026-11-09")
        .unwrap()
        .evidence
        .contains("inconclusive"));
}

#[tokio::test]
async fn daily_cap_includes_journal_and_reservations_before_paid_request() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    let day = Utc::now().format("%Y-%m-%d").to_string();
    let today = FIXTURE.replace("2026-11-09", &day);
    fs::write(&cfg.journal_path, today).unwrap();
    cfg.risk.max_llm_cost_usd_per_day = 0.02;
    // Existing journal $0.006 + reservation $0.014488 exceeds $0.02.
    let server = server(200, &response()).await;
    assert!(run_at(
        &cfg,
        "key",
        &reqwest::Client::new(),
        &format!("{}/v1/messages", server.base_url())
    )
    .await
    .is_err());
    assert_eq!(server.requests().await.len(), 1);
    assert_eq!(
        cost::spent(Path::new(&cfg.journal_path), &day).unwrap(),
        0.0
    );
}

#[tokio::test]
async fn failures_remain_charged_and_never_retry() {
    for (status, body) in [
        (500, "{}".into()),
        (200, "not json".into()),
        (200, response().replace("end_turn", "max_tokens")),
        (200, response().replace(MODEL, "claude-haiku-5-5")),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let server = server(status, &body).await;
        assert!(run_at(
            &cfg,
            "key",
            &reqwest::Client::new(),
            &format!("{}/v1/messages", server.base_url())
        )
        .await
        .is_err());
        assert_eq!(server.requests().await.len(), 2);
        let spent = cost::spent(
            Path::new(&cfg.journal_path),
            &Utc::now().format("%Y-%m-%d").to_string(),
        )
        .unwrap();
        assert!(spent >= 0.003);
        assert_eq!(fs::read_to_string(&cfg.journal_path).unwrap(), FIXTURE);
    }
}

#[test]
fn reservations_are_durable_serialized_and_reset_by_utc_day() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("cost.md");
    let id = cost::reserve(&journal, "2026-11-09", 0.6, 0.1, 1.0).unwrap();
    assert!(cost::reserve(&journal, "2026-11-09", 0.4, 0.1, 1.0).is_err());
    assert_eq!(cost::spent(&journal, "2026-11-09").unwrap(), 0.6);
    cost::settle(&journal, "2026-11-09", &id, 0.2).unwrap();
    assert_eq!(cost::spent(&journal, "2026-11-09").unwrap(), 0.2);
    assert_eq!(cost::spent(&journal, "2026-11-10").unwrap(), 0.0);
    cost::reserve(&journal, "2026-11-10", 1.0, 0.0, 1.0).unwrap();
}

#[test]
fn overlapping_reservations_cannot_both_spend_the_remaining_budget() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("overlap.md");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let journal = journal.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                cost::reserve(&journal, "2026-11-10", 0.6, 0.0, 1.0).is_ok()
            })
        })
        .collect();
    let successes = handles
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum::<usize>();
    assert_eq!(successes, 1);
    assert_eq!(cost::spent(&journal, "2026-11-10").unwrap(), 0.6);
}
