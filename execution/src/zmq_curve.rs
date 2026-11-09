//! Optional ZMQ CURVE wiring + ZAP client public-key allowlist (research).
//!
//! Default path is plain TCP on loopback (`ZMQ_CURVE` unset or `0`). When
//! `ZMQ_CURVE=1`, peers configure CURVE on sockets. If an allowlist is supplied
//! via [`ZMQ_CURVE_ALLOWLIST`] or [`ZMQ_CURVE_ALLOWLIST_FILE`], a ZAP handler is
//! started on the shared [`Context`] and CURVE **server** sockets (bind) reject
//! clients whose public keys are not listed.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use tracing::{debug, warn};
use zmq::{Context, Socket, SocketType};

/// Default ZAP domain for Capital research CURVE servers.
pub const DEFAULT_ZAP_DOMAIN: &str = "limen-capital";

/// Return true when `ZMQ_CURVE=1`.
pub fn curve_enabled() -> bool {
    match std::env::var("ZMQ_CURVE") {
        Ok(v) => v.trim() == "1",
        Err(_) => false,
    }
}

/// Active ZAP domain (`ZMQ_ZAP_DOMAIN` or [`DEFAULT_ZAP_DOMAIN`]).
pub fn zap_domain() -> String {
    if let Ok(v) = std::env::var("ZMQ_ZAP_DOMAIN") {
        let t = v.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    DEFAULT_ZAP_DOMAIN.to_string()
}

fn trim_z85_key(key: &str) -> Option<String> {
    let k = key.trim();
    if k.is_empty() || k.starts_with('#') {
        return None;
    }
    Some(k.to_string())
}

/// Parse optional allowlist from env. `None` when unset/empty (no ZAP).
pub fn load_curve_allowlist() -> Result<Option<Vec<String>>, String> {
    let mut keys = Vec::new();

    if let Ok(raw) = std::env::var("ZMQ_CURVE_ALLOWLIST") {
        for part in raw.split([',', ';', ' ', '\t', '\n']) {
            if let Some(k) = trim_z85_key(part) {
                keys.push(k);
            }
        }
    }

    if let Ok(path) = std::env::var("ZMQ_CURVE_ALLOWLIST_FILE") {
        let path = path.trim();
        if path.is_empty() {
            return Err("ZMQ_CURVE_ALLOWLIST_FILE is empty".into());
        }
        let file =
            fs::File::open(path).map_err(|e| format!("ZMQ_CURVE_ALLOWLIST_FILE ({path}): {e}"))?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|e| format!("ZMQ_CURVE_ALLOWLIST_FILE read: {e}"))?;
            if let Some(k) = trim_z85_key(&line) {
                keys.push(k);
            }
        }
    }

    if keys.is_empty() {
        return Ok(None);
    }

    let mut seen = HashSet::new();
    keys.retain(|k| seen.insert(k.clone()));
    Ok(Some(keys))
}

