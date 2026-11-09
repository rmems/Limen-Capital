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
        }
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
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let req_c = req_c.clone();
                let routes_c = routes_c.clone();
                tokio::spawn(async move {
                    // Read headers.
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let header_end;
                    loop {
                        let n = match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                            header_end = pos + 4;
                            break;
                        }
                    }
                    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                    let mut lines = head.split("\r\n");
                    let request_line = lines.next().unwrap_or("");
                    let mut parts = request_line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").to_string();
                    let mut headers = Vec::new();
                    let mut content_length = 0usize;
                    for l in lines {
                        if let Some((k, v)) = l.split_once(':') {
                            let (k, v) = (k.trim().to_string(), v.trim().to_string());
                            if k.eq_ignore_ascii_case("content-length") {
                                content_length = v.parse().unwrap_or(0);
                            }
                            headers.push((k, v));
                        }
                    }
                    let mut body = buf[header_end..].to_vec();
                    while body.len() < content_length {
                        let n = match sock.read(&mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        body.extend_from_slice(&tmp[..n]);
                    }
                    let body = String::from_utf8_lossy(&body).to_string();

                    let (status, resp_body) = {
                        let mut routes = routes_c.lock().await;
                        match routes
                            .iter_mut()
                            .find(|r| r.method == method && r.path == path)
                        {
                            Some(r) => {
                                if r.responses.len() > 1 {
                                    r.responses.pop_front().unwrap()
                                } else {
                                    r.responses.front().cloned().unwrap_or((500, String::new()))
                                }
                            }
                            None => (
                                404,
                                format!(
                                    r#"{{"type":"client_error","errors":[{{"attr":null,"detail":"no mock route for {method} {path}"}}]}}"#
                                ),
                            ),
                        }
                    };
                    req_c.lock().await.push(RecordedRequest {
                        method,
                        path,
                        headers,
                        body,
                    });
                    let reason = match status {
                        200 => "OK",
                        201 => "Created",
                        400 => "Bad Request",
                        401 => "Unauthorized",
                        404 => "Not Found",
                        429 => "Too Many Requests",
                        500 => "Internal Server Error",
                        _ => "Status",
                    };
                    let resp = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        resp_body.len(),
                        resp_body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
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

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
