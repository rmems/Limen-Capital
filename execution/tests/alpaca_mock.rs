//! Harness contract tests, not a substitute for RM-2460/2462/2463 client suites.
#![cfg(feature = "live-alpaca")]

mod support;

use reqwest::{Client, Method, RequestBuilder};
use serde_json::{json, Value};
use std::time::Duration;
use support::alpaca::{MockAlpaca, PlaceReply, KEY_ID, SECRET_KEY};

fn http() -> Client {
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

fn request(client: &Client, server: &MockAlpaca, method: Method, path: &str) -> RequestBuilder {
    client
        .request(method, format!("{}{path}", server.base_url()))
        .header("APCA-API-KEY-ID", KEY_ID)
        .header("APCA-API-SECRET-KEY", SECRET_KEY)
}

fn order(client_id: &str) -> Value {
    json!({
        "symbol": "SPY", "side": "buy", "type": "limit", "time_in_force": "day",
        "qty": "0.01", "limit_price": "600.00", "client_order_id": client_id
    })
}

async fn get(client: &Client, server: &MockAlpaca, path: &str) -> Value {
    request(client, server, Method::GET, path)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn place(client: &Client, server: &MockAlpaca, id: &str) -> Value {
    request(client, server, Method::POST, "/v2/orders")
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

#[tokio::test]
async fn key_pair_required_on_every_route_before_mutating_state() {
    let server = MockAlpaca::start().await;
    let client = http();
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
            let mut req = client.request(method.clone(), format!("{}{path}", server.base_url()));
            if let Some(key) = key {
                req = req.header("APCA-API-KEY-ID", key);
            }
            if let Some(secret) = secret {
                req = req.header("APCA-API-SECRET-KEY", secret);
            }
            assert_eq!(req.json(&order("test")).send().await.unwrap().status(), 401);
        }
    }
    assert_eq!(
        get(&client, &server, "/v2/orders?status=all").await,
        json!([])
    );
    assert_eq!(
        get(&client, &server, "/v2/account").await["equity"],
        "10.00"
    );
}

async fn assert_scripted_polls(client: &Client, server: &MockAlpaca, id: &str) {
    server.script_order(id, vec![
        json!({"status": "partially_filled", "filled_qty": "0.004", "filled_avg_price": "599.90"}),
        json!({"status": "future_venue_status", "filled_qty": "0.004"}),
        json!({"status": "filled", "filled_qty": "0.01"}),
    ]);
    let path = format!("/v2/orders/{id}");
    assert_eq!(get(client, server, &path).await["filled_qty"], "0.004");
    assert_eq!(
        get(
            client,
            server,
            "/v2/orders:by_client_order_id?client_order_id=order-1"
        )
        .await["status"],
        "future_venue_status"
    );
    assert_eq!(
        get(client, server, "/v2/orders")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn place_poll_cancel_and_record_real_wire_shapes() {
    let server = MockAlpaca::start().await;
    let client = http();
    let placed = place(&client, &server, "order-1").await;
    assert_eq!(placed["filled_qty"], "0");
    assert_eq!(placed["qty"], "0.01");
    assert!(placed["notional"].is_null());
    let id = placed["id"].as_str().unwrap();
    assert_scripted_polls(&client, &server, id).await;
    let path = format!("/v2/orders/{id}");
    let canceled = request(&client, &server, Method::DELETE, &path)
        .send()
        .await
        .unwrap();
    assert_eq!(canceled.status(), 204);
    assert!(canceled.bytes().await.unwrap().is_empty());
    assert_eq!(get(&client, &server, &path).await["status"], "canceled");
    assert_eq!(get(&client, &server, &path).await["filled_qty"], "0.004");
    assert_eq!(get(&client, &server, "/v2/orders").await, json!([]));
    let requests = server.requests();
    assert_eq!(requests.iter().filter(|r| r.method == "POST").count(), 1);
    assert_eq!(requests.iter().filter(|r| r.method == "DELETE").count(), 1);
    assert_eq!(requests[0].path, "/v2/orders");
    assert_eq!(requests[0].headers["apca-api-key-id"], KEY_ID);
    assert_eq!(requests[0].headers["apca-api-secret-key"], SECRET_KEY);
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[0].body).unwrap(),
        order("order-1")
    );
}

async fn post_status(client: &Client, server: &MockAlpaca, id: &str) -> u16 {
    request(client, server, Method::POST, "/v2/orders")
        .json(&order(id))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

// A fresh HTTP client represents reconnect/restart; the venue retains the order.
async fn recover_lost_order(server: &MockAlpaca) -> Value {
    let client = http();
    request(
        &client,
        server,
        Method::GET,
        "/v2/orders:by_client_order_id",
    )
    .query(&[("client_order_id", "lost /+?")])
    .send()
    .await
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .await
    .unwrap()
}

#[tokio::test]
async fn place_failure_and_lost_response_are_one_shot_and_recoverable() {
    let server = MockAlpaca::start().await;
    let client = http();
    server.next_place_reply(PlaceReply::Status(500));
    assert_eq!(post_status(&client, &server, "failure").await, 500);
    assert_eq!(
        get(&client, &server, "/v2/orders?status=all").await,
        json!([])
    );
    server.next_place_reply(PlaceReply::Disconnect);
    assert!(request(&client, &server, Method::POST, "/v2/orders")
        .json(&order("lost /+?"))
        .send()
        .await
        .is_err());
    let recovered = recover_lost_order(&server).await;
    assert_eq!(recovered["client_order_id"], "lost /+?");
    assert_eq!(
        get(&client, &server, "/v2/orders?status=all").await,
        json!([recovered])
    );
    assert_eq!(post_status(&client, &server, "lost /+?").await, 422);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|r| r.method == "POST")
            .count(),
        3
    );
    // The injection was consumed, rather than repeated for later requests.
    assert_eq!(place(&client, &server, "later").await["status"], "new");
}

