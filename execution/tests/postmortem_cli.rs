#![cfg(feature = "live-robinhood")]

use std::process::Command;

#[test]
fn postmortem_dispatch_never_starts_a_venue_or_tick_loop() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("fixture.md");
    let fixture = include_str!("fixtures/postmortem.md");
    std::fs::write(&journal, fixture).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_limen-live"))
        .args(["--postmortem", "--journal"])
        .arg(&journal)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ROBINHOOD_API_KEY")
        .env_remove("ROBINHOOD_PRIVATE_KEY")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let stderr = String::from_utf8(result.stderr).unwrap();
    assert!(stderr.contains("post-mortem: missing secret: set the ANTHROPIC_API_KEY"));
    assert!(!stderr.contains("ROBINHOOD"));
    assert!(!String::from_utf8(result.stdout).unwrap().contains("tick"));
    assert_eq!(std::fs::read_to_string(journal).unwrap(), fixture);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
