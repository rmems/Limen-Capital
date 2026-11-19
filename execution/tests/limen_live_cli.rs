#![cfg(feature = "live-alpaca")]
use std::process::Command;

#[test]
fn live_false_exits_before_credentials_or_venue_access() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("live.toml");
    std::fs::write(&config, "paper = false\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_limen-live"))
        .arg("--config")
        .arg(&config)
        .arg("--once")
        .env_remove("APCA_API_KEY_ID")
        .env_remove("APCA_API_SECRET_KEY")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("live mode is not available"));
    assert!(!dir.path().join("journal.md").exists());
}

#[test]
fn real_cli_runs_one_offline_paper_tick() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("paper.toml");
    let journal = dir.path().join("journal.md");
    std::fs::write(
        &config,
        "paper = true\n[market]\nsource = \"synthetic\"\nstart_price = 200.0\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_limen-live"))
        .arg("--config")
        .arg(&config)
        .arg("--once")
        .arg("--journal")
        .arg(&journal)
        .env_remove("APCA_API_KEY_ID")
        .env_remove("APCA_API_SECRET_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("done: ticks=1 orders=0"));
    let contents = std::fs::read_to_string(&journal).unwrap();
    assert!(contents.contains("synthetic"));
    assert!(contents.contains("AAPL"));
}
