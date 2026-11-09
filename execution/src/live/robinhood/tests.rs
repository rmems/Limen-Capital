use super::*;
use crate::live::test_support::{MockServer, Route};

const API_KEY: &str = "rh-api-6148effc-c0b1-486c-8940-a1d099456be6";
const PRIV: &str = "xQnTJVeQLmw1/Mg2YimEViSpw/SdJcgNXZ5kQkAXNPU=";
const PUB: &str = "jPItx4TLjcnSUnmnXQQyAKL4eJj3+oWNNMmmm2vATqk=";

fn client(server: &MockServer) -> RobinhoodClient {
    RobinhoodClient::new(API_KEY, PRIV, &server.base_url()).unwrap()
}

#[test]
fn query_string_encodes_minimal_set() {
    assert_eq!(query_string(&[]), "");
    assert_eq!(query_string(&[("symbol", "BTC-USD")]), "?symbol=BTC-USD");
    assert_eq!(
        query_string(&[("a", "1 2"), ("q", "0.1,1")]),
        "?a=1%202&q=0.1,1"
    );
}

#[test]
fn rejects_bad_base_url_and_bad_key() {
    assert!(matches!(
        RobinhoodClient::new(API_KEY, PRIV, "trading.robinhood.com"),
        Err(ClientError::BadBaseUrl(_))
    ));
    assert!(matches!(
        RobinhoodClient::new(API_KEY, "nope", DEFAULT_BASE_URL),
        Err(ClientError::Sign(_))
    ));
    let c = RobinhoodClient::new(API_KEY, PRIV, "https://trading.robinhood.com/").unwrap();
    assert_eq!(c.base_url(), DEFAULT_BASE_URL);
    assert_eq!(c.public_key_b64(), PUB);
    assert!(!format!("{c:?}").contains(PRIV));
}

#[tokio::test]
async fn get_account_signs_request_and_remembers_account_number() {
    let server = MockServer::start(vec![Route::ok(
            "GET",
            PATH_ACCOUNTS,
            r#"{"next":null,"previous":null,"results":[{"account_number":"ACC-1","status":"active","buying_power":"10.00","buying_power_currency":"USD"}]}"#,
        )])
        .await;
    let mut c = client(&server);
    let a = c.get_account().await.unwrap();
    assert_eq!(a.account_number, "ACC-1");
    assert_eq!(a.buying_power, 10.0);
    assert_eq!(c.account_number(), Some("ACC-1"));

    let reqs = server.requests().await;
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "GET");
    assert_eq!(r.path, PATH_ACCOUNTS);
    assert_eq!(r.header("x-api-key").unwrap(), API_KEY);
    assert!(
        r.verify_signature(API_KEY, PUB),
        "signature must verify with the registered public key"
    );
}

#[tokio::test]
async fn best_bid_ask_builds_query_and_parses() {
    let server = MockServer::start(vec![Route::ok(
        "GET",
        "/api/v2/crypto/marketdata/best_bid_ask/?symbol=BTC-USD",
        r#"{"results":[{"symbol":"BTC-USD","bid":"100000.00","ask":"100010.00"}]}"#,
    )])
    .await;
    let q = client(&server).best_bid_ask("BTC-USD").await.unwrap();
    assert_eq!(q.bid, 100_000.0);
    assert_eq!(q.ask, 100_010.0);
    let reqs = server.requests().await;
    assert!(reqs[0].verify_signature(API_KEY, PUB));
}

#[tokio::test]
async fn estimated_price_uses_the_v2_trading_path() {
    let server = MockServer::start(vec![Route::ok(
            "GET",
            "/api/v2/crypto/trading/estimated_price/?symbol=BTC-USD&side=bid&quantity=0.0001",
            r#"{"results":[{"symbol":"BTC-USD","side":"bid","quantity":"0.0001","bid":"100000","ask":"100010","fee_ratio":"0.0045","est_fee":"0.045","est_total_credit":"9.955"}]}"#,
        )])
        .await;
    let rows = client(&server)
        .estimated_price("BTC-USD", "bid", 0.0001)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].est_total_credit, Some(9.955));
    let reqs = server.requests().await;
    assert!(reqs[0]
        .path
        .starts_with("/api/v2/crypto/trading/estimated_price/"));
    assert!(reqs[0].verify_signature(API_KEY, PUB));
}

