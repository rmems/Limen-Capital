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
            } => fmt_api_error(f, *status, method, path, parsed, body),
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

fn should_retry_transport(error: &reqwest::Error, attempt: u32, retries: u32) -> bool {
    attempt < retries && (error.is_timeout() || error.is_connect())
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn decode_response<T: DeserializeOwned>(
    method: &Method,
    path: &str,
    status: StatusCode,
    text: String,
) -> Result<T, ClientError> {
    if status.is_success() {
        return serde_json::from_str(&text).map_err(|source| ClientError::Decode {
            path: path.to_string(),
            source,
            body: text.clone(),
        });
    }
    Err(ClientError::Api {
        status: status.as_u16(),
        method: method.to_string(),
        path: path.to_string(),
        parsed: serde_json::from_str::<ApiErrorBody>(&text).ok(),
        body: text,
    })
}

fn fmt_api_error(
    f: &mut fmt::Formatter<'_>,
    status: u16,
    method: &str,
    path: &str,
    parsed: &Option<ApiErrorBody>,
    body: &str,
) -> fmt::Result {
    match parsed {
        Some(parsed) => write!(f, "{method} {path} → {status}: {parsed}"),
        None => write!(f, "{method} {path} → {status}: {}", truncate(body, 200)),
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
            debug!(%method, path, attempt, "robinhood request");
            let resp = match self
                .signed_request(&method, path, body.as_deref())?
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    if should_retry_transport(&e, attempt, retries) {
                        attempt += 1;
                        self.backoff(attempt).await;
                        continue;
                    }
                    return Err(ClientError::Http(e));
                }
            };
            let status = resp.status();
            let text = resp.text().await?;
            if retryable_status(status) && attempt < retries {
                attempt += 1;
                warn!(%status, path, attempt, "robinhood retryable error; backing off");
                self.backoff(attempt).await;
                continue;
            }
            return decode_response(&method, path, status, text);
        }
    }

    /// Build and sign each attempt afresh, including its timestamp.
    fn signed_request(
        &self,
        method: &Method,
        path: &str,
        body: Option<&str>,
    ) -> Result<reqwest::RequestBuilder, ClientError> {
        let headers =
            self.signer
                .headers(Self::now_secs()?, path, method.as_str(), body.unwrap_or(""));
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.http.request(method.clone(), &url);
        for (k, v) in headers.pairs() {
            req = req.header(k, v);
        }
        if let Some(body) = body {
            req = req
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }
        Ok(req)
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
mod tests;
