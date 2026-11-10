//! Alpaca equity orders. Writes are sent once, never retried.
//!
//! Production endpoints are fixed; only unit tests can redirect to a mock.
//! Regular-session day orders only. Expiry at session close is a normal outcome.
pub mod types;

use reqwest::{
    header::{HeaderMap, HeaderValue},
    Method,
};
use serde::de::DeserializeOwned;
use std::{fmt, time::Duration};
use types::*;

pub const DEFAULT_BASE_URL: &str = "https://api.alpaca.markets";
const DATA_BASE_URL: &str = "https://data.alpaca.markets";

#[derive(Debug)]
pub enum ClientError {
    Http(reqwest::Error),
    Api {
        status: u16,
    },
    Decode(serde_json::Error),
    InvalidRequest(&'static str),
    NotFound(String),
    /// The POST may have committed. Reconcile this ID later; never resubmit it.
    SubmissionUnknown {
        client_order_id: String,
    },
}
impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(_) => f.write_str("Alpaca transport error"),
            Self::Api { status } => write!(f, "Alpaca HTTP {status}"),
            Self::Decode(_) => f.write_str("invalid Alpaca response"),
            Self::InvalidRequest(m) => write!(f, "invalid Alpaca request: {m}"),
            Self::NotFound(m) => write!(f, "not found: {m}"),
            Self::SubmissionUnknown { client_order_id } =>
                write!(f, "Alpaca submission unresolved; reconcile client_order_id={client_order_id} before any further action"),
        }
    }
}
impl std::error::Error for ClientError {}
impl From<reqwest::Error> for ClientError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e.without_url())
    }
}