fn first_nonempty(vars: &[&str]) -> Option<String> {
    for name in vars {
        if let Ok(v) = std::env::var(name) {
            let t = v.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

/// Secret key (z85) for the market-feed CURVE server (`market_sim` PUB bind).
pub fn market_server_secret_z85() -> Result<String, String> {
    first_nonempty(&["ZMQ_MARKET_SERVER_SECRET", "ZMQ_SERVER_KEY"]).ok_or_else(|| {
        "market CURVE server requires ZMQ_MARKET_SERVER_SECRET or ZMQ_SERVER_KEY".into()
    })
}

/// Secret key (z85) for the readout CURVE server (brain PUB bind on :5556).
pub fn readout_server_secret_z85() -> Result<String, String> {
    first_nonempty(&["ZMQ_READOUT_SERVER_SECRET", "ZMQ_SERVER_KEY"]).ok_or_else(|| {
        "readout CURVE server requires ZMQ_READOUT_SERVER_SECRET or ZMQ_SERVER_KEY".into()
    })
}

/// Secret key (z85) for CURVE client sockets (connect) on this process.
pub fn client_secret_key_z85() -> Result<String, String> {
    first_nonempty(&[
        "ZMQ_CLIENT_SECRET_KEY",
        "ZMQ_CURVE_SECRET_KEY",
        "ZMQ_SERVER_KEY",
    ])
    .ok_or_else(|| "CURVE client socket requires ZMQ_CLIENT_SECRET_KEY or ZMQ_SERVER_KEY".into())
}

/// Remote market-feed server public key (`LIMEN_IPC_SUB` / :5555 publisher).
pub fn market_server_public_z85() -> Result<String, String> {
    first_nonempty(&["ZMQ_MARKET_SERVER_PUBLIC_KEY", "ZMQ_MARKET_PUB_KEY"])
        .ok_or_else(|| "CURVE client to market feed requires ZMQ_MARKET_SERVER_PUBLIC_KEY".into())
}

/// Remote readout server public key (`LIMEN_IPC_PUB` / :5556 publisher).
pub fn readout_server_public_z85() -> Result<String, String> {
    first_nonempty(&["ZMQ_READOUT_SERVER_PUBLIC_KEY", "ZMQ_CLIENT_KEY"]).ok_or_else(|| {
        "CURVE client to readout requires ZMQ_READOUT_SERVER_PUBLIC_KEY (legacy: ZMQ_CLIENT_KEY)"
            .into()
    })
}

fn decode_z85(key: &str) -> Result<Vec<u8>, String> {
    zmq::z85_decode(key).map_err(|_| format!("invalid Z85 CURVE key ({key})"))
}

/// Configure a socket as CURVE server (bind side). Call **before** `bind`.
pub fn apply_curve_server(
    socket: &Socket,
    server_secret_z85: &str,
    zap: bool,
) -> Result<(), String> {
    if !zmq::has("curve").unwrap_or(false) {
        return Err("libzmq built without CURVE support".into());
    }
    let secret = decode_z85(server_secret_z85)?;
    socket
        .set_curve_server(true)
        .map_err(|e| format!("set_curve_server: {e}"))?;
    socket
        .set_curve_secretkey(&secret)
        .map_err(|e| format!("set_curve_secretkey: {e}"))?;
    if zap {
        let domain = zap_domain();
        socket
            .set_zap_domain(&domain)
            .map_err(|e| format!("set_zap_domain: {e}"))?;
        debug!("CURVE server + ZAP domain {domain}");
    }
    Ok(())
}

/// Configure a socket as CURVE client (connect side). Call **before** `connect`.
pub fn apply_curve_client(
    socket: &Socket,
    server_public_z85: &str,
    client_secret_z85: &str,
    client_public_z85: Option<&str>,
) -> Result<(), String> {
    if !zmq::has("curve").unwrap_or(false) {
        return Err("libzmq built without CURVE support".into());
    }
    let server_pub = decode_z85(server_public_z85)?;
    let client_pub_z85: String = client_public_z85
        .map(|s| s.to_string())
        .or_else(|| first_nonempty(&["ZMQ_CLIENT_PUBLIC_KEY"]))
        .ok_or_else(|| {
            "CURVE client requires ZMQ_CLIENT_PUBLIC_KEY (generate via libzmq keypair tooling)"
                .to_string()
        })?;
    let client_sec = decode_z85(client_secret_z85)?;
    let client_pub = decode_z85(&client_pub_z85)?;

    socket
        .set_curve_server(false)
        .map_err(|e| format!("set_curve_server: {e}"))?;
    socket
        .set_curve_serverkey(&server_pub)
        .map_err(|e| format!("set_curve_serverkey: {e}"))?;
    socket
        .set_curve_publickey(&client_pub)
        .map_err(|e| format!("set_curve_publickey: {e}"))?;
    socket
        .set_curve_secretkey(&client_sec)
        .map_err(|e| format!("set_curve_secretkey: {e}"))?;
    Ok(())
}

/// Background ZAP handler restricting CURVE clients to `allowlist` public keys (z85).
pub struct ZapAllowlist {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ZapAllowlist {
    /// Start ZAP on `ctx` (same context as protected server sockets).
    pub fn start(ctx: Context, allowlist: &[String]) -> Result<Self, String> {
        if allowlist.is_empty() {
            return Err("ZAP allowlist is empty".into());
        }
        let allowed: HashSet<String> = allowlist.iter().cloned().collect();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = stop.clone();
        let join = thread::Builder::new()
            .name("limen-zap-allowlist".into())
            .spawn(move || zap_thread(ctx, allowed, stop_flag))
            .map_err(|e| format!("spawn ZAP thread: {e}"))?;
        Ok(Self {
            stop,
            join: Some(join),
        })
    }
}

impl Drop for ZapAllowlist {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn zap_thread(ctx: Context, allowed: HashSet<String>, stop: Arc<AtomicBool>) {
    let socket = match ctx.socket(SocketType::REP) {
        Ok(s) => s,
        Err(e) => {
            warn!("ZAP: could not create REP socket: {e}");
            return;
        }
    };
    if let Err(e) = socket.bind("inproc://zeromq.org") {
        warn!("ZAP: bind inproc://zeromq.org failed: {e}");
        return;
    }
    debug!("ZAP allowlist handler ready ({} keys)", allowed.len());

    while !stop.load(Ordering::Relaxed) {
        let request = match socket.recv_multipart(0) {
            Ok(parts) => parts,
            Err(e) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                warn!("ZAP recv: {e}");
                continue;
            }
        };

        if request.len() < 7 {
            warn!("ZAP: short request ({} parts)", request.len());
            continue;
        }

        let version = String::from_utf8_lossy(&request[0]);
        let seq = &request[1];
        let mechanism = String::from_utf8_lossy(&request[5]);
        let user = String::from_utf8_lossy(&request[6]);

        let (status, text) = if version != "1.0" {
            (400, "Invalid version")
        } else if mechanism != "CURVE" {
            (400, "Invalid mechanism")
        } else if allowed.contains(user.as_ref()) {
            (200, "OK")
        } else {
            debug!("ZAP reject client key {}", user);
            (400, "Unauthorized CURVE client")
        };

        let reply = vec![
            b"1.0".to_vec(),
            seq.clone(),
            status.to_string().into_bytes(),
            text.as_bytes().to_vec(),
            Vec::new(),
            Vec::new(),
        ];
        if let Err(e) = socket.send_multipart(&reply, 0) {
            warn!("ZAP reply: {e}");
        }
    }
}

/// When `ZMQ_CURVE=1`, optionally start ZAP (if allowlist configured).
pub fn maybe_start_zap(ctx: &Context) -> Result<Option<ZapAllowlist>, String> {
    if !curve_enabled() {
        return Ok(None);
    }
    match load_curve_allowlist()? {
        None => Ok(None),
        Some(keys) => {
            let guard = ZapAllowlist::start(ctx.clone(), &keys)?;
            Ok(Some(guard))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;
    use zmq::CurveKeyPair;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce() -> R, R>(pairs: &[(&str, Option<&str>)], f: F) -> R {
        let _g = ENV_LOCK.lock().unwrap();
        let saved: Vec<(String, Result<String, std::env::VarError>)> = pairs
            .iter()
            .map(|(k, _)| ((*k).to_string(), std::env::var(k)))
            .collect();
        for (k, v) in pairs {
            match v {
                Some(val) => unsafe { std::env::set_var(k, val) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
        let out = f();
        for (k, prev) in saved {
            match prev {
                Ok(v) => unsafe { std::env::set_var(&k, v) },
                Err(_) => unsafe { std::env::remove_var(&k) },
            }
        }
        out
    }

    #[test]
    fn parse_allowlist_env_and_dedupes() {
        with_env(
            &[
                ("ZMQ_CURVE_ALLOWLIST", Some(" AAA , BBB ; AAA ")),
                ("ZMQ_CURVE_ALLOWLIST_FILE", None),
            ],
            || {
                let keys = load_curve_allowlist().unwrap().expect("keys");
                assert_eq!(keys, vec!["AAA", "BBB"]);
            },
        );
    }

    #[test]
    fn curve_defaults_off_without_env() {
        with_env(&[("ZMQ_CURVE", None)], || {
            assert!(!curve_enabled());
            assert!(load_curve_allowlist().unwrap().is_none());
        });
    }

    fn z85_public(pair: &CurveKeyPair) -> String {
        zmq::z85_encode(&pair.public_key).unwrap()
    }

    fn z85_secret(pair: &CurveKeyPair) -> String {
        zmq::z85_encode(&pair.secret_key).unwrap()
    }

    fn curve_capable() -> bool {
        zmq::has("curve").unwrap_or(false)
    }

    #[test]
    fn zap_rejects_unlisted_client_keys() {
        if !curve_capable() {
            eprintln!("skip zap_rejects: no CURVE in libzmq");
            return;
        }

        let server = CurveKeyPair::new().unwrap();
        let allowed = CurveKeyPair::new().unwrap();
        let blocked = CurveKeyPair::new().unwrap();

        let server_pub = z85_public(&server);
        let server_sec = z85_secret(&server);
        let allow_pub = z85_public(&allowed);
        let _blocked_pub = z85_public(&blocked);

        let ctx = Context::new();
        let _zap = ZapAllowlist::start(ctx.clone(), std::slice::from_ref(&allow_pub)).unwrap();

        let publisher = ctx.socket(SocketType::PUB).unwrap();
        apply_curve_server(&publisher, &server_sec, true).unwrap();
        publisher.bind("tcp://127.0.0.1:0").unwrap();
        let endpoint = publisher.get_last_endpoint().unwrap().unwrap();

        let ok_sub = ctx.socket(SocketType::SUB).unwrap();
        apply_curve_client(
            &ok_sub,
            &server_pub,
            &z85_secret(&allowed),
            Some(&allow_pub),
        )
        .unwrap();
        ok_sub.set_subscribe(b"").unwrap();
        ok_sub.set_rcvtimeo(2000).unwrap();
        ok_sub.connect(&endpoint).unwrap();

        let bad_sub = ctx.socket(SocketType::SUB).unwrap();
        apply_curve_client(
            &bad_sub,
            &server_pub,
            &z85_secret(&blocked),
            Some(&z85_public(&blocked)),
        )
        .unwrap();
        bad_sub.set_subscribe(b"").unwrap();
        bad_sub.set_rcvtimeo(500).unwrap();
        bad_sub.connect(&endpoint).unwrap();

        thread::sleep(Duration::from_millis(100));
        publisher.send("hello", 0).unwrap();

        let good = ok_sub.recv_bytes(0);
        assert!(good.is_ok(), "allowed client should receive: {good:?}");

        let bad = bad_sub.recv_bytes(0);
        assert!(bad.is_err(), "unlisted client should not receive: {bad:?}");
    }

    #[test]
    fn curve_plaintext_default_unchanged() {
        let ctx = Context::new();
        let pub_sock = ctx.socket(SocketType::PUB).unwrap();
        pub_sock.bind("tcp://127.0.0.1:0").unwrap();
        let ep = pub_sock.get_last_endpoint().unwrap().unwrap();
        let sub = ctx.socket(SocketType::SUB).unwrap();
        sub.set_subscribe(b"").unwrap();
        sub.set_rcvtimeo(1000).unwrap();
        sub.connect(&ep).unwrap();
        thread::sleep(Duration::from_millis(50));
        pub_sock.send("x", 0).unwrap();
        assert!(sub.recv_bytes(0).is_ok());
    }
}