#[tokio::test]
async fn snapshots_and_seeded_orders_survive_client_restart() {
    let server = MockAlpaca::start().await;
    server.set_account(
        json!({"id": "account-1", "equity": "9.20", "cash": "3.20", "buying_power": "3.20"}),
    );
    server.set_positions(vec![json!({"symbol": "SPY", "qty": "0.01", "avg_entry_price": "600.00",
        "market_value": "6.00", "unrealized_pl": "0.00", "side": "long", "asset_class": "us_equity"})]);
    let client = http();
    let mut seeded = place(&client, &server, "previous-process").await;
    // A separate venue order, already present when a new process connects.
    // Deliberately occupy the next generated venue ID.
    seeded["id"] = json!("00000000-0000-4000-8000-000000000102");
    seeded["client_order_id"] = json!("prior-client");
    seeded["status"] = json!("filled");
    seeded["filled_qty"] = json!("0.01");
    seeded["filled_avg_price"] = json!("600.00");
    server.seed_order(seeded.clone());
    drop(client);
    let client = http();
    assert_eq!(get(&client, &server, "/v2/account").await["equity"], "9.20");
    assert_eq!(
        get(&client, &server, "/v2/positions").await[0]["qty"],
        "0.01"
    );
    assert_eq!(
        get(
            &client,
            &server,
            "/v2/orders?symbols=SPY&status=closed&side=buy"
        )
        .await,
        json!([seeded])
    );
    assert_eq!(
        get(
            &client,
            &server,
            "/v2/orders?status=all&direction=asc&limit=1"
        )
        .await[0]["client_order_id"],
        "previous-process"
    );
    assert_eq!(
        get(&client, &server, "/v2/orders?symbols=QQQ").await,
        json!([])
    );
    let next = place(&client, &server, "after-restart").await;
    assert_ne!(next["id"], "00000000-0000-4000-8000-000000000102");
}

#[tokio::test]
async fn fills_expiry_rejection_and_unknown_status_are_explicit_scripts() {
    let server = MockAlpaca::start().await;
    let client = http();
    for status in ["filled", "expired", "rejected", "future_status"] {
        let placed = place(&client, &server, status).await;
        let id = placed["id"].as_str().unwrap();
        server.script_order(
            id,
            vec![json!({"status": status, "filled_qty": "0.01", "filled_avg_price": "599.95"})],
        );
        let path = format!("/v2/orders/{id}");
        for _ in 0..2 {
            let snapshot = get(&client, &server, &path).await;
            assert_eq!(snapshot["status"], status);
            assert_eq!(snapshot["filled_qty"], "0.01");
        }
        if status != "future_status" {
            assert_eq!(
                request(&client, &server, Method::DELETE, &path)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                422
            );
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

async fn assert_status(
    client: &Client,
    server: &MockAlpaca,
    method: Method,
    path: &str,
    status: u16,
) {
    assert_eq!(
        request(client, server, method, path)
            .send()
            .await
            .unwrap()
            .status(),
        status,
        "{path}"
    );
}

async fn assert_bodies_rejected(client: &Client, server: &MockAlpaca) {
    for body in invalid_order_bodies() {
        assert_eq!(
            request(client, server, Method::POST, "/v2/orders")
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
        request(client, server, Method::POST, "/v2/orders")
            .body("{")
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
}

async fn assert_unknown_routes_404(client: &Client, server: &MockAlpaca) {
    for path in [
        "/v2/orders/absent",
        "/v2/orders:by_client_order_id?client_order_id=absent",
        "/v2/orders:by_client_order_id",
        "/api/v2/crypto/trading/accounts/",
    ] {
        assert_status(client, server, Method::GET, path, 404).await;
    }
}

// The ticket's notional-sized limit variant is deliberately supported.
async fn assert_notional_accepted(client: &Client, server: &MockAlpaca) {
    let mut notional = order("notional");
    notional.as_object_mut().unwrap().remove("qty");
    notional["notional"] = json!("1.00");
    let placed: Value = request(client, server, Method::POST, "/v2/orders")
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
    let server = MockAlpaca::start().await;
    let client = http();
    assert_bodies_rejected(&client, &server).await;
    assert_unknown_routes_404(&client, &server).await;
    assert_eq!(
        get(&client, &server, "/v2/orders?status=all").await,
        json!([])
    );
    assert_notional_accepted(&client, &server).await;
}

#[tokio::test]
async fn parallel_servers_are_isolated_and_drop_stops_connections() {
    let first = MockAlpaca::start().await;
    let second = MockAlpaca::start().await;
    assert_ne!(first.base_url(), second.base_url());
    let client = http();
    place(&client, &first, "same-id").await;
    place(&client, &second, "same-id").await;
    let url = first.base_url().to_owned();
    drop(first);
    tokio::task::yield_now().await;
    assert!(client
        .get(format!("{url}/v2/account"))
        .send()
        .await
        .is_err());
    assert_eq!(
        get(&client, &second, "/v2/orders")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