#[derive(Clone)]
pub struct AlpacaClient {
    http: reqwest::Client,
    base_url: String,
    data_url: String,
}
impl fmt::Debug for AlpacaClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AlpacaClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}
impl AlpacaClient {
    /// Pass the environment-only pair from `Secrets::alpaca()`.
    pub fn new(api_key: &str, secret_key: &str) -> Result<Self, ClientError> {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("apca-api-key-id", api_key),
            ("apca-api-secret-key", secret_key),
        ] {
            if value.trim().is_empty() {
                return Err(ClientError::InvalidRequest("empty API credential"));
            }
            let mut v = HeaderValue::from_str(value)
                .map_err(|_| ClientError::InvalidRequest("invalid API credential header"))?;
            v.set_sensitive(true);
            headers.insert(name, v);
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .default_headers(headers)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()?,
            base_url: DEFAULT_BASE_URL.into(),
            data_url: DATA_BASE_URL.into(),
        })
    }
    #[cfg(test)]
    pub(crate) fn with_mock_url(mut self, url: &str) -> Self {
        self.base_url = url.into();
        self.data_url = url.into();
        self
    }
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, ClientError> {
        if !response.status().is_success() {
            return Err(ClientError::Api {
                status: response.status().as_u16(),
            });
        }
        let bytes = response.bytes().await?;
        serde_json::from_slice(&bytes).map_err(ClientError::Decode)
    }
    fn url(&self, path: &str) -> reqwest::Url {
        // Both bases are fixed constants (or loopback mocks under cfg(test)).
        reqwest::Url::parse(&format!("{}{path}", self.base_url)).expect("fixed Alpaca URL")
    }
    fn resource_url(&self, resource: &str, id: &str) -> reqwest::Url {
        let mut url = self.url(resource);
        url.path_segments_mut()
            .expect("fixed hierarchical URL")
            .push(id);
        url
    }
    async fn get<T: DeserializeOwned>(&self, url: reqwest::Url) -> Result<T, ClientError> {
        Self::decode(self.http.get(url).send().await?).await
    }

    /// One POST. A lost/undecodable response or server error triggers a read by
    /// client ID, never a second POST. A failed lookup leaves an ambiguous result.
    pub async fn place_limit_order(
        &self,
        order: &PlaceLimitOrderRequest,
    ) -> Result<Order, ClientError> {
        validate_order(order)?;
        let response = match self
            .http
            .post(self.url("/v2/orders"))
            .json(order)
            .send()
            .await
        {
            Ok(r) => Self::decode(r).await,
            Err(e) => Err(e.into()),
        };
        match response {
            Ok(order) => Ok(order),
            Err(ClientError::Api { status }) if (400..500).contains(&status) && status != 408 => {
                Err(ClientError::Api { status })
            }
            Err(_) => self
                .get_order_by_client_order_id(&order.client_order_id)
                .await
                .map_err(|_| ClientError::SubmissionUnknown {
                    client_order_id: order.client_order_id.clone(),
                }),
        }
    }

    /// One DELETE. A 204 has no JSON body; cancellation is confirmed by polling.
    pub async fn cancel_order(&self, id: &str) -> Result<(), ClientError> {
        let response = self
            .http
            .request(Method::DELETE, self.resource_url("/v2/orders", id))
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(ClientError::Api {
                status: response.status().as_u16(),
            })
        }
    }
    pub async fn get_order(&self, id: &str) -> Result<Order, ClientError> {
        self.get(self.resource_url("/v2/orders", id)).await
    }
    pub async fn get_order_by_client_order_id(&self, id: &str) -> Result<Order, ClientError> {
        let mut url = self.url("/v2/orders:by_client_order_id");
        url.query_pairs_mut().append_pair("client_order_id", id);
        self.get(url).await
    }
    /// Latest 500 orders, including closed ones (Alpaca defaults to open only).
    /// Use `get_order` for authoritative tracking of a known order.
    pub async fn get_orders(&self) -> Result<Vec<Order>, ClientError> {
        let mut url = self.url("/v2/orders");
        url.query_pairs_mut()
            .append_pair("status", "all")
            .append_pair("limit", "500");
        self.get(url).await
    }
    /// Bounded polling, returning the latest snapshot even if it remains open.
    /// `filled_qty` is cumulative: repeated snapshots must not be added together.
    /// Unknown statuses continue polling. Expiry is terminal, not an error.
    pub async fn poll_order(
        &self,
        id: &str,
        max_polls: usize,
        interval: Duration,
    ) -> Result<Order, ClientError> {
        if max_polls == 0 {
            return Err(ClientError::InvalidRequest("max_polls must be positive"));
        }
        for i in 0..max_polls {
            let order = self.get_order(id).await?;
            if order.status.is_terminal() || i + 1 == max_polls {
                return Ok(order);
            }
            tokio::time::sleep(interval).await;
        }
        unreachable!("positive poll budget")
    }
    pub async fn get_account(&self) -> Result<Account, ClientError> {
        self.get(self.url("/v2/account")).await
    }
    pub async fn get_asset(&self, symbol: &str) -> Result<Asset, ClientError> {
        self.get(self.resource_url("/v2/assets", symbol)).await
    }
    pub async fn best_bid_ask(&self, symbol: &str) -> Result<BestBidAsk, ClientError> {
        #[derive(serde::Deserialize)]
        struct Quote {
            bp: f64,
            ap: f64,
        }
        #[derive(serde::Deserialize)]
        struct Latest {
            symbol: String,
            quote: Quote,
        }
        let mut url =
            reqwest::Url::parse(&format!("{}/v2/stocks", self.data_url)).expect("fixed data URL");
        url.path_segments_mut()
            .expect("hierarchical URL")
            .push(symbol)
            .push("quotes")
            .push("latest");
        url.query_pairs_mut().append_pair("feed", "iex");
        let latest: Latest = self.get(url).await?;
        Ok(BestBidAsk {
            symbol: latest.symbol,
            bid: latest.quote.bp,
            ask: latest.quote.ap,
        })
    }
}
fn validate_order(o: &PlaceLimitOrderRequest) -> Result<(), ClientError> {
    if uuid::Uuid::parse_str(&o.client_order_id).is_err() {
        return Err(ClientError::InvalidRequest(
            "client_order_id must be a UUID",
        ));
    }
    if o.symbol.trim().is_empty() || o.extended_hours {
        return Err(ClientError::InvalidRequest(
            "symbol required; extended hours unsupported",
        ));
    }
    let size = match &o.size {
        LimitSize::Qty { qty } => qty,
        LimitSize::Notional { notional } => notional,
    };
    for value in [size, &o.limit_price] {
        if !value.parse::<f64>().is_ok_and(|n| n.is_finite() && n > 0.0) {
            return Err(ClientError::InvalidRequest(
                "size and limit price must be positive finite decimals",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
