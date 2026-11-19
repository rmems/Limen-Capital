//! Harness contract tests, not a substitute for RM-2460/2462/2463 client suites.
#![cfg(feature = "live-alpaca")]

mod support;

use reqwest::{Client, Method, RequestBuilder};
use serde_json::{json, Value};
use std::time::Duration;
use support::alpaca::{MockAlpaca, PlaceReply, RecordedRequest, KEY_ID, SECRET_KEY};

fn http() -> Client {
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

/// One mock venue plus an authenticated HTTP client bound to it.
struct Session {
    server: MockAlpaca,
    client: Client,
}

impl Session {
    async fn start() -> Self {
        Self {
            server: MockAlpaca::start().await,
            client: http(),
        }
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.server.base_url()))
            .header("APCA-API-KEY-ID", KEY_ID)
            .header("APCA-API-SECRET-KEY", SECRET_KEY)
    }

    fn raw(&self, method: Method, path: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.server.base_url()))
    }

    async fn get(&self, path: &str) -> Value {
        self.request(Method::GET, path)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn status(&self, method: Method, path: &str) -> u16 {
        self.request(method, path)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    async fn place(&self, id: &str) -> Value {
        self.request(Method::POST, "/v2/orders")
            .json(&order(id))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn post_status(&self, id: &str) -> u16 {
        self.request(Method::POST, "/v2/orders")
            .json(&order(id))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.server.requests()
    }

    /// A fresh HTTP client represents reconnect/restart; the venue retains state.
    fn reconnect(&mut self) {
        self.client = http();
    }
}

fn order(client_id: &str) -> Value {
    json!({
        "symbol": "SPY", "side": "buy", "type": "limit", "time_in_force": "day",
        "qty": "0.01", "limit_price": "600.00", "client_order_id": client_id
    })
}

#[tokio::test]
async fn key_pair_required_on_every_route_before_mutating_state() {
    let session = Session::start().await;
    let routes = [
        (Method::GET, "/v2/account"),
        (Method::GET, "/v2/positions"),
        (Method::GET, "/v2/orders"),
        (Method::POST, "/v2/orders"),
        (
            Method::GET,
            "/v2/orders:by_client_order_id?client_order_id=test",
        ),
        (Method::GET, "/v2/orders/absent"),
        (Method::DELETE, "/v2/orders/absent"),
    ];
    for (method, path) in routes {
        for (key, secret) in [
            (None, None),
            (Some(KEY_ID), None),
            (None, Some(SECRET_KEY)),
            (Some("wrong"), Some(SECRET_KEY)),
            (Some(KEY_ID), Some("wrong")),
        ] {
            let mut req = session.raw(method.clone(), path);
            if let Some(key) = key {
                req = req.header("APCA-API-KEY-ID", key);
            }
            if let Some(secret) = secret {
                req = req.header("APCA-API-SECRET-KEY", secret);
            }
            assert_eq!(req.json(&order("test")).send().await.unwrap().status(), 401);
        }
    }
    assert_eq!(session.get("/v2/orders?status=all").await, json!([]));
    assert_eq!(session.get("/v2/account").await["equity"], "10.00");
}

async fn assert_scripted_polls(session: &Session, client_id: &str) {
    let placed = session.place(client_id).await;
    assert_eq!(placed["filled_qty"], "0");
    assert_eq!(placed["qty"], "0.01");
    assert!(placed["notional"].is_null());
    let id = placed["id"].as_str().unwrap();
    session.server.script_order(id, vec![
        json!({"status": "partially_filled", "filled_qty": "0.004", "filled_avg_price": "599.90"}),
        json!({"status": "future_venue_status", "filled_qty": "0.004"}),
        json!({"status": "filled", "filled_qty": "0.01"}),
    ]);
    let path = format!("/v2/orders/{id}");
    assert_eq!(session.get(&path).await["filled_qty"], "0.004");
    assert_eq!(
        session
            .get(&format!(
                "/v2/orders:by_client_order_id?client_order_id={client_id}"
            ))
            .await["status"],
        "future_venue_status"
    );
    assert_eq!(session.get("/v2/orders").await.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn place_and_scripted_polls_report_wire_shapes() {
    let session = Session::start().await;
    assert_scripted_polls(&session, "order-1").await;
}

fn assert_request_counts(requests: &[RecordedRequest]) {
    let posts = requests.iter().filter(|r| r.method == "POST").count();
    let deletes = requests.iter().filter(|r| r.method == "DELETE").count();
    assert_eq!((posts, deletes), (1, 1));
}

fn assert_first_request_shape(requests: &[RecordedRequest]) {
    let first = &requests[0];
    assert_eq!(first.path, "/v2/orders");
    assert_eq!(first.headers["apca-api-key-id"], KEY_ID);
    assert_eq!(first.headers["apca-api-secret-key"], SECRET_KEY);
    assert_eq!(
        serde_json::from_slice::<Value>(&first.body).unwrap(),
        order("order-1")
    );
}

fn assert_recorded_wire_shape(requests: &[RecordedRequest]) {
    assert_request_counts(requests);
    assert_first_request_shape(requests);
}

#[tokio::test]
async fn cancel_clears_script_and_records_real_wire_shapes() {
    let session = Session::start().await;
    let placed = session.place("order-1").await;
    let path = format!("/v2/orders/{}", placed["id"].as_str().unwrap());
    session.server.script_order(
        placed["id"].as_str().unwrap(),
        vec![json!({"status": "partially_filled", "filled_qty": "0.004"})],
    );
    assert_eq!(session.get(&path).await["filled_qty"], "0.004");
    let canceled = session.request(Method::DELETE, &path).send().await.unwrap();
    assert_eq!(canceled.status(), 204);
    assert!(canceled.bytes().await.unwrap().is_empty());
    assert_eq!(session.get(&path).await["status"], "canceled");
    assert_eq!(session.get(&path).await["filled_qty"], "0.004");
    assert_eq!(session.get("/v2/orders").await, json!([]));
    assert_recorded_wire_shape(&session.requests());
}

#[tokio::test]
async fn place_failure_and_lost_response_are_one_shot_and_recoverable() {
    let mut session = Session::start().await;
    session.server.next_place_reply(PlaceReply::Status(500));
    assert_eq!(session.post_status("failure").await, 500);
    assert_eq!(session.get("/v2/orders?status=all").await, json!([]));
    session.server.next_place_reply(PlaceReply::Disconnect);
    assert!(session
        .request(Method::POST, "/v2/orders")
        .json(&order("lost /+?"))
        .send()
        .await
        .is_err());
    session.reconnect();
    let recovered = session
        .request(Method::GET, "/v2/orders:by_client_order_id")
        .query(&[("client_order_id", "lost /+?")])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(recovered["client_order_id"], "lost /+?");
    assert_eq!(
        session.get("/v2/orders?status=all").await,
        json!([recovered])
    );
    assert_injection_consumed(&session).await;
}

// The injection was consumed, rather than repeated for later requests.
async fn assert_injection_consumed(session: &Session) {
    assert_eq!(session.post_status("lost /+?").await, 422);
    let posts = session
        .requests()
        .iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(posts, 3);
    assert_eq!(session.place("later").await["status"], "new");
}

#[tokio::test]
async fn snapshots_and_seeded_orders_survive_client_restart() {
    let mut session = Session::start().await;
    session.server.set_account(
        json!({"id": "account-1", "equity": "9.20", "cash": "3.20", "buying_power": "3.20"}),
    );
    session.server.set_positions(vec![json!({"symbol": "SPY", "qty": "0.01", "avg_entry_price": "600.00",
        "market_value": "6.00", "unrealized_pl": "0.00", "side": "long", "asset_class": "us_equity"})]);
    let mut seeded = session.place("previous-process").await;
    // A separate venue order, already present when a new process connects.
    // Deliberately occupy the next generated venue ID.
    seeded["id"] = json!("00000000-0000-4000-8000-000000000102");
    seeded["client_order_id"] = json!("prior-client");
    seeded["status"] = json!("filled");
    seeded["filled_qty"] = json!("0.01");
    seeded["filled_avg_price"] = json!("600.00");
    session.server.seed_order(seeded.clone());
    session.reconnect();
    assert_eq!(session.get("/v2/account").await["equity"], "9.20");
    assert_eq!(session.get("/v2/positions").await[0]["qty"], "0.01");
    assert_seeded_order_visible(&session, &seeded).await;
    let next = session.place("after-restart").await;
    assert_ne!(next["id"], "00000000-0000-4000-8000-000000000102");
}

async fn assert_seeded_order_visible(session: &Session, seeded: &Value) {
    assert_eq!(
        session
            .get("/v2/orders?symbols=SPY&status=closed&side=buy")
            .await,
        json!([seeded.clone()])
    );
    assert_eq!(
        session
            .get("/v2/orders?status=all&direction=asc&limit=1")
            .await[0]["client_order_id"],
        "previous-process"
    );
    assert_eq!(session.get("/v2/orders?symbols=QQQ").await, json!([]));
}

#[tokio::test]
async fn fills_expiry_rejection_and_unknown_status_are_explicit_scripts() {
    let session = Session::start().await;
    for status in ["filled", "expired", "rejected", "future_status"] {
        let placed = session.place(status).await;
        let id = placed["id"].as_str().unwrap();
        session.server.script_order(
            id,
            vec![json!({"status": status, "filled_qty": "0.01", "filled_avg_price": "599.95"})],
        );
        let path = format!("/v2/orders/{id}");
        for _ in 0..2 {
            let snapshot = session.get(&path).await;
            assert_eq!(snapshot["status"], status);
            assert_eq!(snapshot["filled_qty"], "0.01");
        }
        if status != "future_status" {
            assert_eq!(session.status(Method::DELETE, &path).await, 422);
        }
    }
}

fn invalid_order_bodies() -> Vec<Value> {
    let mut invalid = vec![json!(null), json!({}), json!({"type": "market"})];
    for (field, value) in [
        ("type", json!("market")),
        ("time_in_force", json!("gtc")),
        ("qty", json!("-1")),
        ("qty", json!(null)),
        ("qty", json!("NaN")),
        ("qty", json!("inf")),
        ("qty", json!("0")),
        ("limit_price", json!(0)),
        ("client_order_id", json!("")),
        ("side", json!("bid")),
        ("extended_hours", json!(true)),
        ("notional", json!("1")),
    ] {
        let mut body = order("bad");
        body[field] = value;
        invalid.push(body);
    }
    let mut missing_size = order("bad");
    missing_size.as_object_mut().unwrap().remove("qty");
    invalid.push(missing_size);
    invalid
}

async fn assert_bodies_rejected(session: &Session) {
    for body in invalid_order_bodies() {
        assert_eq!(
            session
                .request(Method::POST, "/v2/orders")
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            422,
            "{body}"
        );
    }
    // Raw malformed JSON body is still rejected.
    assert_eq!(
        session
            .request(Method::POST, "/v2/orders")
            .body("{")
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
}

async fn assert_unknown_routes_404(session: &Session) {
    for path in [
        "/v2/orders/absent",
        "/v2/orders:by_client_order_id?client_order_id=absent",
        "/v2/orders:by_client_order_id",
        "/api/v2/crypto/trading/accounts/",
    ] {
        assert_eq!(session.status(Method::GET, path).await, 404, "{path}");
    }
}

// The ticket's notional-sized limit variant is deliberately supported.
async fn assert_notional_accepted(session: &Session) {
    let mut notional = order("notional");
    notional.as_object_mut().unwrap().remove("qty");
    notional["notional"] = json!("1.00");
    let placed: Value = session
        .request(Method::POST, "/v2/orders")
        .json(&notional)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(placed["notional"], "1.00");
    assert!(placed["qty"].is_null());
}

#[tokio::test]
async fn rejects_wrong_order_shapes_and_unknown_routes_without_creating_orders() {
    let session = Session::start().await;
    assert_bodies_rejected(&session).await;
    assert_unknown_routes_404(&session).await;
    assert_eq!(session.get("/v2/orders?status=all").await, json!([]));
    assert_notional_accepted(&session).await;
}

#[tokio::test]
async fn parallel_servers_are_isolated_and_drop_stops_connections() {
    let first = Session::start().await;
    let second = Session::start().await;
    assert_ne!(first.server.base_url(), second.server.base_url());
    first.place("same-id").await;
    second.place("same-id").await;
    let url = first.server.base_url().to_owned();
    let client = http();
    drop(first);
    tokio::task::yield_now().await;
    assert!(client
        .get(format!("{url}/v2/account"))
        .send()
        .await
        .is_err());
    assert_eq!(second.get("/v2/orders").await.as_array().unwrap().len(), 1);
}
