use super::*;
use crate::live::test_support::{MockServer, Route};
use serde_json::{json, Value};

const CLIENT_ID: &str = "131de903-5a9c-4260-abc1-28d562a5dcf0";
fn order(status: &str, filled: &str) -> String {
    json!({
        "id": "venue-id", "client_order_id": CLIENT_ID, "symbol": "AAPL",
        "side": "buy", "status": status, "filled_qty": filled, "filled_avg_price": null
    })
    .to_string()
}
fn request() -> PlaceLimitOrderRequest {
    PlaceLimitOrderRequest {
        client_order_id: CLIENT_ID.into(),
        symbol: "AAPL".into(),
        side: OrderSide::Buy,
        order_type: LimitOnly::Limit,
        size: LimitSize::Qty {
            qty: "0.025".into(),
        },
        limit_price: "200.00".into(),
        time_in_force: TimeInForce::Day,
        extended_hours: false,
    }
}
fn client(server: &MockServer) -> AlpacaClient {
    AlpacaClient::new("mock-key", "mock-secret")
        .unwrap()
        .with_mock_url(&server.base_url())
}
fn lookup() -> String {
    format!("/v2/orders:by_client_order_id?client_order_id={CLIENT_ID}")
}

#[tokio::test]
async fn places_exact_limit_json_with_environment_pair_headers() {
    let server = MockServer::start(vec![Route::status(
        "POST",
        "/v2/orders",
        201,
        &order("new", "0"),
    )])
    .await;
    let result = client(&server).place_limit_order(&request()).await.unwrap();
    assert_eq!(result.status, OrderState::Open);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let r = &requests[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/v2/orders");
    assert_eq!(r.header("APCA-API-KEY-ID"), Some("mock-key"));
    assert_eq!(r.header("APCA-API-SECRET-KEY"), Some("mock-secret"));
    assert_eq!(r.header("x-signature"), None);
    assert_eq!(
        serde_json::from_str::<Value>(&r.body).unwrap(),
        json!({
            "client_order_id": CLIENT_ID, "symbol": "AAPL", "side": "buy", "type": "limit",
            "qty": "0.025", "limit_price": "200.00", "time_in_force": "day", "extended_hours": false
        })
    );
}

#[tokio::test]
async fn notional_size_has_no_qty_field() {
    let server = MockServer::start(vec![Route::ok(
        "POST",
        "/v2/orders",
        &order("accepted", "0"),
    )])
    .await;
    let mut req = request();
    req.size = LimitSize::Notional {
        notional: "5.00".into(),
    };
    client(&server).place_limit_order(&req).await.unwrap();
    let body: Value = serde_json::from_str(&server.requests().await[0].body).unwrap();
    assert_eq!(body["notional"], "5.00");
    assert!(body.get("qty").is_none());
    assert_eq!(body["type"], "limit");
    assert_eq!(body["time_in_force"], "day");
}

#[tokio::test]
async fn server_error_is_never_retried_and_failed_lookup_stays_ambiguous() {
    let server = MockServer::start(vec![
        Route::status("POST", "/v2/orders", 500, "{}"),
        Route::status("GET", &lookup(), 404, "{}"),
    ])
    .await;
    let result = client(&server).place_limit_order(&request()).await;
    assert!(
        matches!(result, Err(ClientError::SubmissionUnknown { client_order_id }) if client_order_id == CLIENT_ID)
    );
    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests.iter().filter(|r| r.method == "POST").count(), 1);
    assert_eq!(requests[1].path, lookup());
}

#[tokio::test]
async fn lost_post_response_recovers_by_client_id_without_second_order() {
    for (status, body) in [(0, ""), (200, "truncated-json"), (500, "{}")] {
        let server = MockServer::start(vec![
            Route::status("POST", "/v2/orders", status, body),
            Route::ok("GET", &lookup(), &order("partially_filled", "0.01")),
        ])
        .await;
        let result = client(&server).place_limit_order(&request()).await.unwrap();
        assert_eq!(result.id, "venue-id");
        assert_eq!(result.filled_qty, 0.01);
        let requests = server.requests().await;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[1].method, "GET");
        assert_eq!(requests[1].path, lookup());
    }
}

