//! Shared loopback Alpaca Trading API stand-in. No production credentials or network.
use hyper::{body::to_bytes, service::service_fn, Body, Request, Response};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::{net::TcpListener, task::JoinHandle, task::JoinSet};

pub const KEY_ID: &str = "mock-alpaca-key";
pub const SECRET_KEY: &str = "mock-alpaca-secret";
const TIME: &str = "2026-11-10T15:00:00Z";

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: hyper::HeaderMap,
    pub body: Vec<u8>,
}

#[derive(Default)]
pub enum PlaceReply {
    #[default]
    Normal,
    Status(u16),
    /// Commit the order, then close the connection without an HTTP response.
    Disconnect,
}

struct State {
    account: Value,
    positions: Vec<Value>,
    orders: Vec<Value>,
    polls: HashMap<String, VecDeque<Value>>,
    next_place: PlaceReply,
    requests: Vec<RecordedRequest>,
}

pub struct MockAlpaca {
    base_url: String,
    state: Arc<Mutex<State>>,
    task: JoinHandle<()>,
}

impl Drop for MockAlpaca {
    fn drop(&mut self) {
        // Dropping the accept loop's JoinSet also aborts active connections.
        self.task.abort();
    }
}

impl MockAlpaca {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            account: json!({
                "id": "00000000-0000-4000-8000-000000000001",
                "account_number": "MOCK001", "status": "ACTIVE", "currency": "USD",
                "cash": "10.00", "buying_power": "10.00", "equity": "10.00",
                "last_equity": "10.00", "portfolio_value": "10.00",
                "trading_blocked": false, "account_blocked": false,
                "trade_suspended_by_user": false, "shorting_enabled": false
            }),
            positions: vec![],
            orders: vec![],
            polls: HashMap::new(),
            next_place: PlaceReply::Normal,
            requests: vec![],
        }));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let state = shared.clone();
                        connections.spawn(async move {
                            let _ = hyper::server::conn::Http::new()
                                .http1_only(true)
                                .serve_connection(socket, service_fn(move |req| handle(req, state.clone())))
                                .await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            base_url,
            state,
            task,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().unwrap().requests.clone()
    }

    /// Explicit venue snapshots, independent of any local book or risk implementation.
    pub fn set_account(&self, account: Value) {
        self.state.lock().unwrap().account = account;
    }

    pub fn set_positions(&self, positions: Vec<Value>) {
        self.state.lock().unwrap().positions = positions;
    }

    pub fn seed_order(&self, order: Value) {
        let mut state = self.state.lock().unwrap();
        assert!(order["id"].is_string() && order["client_order_id"].is_string());
        assert!(!state
            .orders
            .iter()
            .any(|o| o["id"] == order["id"] || o["client_order_id"] == order["client_order_id"]));
        state.orders.push(order);
    }

    /// Patches applied on successive detail/ID lookups; the last snapshot persists.
    /// List requests do not advance the script. Cancellation clears it.
    pub fn script_order(&self, id: &str, patches: Vec<Value>) {
        let mut state = self.state.lock().unwrap();
        assert!(state.orders.iter().any(|o| o["id"] == id));
        assert!(patches
            .iter()
            .all(|v| v.is_object() && v.get("id").is_none() && v.get("client_order_id").is_none()));
        state.polls.insert(id.into(), patches.into());
    }

    pub fn next_place_reply(&self, reply: PlaceReply) {
        self.state.lock().unwrap().next_place = reply;
    }
}

fn header<'a>(request: &'a RecordedRequest, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|v| v.to_str().ok())
}

fn authorized(request: &RecordedRequest) -> bool {
    header(request, "APCA-API-KEY-ID") == Some(KEY_ID)
        && header(request, "APCA-API-SECRET-KEY") == Some(SECRET_KEY)
}

async fn handle(
    req: Request<Body>,
    state: Arc<Mutex<State>>,
) -> Result<Response<Body>, std::io::Error> {
    let (parts, body) = req.into_parts();
    let body = to_bytes(body).await.map_err(std::io::Error::other)?;
    let request = RecordedRequest {
        method: parts.method.to_string(),
        path: parts.uri.to_string(),
        headers: parts.headers,
        body: body.to_vec(),
    };
    let mut state = state.lock().unwrap();
    state.requests.push(request.clone());
    if !authorized(&request) {
        return Ok(error(401, "unauthorized"));
    }
    route(&mut state, &request)
}

fn route(state: &mut State, request: &RecordedRequest) -> Result<Response<Body>, std::io::Error> {
    let url = reqwest::Url::parse(&format!("http://localhost{}", request.path)).unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let (method, path) = (request.method.as_str(), url.path());
    let response = match (method, path) {
        ("GET", "/v2/account") => response(200, state.account.clone()),
        ("GET", "/v2/positions") => response(200, json!(state.positions)),
        ("GET", "/v2/orders") => list_orders(state, &query),
        ("POST", "/v2/orders") => return place(state, request),
        ("GET", "/v2/orders:by_client_order_id") => by_client_id(state, &query),
        _ if path.starts_with("/v2/orders/") => {
            order_by_id(state, method, &path["/v2/orders/".len()..])
        }
        _ => error(404, "unsupported mock route"),
    };
    Ok(response)
}

fn by_client_id(state: &mut State, query: &HashMap<String, String>) -> Response<Body> {
    let index = query.get("client_order_id").and_then(|id| {
        state
            .orders
            .iter()
            .position(|o| o["client_order_id"] == *id)
    });
    lookup(state, index)
}

fn order_by_id(state: &mut State, method: &str, id: &str) -> Response<Body> {
    let index = state.orders.iter().position(|o| o["id"] == id);
    match (method, index) {
        ("GET", _) => lookup(state, index),
        ("DELETE", Some(i)) => cancel_order(state, id, i),
        ("DELETE", None) => error(404, "order not found"),
        _ => error(404, "unsupported mock route"),
    }
}

