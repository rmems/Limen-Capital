//! Tiny in-process HTTP/1.1 mock server for unit tests (no extra crates).
//!
//! Records every request (method, path, headers, body) and answers from a
//! route table. Enough for the venue client and the LLM decider; not a
//! general HTTP implementation.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Verify the Robinhood `x-signature` header against `pub_key_b64`.
    pub fn verify_signature(&self, api_key: &str, pub_key_b64: &str) -> bool {
        let (Some(sig), Some(ts), Some(key)) = (
            self.header("x-signature"),
            self.header("x-timestamp"),
            self.header("x-api-key"),
        ) else {
            return false;
        };
        if key != api_key {
            return false;
        }
        let Ok(ts) = ts.parse::<u64>() else {
            return false;
        };
        let Ok(vk) = crate::live::sign::verifying_key_from_b64(pub_key_b64) else {
            return false;
        };
        let Ok(sig_bytes) = B64.decode(sig) else {
            return false;
        };
        let Ok(sig) = Signature::from_slice(&sig_bytes) else {
            return false;
        };
        let msg =
            crate::live::sign::signing_message(api_key, ts, &self.path, &self.method, &self.body);
        vk.verify(msg.as_bytes(), &sig).is_ok()
    }
}

/// One route: method + exact path (with query) → ordered responses.
/// The last response repeats once the sequence is exhausted.
#[derive(Debug, Clone)]
pub struct Route {
    pub method: String,
    pub path: String,
    pub responses: VecDeque<(u16, String)>,
    /// Delay before answering (timeout tests).
    pub delay_ms: u64,
}

impl Route {
    pub fn ok(method: &str, path: &str, body: &str) -> Self {
        Self::status(method, path, 200, body)
    }

    pub fn status(method: &str, path: &str, status: u16, body: &str) -> Self {
        Self::sequence(method, path, vec![(status, body.to_string())])
    }

    pub fn sequence(method: &str, path: &str, responses: Vec<(u16, String)>) -> Self {
        Self {
            method: method.to_string(),
            path: path.to_string(),
            responses: responses.into_iter().collect(),
            delay_ms: 0,
        }
    }

    pub fn with_delay(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }
}

pub struct MockServer {
    addr: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    _routes: Arc<Mutex<Vec<Route>>>,
    accept_loop: tokio::task::JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        // Stop accepting so the listener is released with the server.
        self.accept_loop.abort();
    }
}

impl MockServer {
    pub async fn start(routes: Vec<Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let routes = Arc::new(Mutex::new(routes));
        let req_c = requests.clone();
        let routes_c = routes.clone();
        let accept_loop = tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(serve_connection(sock, req_c.clone(), routes_c.clone()));
            }
        });
        Self {
            addr,
            requests,
            _routes: routes,
            accept_loop,
        }
    }
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub async fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().await.clone()
    }
}

async fn serve_connection(
    mut sock: tokio::net::TcpStream,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    routes: Arc<Mutex<Vec<Route>>>,
) {
    let Ok(request) = read_request(&mut sock).await else {
        return;
    };
    let (status, body, delay_ms) = {
        let mut routes = routes.lock().await;
        let delay = routes
            .iter()
            .find(|r| r.method == request.method && r.path == request.path)
            .map(|r| r.delay_ms)
            .unwrap_or(0);
        let (s, b) = route_response(&mut routes, &request);
        (s, b, delay)
    };
    requests.lock().await.push(request);
    if delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
    let response = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        reason_phrase(status),
        body.len()
    );
    let _ = sock.write_all(response.as_bytes()).await;
    let _ = sock.shutdown().await;
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> std::io::Result<RecordedRequest> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let mut request = parse_headers(&String::from_utf8_lossy(&buf[..header_end]));
    let content_length = request
        .header("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    request.body = String::from_utf8_lossy(&body).to_string();
    Ok(request)
}

fn parse_headers(head: &str) -> RecordedRequest {
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    RecordedRequest {
        method,
        path,
        headers,
        body: String::new(),
    }
}

fn route_response(routes: &mut [Route], request: &RecordedRequest) -> (u16, String) {
    match routes
        .iter_mut()
        .find(|r| r.method == request.method && r.path == request.path)
    {
        Some(route) if route.responses.len() > 1 => route.responses.pop_front().unwrap(),
        Some(route) => route
            .responses
            .front()
            .cloned()
            .unwrap_or((500, String::new())),
        None => (
            404,
            format!(
                r#"{{"type":"client_error","errors":[{{"attr":null,"detail":"no mock route for {} {}"}}]}}"#,
                request.method, request.path
            ),
        ),
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    }
}
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Claude-side fixtures: seeded price table, a mockable config, a settable
/// clock, and Messages API response builders.
pub mod claude {
    use super::Route;
    use crate::live::config::{DeciderKind, LiveConfig, LlmConfig, MarketSourceConfig, ModelPrice};
    use crate::live::decider::budget::Clock;
    use chrono::{DateTime, TimeZone, Utc};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    pub const SONNET: &str = "claude-sonnet-5-5";
    pub const HAIKU: &str = "claude-haiku-5-5";
    pub const TEST_KEY: &str = "sk-ant-TEST-SECRET-KEY";
    pub const EXPIRY: &str = "2026-11-12T23:59:59-08:00";