#[tokio::test]
async fn holdings_requires_account_number() {
    let server = MockServer::start(vec![]).await;
    let err = client(&server).holdings(None).await.unwrap_err();
    assert!(matches!(err, ClientError::MissingAccount));
    assert!(server.requests().await.is_empty());
}

#[tokio::test]
async fn holdings_and_pair_and_orders_paths() {
    let server = MockServer::start(vec![
            Route::ok(
                "GET",
                "/api/v2/crypto/trading/holdings/?account_number=ACC-1&asset_code=BTC",
                r#"{"results":[{"account_number":"ACC-1","asset_code":"BTC","total_quantity":"0.0001","quantity_available_for_trading":"0.0001"}]}"#,
            ),
            Route::ok(
                "GET",
                "/api/v2/crypto/trading/trading_pairs/?symbol=BTC-USD",
                r#"{"results":[{"symbol":"BTC-USD","asset_code":"BTC","quote_code":"USD","asset_increment":"0.00000001","quote_increment":"0.01","max_order_size":"100","min_order_amount":"1","status":"tradable","is_api_tradable":true}]}"#,
            ),
            Route::ok(
                "GET",
                "/api/v2/crypto/trading/orders/?account_number=ACC-1&symbol=BTC-USD&state=open",
                r#"{"results":[{"id":"o-1","symbol":"BTC-USD","side":"buy","type":"limit","state":"open"}]}"#,
            ),
            Route::ok(
                "GET",
                "/api/v1/crypto/trading/orders/o-1/",
                r#"{"id":"o-1","symbol":"BTC-USD","side":"buy","type":"limit","state":"filled","executions":[{"effective_price":"100000","quantity":"0.00001"}]}"#,
            ),
        ])
        .await;
    let c = client(&server).with_account_number("ACC-1");
    let h = c.holding("BTC").await.unwrap().unwrap();
    assert_eq!(h.total_quantity, 1e-4);
    assert!(c.holding("ETH").await.is_err()); // no route → 404 → Api error
    let p = c.trading_pair("BTC-USD").await.unwrap();
    assert_eq!(p.min_order_amount, Some(1.0));
    let open = c
        .get_orders(&OrderFilter {
            symbol: Some("BTC-USD".into()),
            side: None,
            state: Some(OrderState::Open),
        })
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    let o = c.get_order("o-1").await.unwrap();
    assert_eq!(o.state, OrderState::Filled);
    assert_eq!(o.filled_qty(), 1e-5);
}

#[tokio::test]
async fn api_error_is_surfaced_with_parsed_body() {
    let server = MockServer::start(vec![Route::status(
        "GET",
        PATH_ACCOUNTS,
        401,
        r#"{"type":"client_error","errors":[{"attr":null,"detail":"Invalid signature."}]}"#,
    )])
    .await;
    let err = client(&server).get_accounts().await.unwrap_err();
    match err {
        ClientError::Api { status, parsed, .. } => {
            assert_eq!(status, 401);
            assert_eq!(parsed.unwrap().errors[0].detail, "Invalid signature.");
        }
        other => panic!("expected Api error, got {other:?}"),
    }
    // 401 is not retryable: exactly one request.
    assert_eq!(server.requests().await.len(), 1);
}

#[tokio::test]
async fn get_retries_on_429_then_succeeds() {
    let server = MockServer::start(vec![Route::sequence(
        "GET",
        PATH_ACCOUNTS,
        vec![
            (429, r#"{"type":"client_error","errors":[]}"#.to_string()),
            (
                200,
                r#"{"results":[{"account_number":"ACC-1","buying_power":"1"}]}"#.to_string(),
            ),
        ],
    )])
    .await;
    let accounts = client(&server).get_accounts().await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(server.requests().await.len(), 2);
}

#[tokio::test]
async fn decode_error_when_schema_mismatch() {
    let server = MockServer::start(vec![Route::ok(
        "GET",
        PATH_ACCOUNTS,
        r#"{"results":[{"nope":1}]}"#,
    )])
    .await;
    let err = client(&server).get_accounts().await.unwrap_err();
    assert!(matches!(err, ClientError::Decode { .. }), "{err}");
}
