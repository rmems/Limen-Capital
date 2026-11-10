//! Exercise the shipped binary, using independent wire bytes and private IPC.
#![cfg(unix)]

use std::fs;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for(log: &std::path::Path, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = fs::read_to_string(log).unwrap();
        if text.contains(needle) {
            return text;
        }
        assert!(Instant::now() < deadline, "missing {needle:?}: {text}");
        sleep(Duration::from_millis(20));
    }
}

fn readout(tick: i64) -> Vec<u8> {
    let mut frame = b"NW1R\x01\x00\x00\x00".to_vec();
    frame.extend_from_slice(&tick.to_le_bytes());
    frame.extend_from_slice(&[0; 64]);
    for _ in 0..4 {
        frame.extend_from_slice(&0.25_f32.to_le_bytes());
    }
    frame
}

#[test]
fn real_listener_rejects_legacy_replay_stale_and_restarts_then_stops_on_sigint() {
    let tmp = tempfile::tempdir().unwrap();
    let context = zmq::Context::new();
    let publisher = context.socket(zmq::PUB).unwrap();
    let ep = format!("ipc://{}/feed", tmp.path().display());
    publisher.bind(&ep).unwrap();
    let log = tmp.path().join("listener.log");
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_spikenaut-execution-engine"))
            .current_dir(tmp.path())
            .env("LIMEN_IPC_SUB", &ep)
            .env("LIMEN_IPC_PUB", &ep)
            .env("LIMEN_WIRE", "binary")
            .env("ZMQ_CURVE", "0")
            .env("LIMEN_CONFIDENCE_THRESHOLD", "0.15")
            .env("LIMEN_PULSE_MAX_AGE_MS", "2000")
            .stdout(fs::File::create(&log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(&log, "Binary SUB connected");
    sleep(Duration::from_millis(200)); // PUB/SUB subscription propagation
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let mut market = b"NW1P\x01\x00\x00\x00".to_vec();
    let mut raw = fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../wire/fixtures/marketpulse.bin"
    ))
    .unwrap();
    raw[..8].copy_from_slice(&now.to_le_bytes());
    market.extend_from_slice(&raw);
    publisher.send(market, 0).unwrap();
    let valid = readout(now as i64);
    publisher.send(&valid, 0).unwrap();
    wait_for(&log, "Signal rejected: confidence");
    publisher.send(&valid, 0).unwrap(); // duplicate
    publisher.send(readout(now as i64 - 1), 0).unwrap(); // out of order
    publisher.send(readout(42), 0).unwrap(); // pre-restart counter
    publisher
        .send(readout(now as i64 - 3_000_000_000), 0)
        .unwrap(); // stale
    publisher
        .send(readout(now as i64 + 60_000_000_000), 0)
        .unwrap(); // future
    publisher.send(&valid[8..], 0).unwrap(); // legacy bare frame
    let mut wrong_version = valid;
    wrong_version[4] = 2;
    publisher.send(wrong_version, 0).unwrap();
    // Wait until all seven refusals arrive, without relying on a fixed delay.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = fs::read_to_string(&log).unwrap();
        if text.matches("Binary frame refused:").count() == 7 {
            assert_eq!(text.matches("Signal rejected: confidence").count(), 1);
            break;
        }
        assert!(Instant::now() < deadline, "{text}");
        sleep(Duration::from_millis(20));
    }
    let shutdown = Instant::now();
    assert!(Command::new("kill")
        .args(["-INT", &child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(shutdown.elapsed() < Duration::from_secs(3), "Ctrl+C hung");
        sleep(Duration::from_millis(20));
    }
    println!("Idle listener Ctrl+C shutdown: {:?}", shutdown.elapsed());
}

#[test]
fn real_binary_refuses_remote_endpoint_without_opt_in() {
    let output = Command::new(env!("CARGO_BIN_EXE_spikenaut-execution-engine"))
        .env("LIMEN_WIRE", "binary")
        .env("LIMEN_IPC_PUB", "tcp://192.0.2.1:5556")
        .env_remove("LIMEN_ALLOW_REMOTE_WIRE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("LIMEN_ALLOW_REMOTE_WIRE=1"));
}
