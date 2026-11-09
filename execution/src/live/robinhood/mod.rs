//! Robinhood Crypto Trading API adapter.
//!
//! Docs: <https://docs.robinhood.com/crypto/trading/> (verified 2026-11-08).
//! Base URL `https://trading.robinhood.com`; every request carries
//! `x-api-key`, `x-signature`, `x-timestamp` (see [`sign`]).
//!
//! Only **read-only** calls live here for now (accounts, market data,
//! trading pairs, holdings, orders). Order placement / cancellation arrive
//! in a later step, gated behind `paper = false`.
//!
//! Rate limiting is a per-endpoint token bucket (100 req/min sustained,
//! 300 burst). GETs back off on `429` / `5xx` with exponential delay and
//! jitter; POSTs are **never** retried (idempotency is `client_order_id`).

pub mod sign;
pub mod types;

use rand::Rng;
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use sign::{RequestSigner, SignError};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};
use types::*;

pub const DEFAULT_BASE_URL: &str = "https://trading.robinhood.com";

const PATH_ACCOUNTS: &str = "/api/v2/crypto/trading/accounts/";
const PATH_BEST_BID_ASK: &str = "/api/v2/crypto/marketdata/best_bid_ask/";
// Note: in V2 only best_bid_ask lives under /marketdata/; estimated_price is under /trading/.
const PATH_ESTIMATED_PRICE: &str = "/api/v2/crypto/trading/estimated_price/";
const PATH_TRADING_PAIRS: &str = "/api/v2/crypto/trading/trading_pairs/";
const PATH_HOLDINGS: &str = "/api/v2/crypto/trading/holdings/";
const PATH_ORDERS: &str = "/api/v2/crypto/trading/orders/";
/// V2 has no order-by-id GET; the documented V1 detail endpoint is used instead.
const PATH_ORDER_DETAIL_V1: &str = "/api/v1/crypto/trading/orders/";

/// GET retry budget on 429 / 5xx.
const MAX_GET_RETRIES: u32 = 3;
const BACKOFF_BASE_MS: u64 = 500;