fn cancel_order(state: &mut State, id: &str, i: usize) -> Response<Body> {
    if closed(&state.orders[i]) {
        return error(422, "order is not cancelable");
    }
    state.polls.remove(id);
    state.orders[i]["status"] = json!("canceled");
    state.orders[i]["canceled_at"] = json!(TIME);
    Response::builder().status(204).body(Body::empty()).unwrap()
}

fn response(status: u16, body: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn error(status: u16, message: &str) -> Response<Body> {
    response(
        status,
        json!({"code": u32::from(status) * 10000, "message": message}),
    )
}

fn closed(order: &Value) -> bool {
    matches!(
        order["status"].as_str(),
        Some("filled" | "canceled" | "expired" | "rejected" | "replaced")
    )
}

fn lookup(state: &mut State, index: Option<usize>) -> Response<Body> {
    let Some(i) = index else {
        return error(404, "order not found");
    };
    let id = state.orders[i]["id"].as_str().unwrap();
    if let Some(patch) = state.polls.get_mut(id).and_then(VecDeque::pop_front) {
        for (key, value) in patch.as_object().unwrap() {
            state.orders[i][key] = value.clone();
        }
    }
    response(200, state.orders[i].clone())
}

fn list_orders(state: &State, query: &HashMap<String, String>) -> Response<Body> {
    if query.keys().any(|k| {
        !matches!(
            k.as_str(),
            "status" | "symbols" | "side" | "limit" | "direction"
        )
    }) {
        return error(422, "unsupported mock order filter");
    }
    let status = query.get("status").map(String::as_str).unwrap_or("open");
    if !matches!(status, "open" | "closed" | "all") {
        return error(422, "invalid status filter");
    }
    let mut rows: Vec<_> = state
        .orders
        .iter()
        .filter(|o| {
            (status == "all" || (status == "closed") == closed(o))
                && query
                    .get("symbols")
                    .is_none_or(|s| s.split(',').any(|s| o["symbol"] == s))
                && query.get("side").is_none_or(|s| o["side"] == *s)
        })
        .cloned()
        .collect();
    if query.get("direction").is_none_or(|d| d != "asc") {
        rows.reverse();
    }
    rows.truncate(
        query
            .get("limit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(50),
    );
    response(200, json!(rows))
}

fn positive(value: &Value) -> bool {
    value
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| value.as_f64())
        .is_some_and(|n| n.is_finite() && n > 0.0)
}

fn valid_shape(order: &Value) -> bool {
    order["type"] == "limit"
        && order["time_in_force"] == "day"
        && matches!(order["side"].as_str(), Some("buy" | "sell"))
        && order["symbol"].as_str().is_some_and(|s| !s.is_empty())
        && order["client_order_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 128)
}

fn size_field(order: &Value) -> Option<&Value> {
    order.get("qty").or_else(|| order.get("notional"))
}

fn valid_size(order: &Value) -> bool {
    order.get("qty").is_some() != order.get("notional").is_some()
        && size_field(order).is_some_and(positive)
        && positive(&order["limit_price"])
        && order.get("extended_hours").is_none_or(|v| v == false)
}

fn valid_order(order: &Value) -> bool {
    order.is_object() && valid_shape(order) && valid_size(order)
}

fn client_id_taken(state: &State, order: &Value) -> bool {
    state
        .orders
        .iter()
        .any(|o| o["client_order_id"] == order["client_order_id"])
}

// Preserve decimal strings; responses always include both nullable size fields.
fn normalize_sizes(order: &mut Value) {
    for key in ["qty", "notional", "limit_price"] {
        order[key] = match order.get(key) {
            Some(Value::Number(n)) => json!(n.to_string()),
            Some(v) => v.clone(),
            None => Value::Null,
        };
    }
}

fn assign_order(state: &State, order: &mut Value) {
    let mut sequence = state.orders.len() + 100;
    loop {
        let id = format!("00000000-0000-4000-8000-{sequence:012}");
        if !state.orders.iter().any(|o| o["id"] == id) {
            order["id"] = json!(id);
            break;
        }
        sequence += 1;
    }
    order["status"] = json!("new");
    order["filled_qty"] = json!("0");
    order["filled_avg_price"] = Value::Null;
    order["asset_class"] = json!("us_equity");
    order["asset_id"] = json!("00000000-0000-4000-8000-000000000002");
    order["order_class"] = json!("simple");
    order["extended_hours"] = json!(false);
    for key in ["created_at", "updated_at", "submitted_at"] {
        order[key] = json!(TIME);
    }
    for key in ["filled_at", "canceled_at", "expired_at", "failed_at"] {
        order[key] = Value::Null;
    }
}

fn place(state: &mut State, request: &RecordedRequest) -> Result<Response<Body>, std::io::Error> {
    let Ok(mut order) = serde_json::from_slice::<Value>(&request.body) else {
        return Ok(error(422, "invalid JSON"));
    };
    if !valid_order(&order) {
        return Ok(error(
            422,
            "expected day limit order with client_order_id and exactly one size",
        ));
    }
    if client_id_taken(state, &order) {
        return Ok(error(422, "client_order_id must be unique"));
    }
    let reply = std::mem::take(&mut state.next_place);
    if let PlaceReply::Status(status) = reply {
        return Ok(error(status, "injected place failure"));
    }
    normalize_sizes(&mut order);
    assign_order(state, &mut order);
    state.orders.push(order.clone());
    if matches!(reply, PlaceReply::Disconnect) {
        return Err(std::io::Error::other("injected lost POST response"));
    }
    Ok(response(200, order))
}