    /// 2026-11-09 rates from the issue (cache rates: 0.1× / 1.25× input).
    pub fn seed_pricing() -> BTreeMap<String, ModelPrice> {
        BTreeMap::from([
            (
                SONNET.to_string(),
                ModelPrice {
                    input_usd_per_mtok: 2.0,
                    output_usd_per_mtok: 10.0,
                    cache_read_usd_per_mtok: 0.2,
                    cache_write_usd_per_mtok: 2.5,
                    long_context: None,
                },
            ),
            (
                HAIKU.to_string(),
                ModelPrice {
                    input_usd_per_mtok: 0.1,
                    output_usd_per_mtok: 0.5,
                    cache_read_usd_per_mtok: 0.01,
                    cache_write_usd_per_mtok: 0.125,
                    long_context: None,
                },
            ),
        ])
    }

    pub fn claude_cfg(api_base_url: &str, dir: &Path) -> LiveConfig {
        LiveConfig {
            decider: DeciderKind::Claude,
            tick_interval_secs: 1,
            journal_path: dir.join("journal.md").to_string_lossy().into_owned(),
            market: MarketSourceConfig::Synthetic {
                seed: 1,
                start_price: 100_000.0,
                vol: 0.004,
                half_spread_bps: 1.0,
            },
            llm: LlmConfig {
                api_base_url: api_base_url.to_string(),
                ledger_path: dir.join("llm_budget.json").to_string_lossy().into_owned(),
                max_llm_cost_usd_total: Some(200.0),
                max_llm_cost_usd_per_day: Some(60.0),
                credit_expiry: Some(EXPIRY.to_string()),
                request_timeout_secs: 5,
                decision_deadline_secs: 30,
                max_retries: 1,
                retry_backoff_ms: 10,
                pricing: seed_pricing(),
                ..LlmConfig::default()
            },
            ..LiveConfig::default()
        }
    }

    pub fn with_helper(cfg: &mut LiveConfig) {
        cfg.helper_model = Some(HAIKU.to_string());
        cfg.llm.helper_max_cost_usd_total = Some(1.0);
        cfg.llm.helper_max_cost_usd_per_day = Some(0.2);
    }

    #[derive(Clone)]
    pub struct TestClock(Arc<Mutex<DateTime<Utc>>>);

    impl TestClock {
        /// 2026-11-10T12:00:00Z, inside the credit window.
        pub fn new() -> Self {
            Self::at(Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap())
        }

        pub fn at(t: DateTime<Utc>) -> Self {
            Self(Arc::new(Mutex::new(t)))
        }

        pub fn set(&self, t: DateTime<Utc>) {
            *self.0.lock().unwrap() = t;
        }

        pub fn clock(&self) -> Clock {
            let inner = self.0.clone();
            Arc::new(move || *inner.lock().unwrap())
        }
    }

    pub fn usage(input: u64, output: u64) -> Value {
        json!({"input_tokens": input, "output_tokens": output,
               "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0})
    }

    /// Message with a thinking block and the given content blocks.
    pub fn message(
        model: &str,
        stop_reason: &str,
        blocks: Vec<Value>,
        usage: Option<Value>,
    ) -> String {
        let mut content =
            vec![json!({"type": "thinking", "thinking": "PRIVATE-THOUGHT", "signature": "sig"})];
        content.extend(blocks);
        let mut v = json!({"id": "msg_1", "type": "message", "role": "assistant",
                           "model": model, "stop_reason": stop_reason, "content": content});
        if let Some(u) = usage {
            v["usage"] = u;
        }
        v.to_string()
    }

    pub fn tool_block(name: &str, input: Value) -> Value {
        json!({"type": "tool_use", "id": "toolu_1", "name": name, "input": input})
    }

    /// Valid decision response: 1000 in / 200 out = $0.004 at Sonnet rates.
    pub fn decision(action: &str, size_usd: f64, reason: &str) -> String {
        message(
            SONNET,
            "tool_use",
            vec![tool_block(
                "submit_decision",
                json!({"action": action, "size_usd": size_usd, "reason": reason}),
            )],
            Some(usage(1000, 200)),
        )
    }

    pub fn messages_route(responses: Vec<(u16, String)>) -> Route {
        Route::sequence("POST", "/v1/messages", responses)
    }
}