#[derive(Debug)]
pub enum ClientError {
    Sign(SignError),
    Http(reqwest::Error),
    /// Non-2xx from the venue (after retries, where applicable).
    Api {
        status: u16,
        method: String,
        path: String,
        parsed: Option<ApiErrorBody>,
        body: String,
    },
    /// 2xx but the body did not match the expected schema.
    Decode {
        path: String,
        source: serde_json::Error,
        body: String,
    },
    /// `account_number` needed but not configured and no account fetched yet.
    MissingAccount,
    /// A list endpoint returned zero rows where exactly one was expected.
    NotFound(String),
    /// Base URL did not parse.
    BadBaseUrl(String),
    /// The system clock is before the Unix epoch; no valid `x-timestamp` is possible.
    Clock(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Sign(e) => write!(f, "signing: {e}"),
            ClientError::Http(e) => write!(f, "http: {e}"),
            ClientError::Api {
                status,
                method,
                path,
                parsed,
                body,
            } => match parsed {
                Some(p) => write!(f, "{method} {path} → {status}: {p}"),
                None => write!(f, "{method} {path} → {status}: {}", truncate(body, 200)),
            },
            ClientError::Decode { path, source, body } => {
                write!(f, "decode {path}: {source} (body: {})", truncate(body, 200))
            }
            ClientError::MissingAccount => write!(
                f,
                "account_number required: call get_account() or with_account_number()"
            ),
            ClientError::NotFound(what) => write!(f, "not found: {what}"),
            ClientError::BadBaseUrl(u) => write!(f, "bad base url: {u}"),
            ClientError::Clock(e) => write!(f, "system clock error: {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<SignError> for ClientError {
    fn from(e: SignError) -> Self {
        ClientError::Sign(e)
    }
}

impl From<reqwest::Error> for ClientError {
    fn from(e: reqwest::Error) -> Self {
        ClientError::Http(e)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut end = n;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Minimal query-string encoder for the small set of characters we send.
/// The signed `path` must match the wire path byte-for-byte, so we build it
/// ourselves instead of letting `reqwest` re-encode it.
pub fn query_string(params: &[(&str, &str)]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let mut out = String::from("?");
    for (i, (k, v)) in params.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&percent_encode(k));
        out.push('=');
        out.push_str(&percent_encode(v));
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b',' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Filters for `GET /orders/`.
#[derive(Debug, Clone, Default)]
pub struct OrderFilter {
    pub symbol: Option<String>,
    pub side: Option<OrderSide>,
    pub state: Option<OrderState>,
}

/// Thin, signed HTTP client for the venue.
#[derive(Clone)]
pub struct RobinhoodClient {
    http: reqwest::Client,
    base_url: String,
    signer: RequestSigner,
    account_number: Option<String>,
}

impl fmt::Debug for RobinhoodClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RobinhoodClient")
            .field("base_url", &self.base_url)
            .field("signer", &self.signer)
            .field(
                "account_number",
                &self.account_number.as_deref().map(sign::redact),
            )
            .finish()
    }
}

impl RobinhoodClient {
    /// `priv_key_b64` is the base64 Ed25519 seed (`ROBINHOOD_PRIVATE_KEY`).
    pub fn new(api_key: &str, priv_key_b64: &str, base_url: &str) -> Result<Self, ClientError> {
        let base_url = base_url.trim_end_matches('/').to_string();
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err(ClientError::BadBaseUrl(base_url));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("limen-capital-live/0.1 (RM-2436)")
            .build()?;
        Ok(Self {
            http,
            base_url,
            signer: RequestSigner::new(api_key, priv_key_b64)?,
            account_number: None,
        })
    }

    pub fn with_account_number(mut self, account_number: impl Into<String>) -> Self {
        self.account_number = Some(account_number.into());
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn account_number(&self) -> Option<&str> {
        self.account_number.as_deref()
    }

    /// Base64 public key derived from the configured private key (for the
    /// operator to compare against the key registered at the venue).
    pub fn public_key_b64(&self) -> String {
        self.signer.public_key_b64()
    }

    fn require_account(&self) -> Result<&str, ClientError> {
        self.account_number
            .as_deref()
            .ok_or(ClientError::MissingAccount)
    }

    fn now_secs() -> Result<u64, ClientError> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .map_err(|e| ClientError::Clock(e.to_string()))
    }

    /// Signed request. `path` includes the query string. GETs retry on 429/5xx;
    /// anything with a body is sent exactly once.
    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
    ) -> Result<T, ClientError> {
        let retries = if method == Method::GET {
            MAX_GET_RETRIES
        } else {
            0
        };
        let mut attempt = 0u32;
        loop {
            let ts = Self::now_secs()?;
            let body_str = body.as_deref().unwrap_or("");
            let headers = self.signer.headers(ts, path, method.as_str(), body_str);
            let url = format!("{}{}", self.base_url, path);
            let mut req = self.http.request(method.clone(), &url);
            for (k, v) in headers.pairs() {
                req = req.header(k, v);
            }
            if let Some(b) = &body {
                req = req
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(b.clone());
            }
            debug!(%method, path, attempt, "robinhood request");
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    // Transport errors: retry GETs only.
                    if attempt < retries && (e.is_timeout() || e.is_connect()) {
                        attempt += 1;
                        self.backoff(attempt).await;
                        continue;
                    }
                    return Err(ClientError::Http(e));
                }
            };
            let status = resp.status();
            let text = resp.text().await?;
            if status.is_success() {
                return serde_json::from_str::<T>(&text).map_err(|source| ClientError::Decode {
                    path: path.to_string(),
                    source,
                    body: text.clone(),
                });
            }
            let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
            if retryable && attempt < retries {
                attempt += 1;
                warn!(%status, path, attempt, "robinhood retryable error; backing off");
                self.backoff(attempt).await;
                continue;
            }
            let parsed = serde_json::from_str::<ApiErrorBody>(&text).ok();
            return Err(ClientError::Api {
                status: status.as_u16(),
                method: method.to_string(),
                path: path.to_string(),
                parsed,
                body: text,
            });
        }
    }

    /// Exponential backoff with full jitter: `[0, base × 2^attempt)` ms, capped.
    async fn backoff(&self, attempt: u32) {
        let cap_ms = BACKOFF_BASE_MS.saturating_mul(1u64 << attempt.min(6));
        let jitter_ms = rand::thread_rng().gen_range(0..=cap_ms);
        tokio::time::sleep(Duration::from_millis(jitter_ms)).await;
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        self.request(Method::GET, path, None).await
    }

    // ----------------------------------------------------------------------
    // Read-only endpoints
    // ----------------------------------------------------------------------

    /// Crypto trading accounts for the key (first page; a key normally has one).
    pub async fn get_accounts(&self) -> Result<Vec<Account>, ClientError> {
        let page: Paginated<Account> = self.get(PATH_ACCOUNTS).await?;
        Ok(page.results)
    }

    /// First account; also remembers its `account_number` for later calls.
    pub async fn get_account(&mut self) -> Result<Account, ClientError> {
        let mut accounts = self.get_accounts().await?;
        if accounts.is_empty() {
            return Err(ClientError::NotFound("crypto trading account".into()));
        }
        let account = accounts.remove(0);
        if self.account_number.is_none() {
            self.account_number = Some(account.account_number.clone());
        }
        Ok(account)
    }

    /// Best bid / ask for one symbol (e.g. `BTC-USD`).
    pub async fn best_bid_ask(&self, symbol: &str) -> Result<BestBidAsk, ClientError> {
        let path = format!("{PATH_BEST_BID_ASK}{}", query_string(&[("symbol", symbol)]));
        let r: Results<BestBidAsk> = self.get(&path).await?;
        r.results
            .into_iter()
            .find(|q| q.symbol.eq_ignore_ascii_case(symbol))
            .ok_or_else(|| ClientError::NotFound(format!("best_bid_ask for {symbol}")))
    }

    /// Estimated execution price for `quantity` on `side` (`bid` | `ask` | `both`).
    pub async fn estimated_price(
        &self,
        symbol: &str,
        side: &str,
        quantity: f64,
    ) -> Result<Vec<EstimatedPrice>, ClientError> {
        let qty = format!("{quantity}");
        let path = format!(
            "{PATH_ESTIMATED_PRICE}{}",
            query_string(&[("symbol", symbol), ("side", side), ("quantity", &qty)])
        );
        let r: Results<EstimatedPrice> = self.get(&path).await?;
        Ok(r.results)
    }

    /// Trading-pair rules for one symbol (increments, min notional, tradability).
    pub async fn trading_pair(&self, symbol: &str) -> Result<TradingPair, ClientError> {
        let path = format!(
            "{PATH_TRADING_PAIRS}{}",
            query_string(&[("symbol", symbol)])
        );
        let page: Paginated<TradingPair> = self.get(&path).await?;
        page.results
            .into_iter()
            .find(|p| p.symbol.eq_ignore_ascii_case(symbol))
            .ok_or_else(|| ClientError::NotFound(format!("trading pair {symbol}")))
    }

    /// Holdings in the configured account, optionally filtered by asset code.
    pub async fn holdings(&self, asset_code: Option<&str>) -> Result<Vec<Holding>, ClientError> {
        let acct = self.require_account()?;
        let mut params: Vec<(&str, &str)> = vec![("account_number", acct)];
        if let Some(a) = asset_code {
            params.push(("asset_code", a));
        }
        let path = format!("{PATH_HOLDINGS}{}", query_string(&params));
        let page: Paginated<Holding> = self.get(&path).await?;
        Ok(page.results)
    }

    /// Holding for one asset code (`None` when nothing is held).
    pub async fn holding(&self, asset_code: &str) -> Result<Option<Holding>, ClientError> {
        let rows = self.holdings(Some(asset_code)).await?;
        Ok(rows
            .into_iter()
            .find(|h| h.asset_code.eq_ignore_ascii_case(asset_code)))
    }

    /// Orders in the configured account matching `filter` (first page).
    pub async fn get_orders(&self, filter: &OrderFilter) -> Result<Vec<Order>, ClientError> {
        let acct = self.require_account()?;
        let mut params: Vec<(&str, &str)> = vec![("account_number", acct)];
        if let Some(s) = &filter.symbol {
            params.push(("symbol", s));
        }
        if let Some(side) = filter.side {
            params.push(("side", side.as_str()));
        }
        if let Some(state) = filter.state {
            params.push(("state", state.as_str()));
        }
        let path = format!("{PATH_ORDERS}{}", query_string(&params));
        let page: Paginated<Order> = self.get(&path).await?;
        Ok(page.results)
    }

    /// One order by venue id.
    pub async fn get_order(&self, order_id: &str) -> Result<Order, ClientError> {
        let path = format!("{PATH_ORDER_DETAIL_V1}{}/", percent_encode(order_id));
        self.get(&path).await
    }
}

#[cfg(test)]
mod tests {
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
}