#[tokio::test]
async fn rejected_post_is_not_retried() {
    let server = MockServer::start(vec![Route::status("POST", "/v2/orders", 422, "{}")]).await;
    assert!(matches!(
        client(&server).place_limit_order(&request()).await,
        Err(ClientError::Api { status: 422 })
    ));
    assert_eq!(server.requests().await.len(), 1);
}

#[tokio::test]
async fn cancel_sends_one_delete_and_accepts_empty_204() {
    for status in [204, 500] {
        let server = MockServer::start(vec![Route::status(
            "DELETE",
            "/v2/orders/venue-id",
            status,
            "",
        )])
        .await;
        let result = client(&server).cancel_order("venue-id").await;
        assert_eq!(result.is_ok(), status == 204);
        let requests = server.requests().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "DELETE");
        assert_eq!(requests[0].path, "/v2/orders/venue-id");
        assert!(requests[0].body.is_empty());
    }
}

#[tokio::test]
async fn partial_fill_snapshots_are_cumulative_not_double_counted() {
    let server = MockServer::start(vec![Route::sequence(
        "GET",
        "/v2/orders/venue-id",
        vec![
            (200, order("new", "0")),
            (200, order("partially_filled", "0.01")),
            (200, order("partially_filled", "0.01")),
            (200, order("partially_filled", "0.02")),
            (200, order("filled", "0.025")),
        ],
    )])
    .await;
    let result = client(&server)
        .poll_order("venue-id", 10, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(result.status, OrderState::Filled);
    assert_eq!(result.filled_qty, 0.025);
    assert_eq!(server.requests().await.len(), 5);
}

#[tokio::test]
async fn every_terminal_state_stops_polling_including_close_expiry() {
    for state in ["filled", "canceled", "expired", "rejected"] {
        let server = MockServer::start(vec![Route::ok(
            "GET",
            "/v2/orders/venue-id",
            &order(state, "0.01"),
        )])
        .await;
        let result = client(&server)
            .poll_order("venue-id", 5, Duration::ZERO)
            .await
            .unwrap();
        assert!(result.status.is_terminal(), "{state}");
        assert_eq!(result.filled_qty, 0.01);
        assert_eq!(server.requests().await.len(), 1);
    }
}

#[tokio::test]
async fn unknown_status_is_neither_terminal_nor_a_fill_and_keeps_polling() {
    let server = MockServer::start(vec![Route::sequence(
        "GET",
        "/v2/orders/venue-id",
        vec![
            (200, order("future_status", "0")),
            (200, order("pending_cancel", "0")),
            (200, order("expired", "0")),
        ],
    )])
    .await;
    let c = client(&server);
    let first = c.get_order("venue-id").await.unwrap();
    assert_eq!(first.status, OrderState::Unknown);
    assert!(!first.status.is_terminal());
    assert_ne!(first.status, OrderState::Filled);
    let result = c.poll_order("venue-id", 5, Duration::ZERO).await.unwrap();
    assert_eq!(result.status, OrderState::Expired);
    assert_eq!(server.requests().await.len(), 3);

    let server = MockServer::start(vec![Route::ok(
        "GET",
        "/v2/orders/venue-id",
        &order("future_status", "0"),
    )])
    .await;
    let result = client(&server)
        .poll_order("venue-id", 2, Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(result.status, OrderState::Unknown);
    assert_eq!(server.requests().await.len(), 2);
}

#[tokio::test]
async fn poll_budget_returns_still_open_order_and_list_includes_closed() {
    let server = MockServer::start(vec![
        Route::ok("GET", "/v2/orders/venue-id", &order("new", "0")),
        Route::ok(
            "GET",
            "/v2/orders?status=all&limit=500",
            &format!("[{}]", order("expired", "0")),
        ),
    ])
    .await;
    let c = client(&server);
    let result = c.poll_order("venue-id", 2, Duration::ZERO).await.unwrap();
    assert_eq!(result.status, OrderState::Open);
    assert!(!result.status.is_terminal());
    assert_eq!(c.get_orders().await.unwrap()[0].status, OrderState::Expired);
    assert_eq!(server.requests().await.len(), 3);
    assert!(c.poll_order("venue-id", 0, Duration::ZERO).await.is_err());
    assert_eq!(server.requests().await.len(), 3);
}

#[test]
fn fixed_live_endpoint_and_debug_never_expose_keys() {
    let c = AlpacaClient::new("secret-id-value", "secret-key-value").unwrap();
    assert_eq!(c.base_url(), "https://api.alpaca.markets");
    assert!(!format!("{c:?}").contains("secret"));
    assert!(AlpacaClient::new("", "secret").is_err());
    assert!(AlpacaClient::new("id", "bad\nheader").is_err());
}

#[tokio::test]
async fn invalid_requests_never_reach_the_venue() {
    async fn expect_rejected(c: &AlpacaClient, r: &PlaceLimitOrderRequest) {
        assert!(c.place_limit_order(r).await.is_err(), "{r:?}");
    }
    let server = MockServer::start(vec![]).await;
    let c = client(&server);
    for bad in ["0", "-1", "NaN", "inf", "invalid"] {
        let mut r = request();
        r.limit_price = bad.into();
        expect_rejected(&c, &r).await;
        let mut r = request();
        r.size = LimitSize::Qty { qty: bad.into() };
        expect_rejected(&c, &r).await;
    }
    let mut r = request();
    r.client_order_id = "not-a-uuid".into();
    expect_rejected(&c, &r).await;
    let mut r = request();
    r.extended_hours = true;
    expect_rejected(&c, &r).await;
    assert!(server.requests().await.is_empty());
}

#[tokio::test]
async fn account_asset_and_iex_quotes_use_alpaca_shapes() {
    let server = MockServer::start(vec![
        Route::ok(
            "GET",
            "/v2/account",
            r#"{"status":"ACTIVE","buying_power":"10.00"}"#,
        ),
        Route::ok(
            "GET",
            "/v2/assets/AAPL",
            r#"{"symbol":"AAPL","tradable":true,"fractionable":true,"status":"active"}"#,
        ),
        Route::ok(
            "GET",
            "/v2/stocks/AAPL/quotes/latest?feed=iex",
            r#"{"symbol":"AAPL","quote":{"bp":199.99,"ap":200.01}}"#,
        ),
    ])
    .await;
    let c = client(&server);
    assert_eq!(c.get_account().await.unwrap().buying_power, "10.00");
    assert!(c.get_asset("AAPL").await.unwrap().fractionable);
    let q = c.best_bid_ask("AAPL").await.unwrap();
    assert_eq!(q.mid(), 200.0);
    assert!(q.is_sane());
    assert_eq!(server.requests().await.len(), 3);
}

#[test]
fn risk_approval_preserves_uuid_and_rounded_wire_values() {
    use crate::live::{decider::Decision, market_state::MarketState, risk::*};
    let state = MarketState::new(
        1,
        1,
        "AAPL",
        199.99,
        200.01,
        crate::live::market_state::AccountState {
            position_qty: 0.0,
            avg_cost_usd: None,
            cash_usd: 10.0,
            realized_pnl_usd: 0.0,
        },
        vec![],
    );
    let pair = PairRules {
        symbol: "AAPL".into(),
        asset_increment: 1e-9,
        ..PairRules::btc_usd_defaults()
    };
    let inputs = RiskInputs {
        now_secs: 1,
        last_order_secs: None,
        open_orders: 0,
        daily_pnl_usd: 0.0,
        llm_cost_today_usd: 0.0,
        resting_notional_usd: 0.0,
    };
    let RiskVerdict::Approved(approved) = check(
        &Decision::buy(5.0, "test"),
        &state,
        &pair,
        &RiskLimits::default(),
        &inputs,
    ) else {
        panic!("expected approval")
    };
    let req = PlaceLimitOrderRequest::from(&approved);
    assert_eq!(req.client_order_id, approved.client_order_id);
    assert!(uuid::Uuid::parse_str(&req.client_order_id).is_ok());
    assert_eq!(req.limit_price, approved.limit_price_str);
    assert_eq!(
        req.size,
        LimitSize::Qty {
            qty: approved.asset_qty_str
        }
    );
}
