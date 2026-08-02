//! dydx Market Feed Integration
//!
//! Real-time price and order book data from dydx v4 (decentralized perpetuals exchange)
//! Uses dydx public REST API for market data ingestion.
//!
//! dydx v4: https://dydx.trade/
//! API Docs: https://dydx.exchange/api/
//!
//! Supported pairs:
//! - BTC-USD
//! - ETH-USD
//! - SOL-USD
//! - ATOM-USD
//! - etc.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// dydx market price snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DydxPrice {
    pub ticker: String,
    pub price: f64,
    pub timestamp_ms: i64,
    pub bid: f64,
    pub ask: f64,
    pub mid_price: f64,
}

impl DydxPrice {
    pub fn spread(&self) -> f64 {
        self.ask - self.bid
    }

    pub fn spread_bps(&self) -> f64 {
        (self.spread() / self.mid_price) * 10_000.0
    }
}

/// Order book level
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookLevel {
    pub price: f64,
    pub quantity: f64,
    pub notional: f64, // price * quantity
}

/// dydx order book snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DydxOrderBook {
    pub ticker: String,
    pub timestamp_ms: i64,
    pub bids: Vec<OrderBookLevel>, // [0] = best bid
    pub asks: Vec<OrderBookLevel>, // [0] = best ask
}

impl DydxOrderBook {
    pub fn mid_price(&self) -> Option<f64> {
        match (self.bids.first(), self.asks.first()) {
            (Some(bid), Some(ask)) => Some((bid.price + ask.price) / 2.0),
            _ => None,
        }
    }

    pub fn spread(&self) -> Option<f64> {
        match (self.bids.first(), self.asks.first()) {
            (Some(bid), Some(ask)) => Some(ask.price - bid.price),
            _ => None,
        }
    }
}

/// dydx API client (REST-based, not WebSocket yet)
pub struct DydxClient {
    base_url: String,
    client: reqwest::Client,
}

impl Default for DydxClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DydxClient {
    pub fn new() -> Self {
        Self {
            base_url: "https://indexer.dydx.trade/v4".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Fetch current price for a ticker
    /// Examples: BTC, ETH, SOL, ATOM
    pub async fn get_price(&self, ticker: &str) -> Result<DydxPrice, String> {
        let url = format!("{}/markets/{}", self.base_url, ticker);

        match self.client.get(&url).send().await {
            Ok(resp) => {
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        // Parse dydx response
                        if let Some(market) = body.get("market") {
                            let price = market
                                .get("oraclePrice")
                                .and_then(|p| p.as_str())
                                .and_then(|p| p.parse::<f64>().ok())
                                .ok_or("Missing oraclePrice")?;

                            let bid = market
                                .get("indexPrice")
                                .and_then(|p| p.as_str())
                                .and_then(|p| p.parse::<f64>().ok())
                                .unwrap_or(price * 0.999);

                            let ask = bid / 0.999; // Synthetic spread

                            Ok(DydxPrice {
                                ticker: ticker.to_string(),
                                price,
                                bid,
                                ask,
                                mid_price: (bid + ask) / 2.0,
                                timestamp_ms: chrono::Utc::now().timestamp_millis(),
                            })
                        } else {
                            Err("Market not found in response".to_string())
                        }
                    }
                    Err(e) => Err(format!("Failed to parse JSON: {}", e)),
                }
            }
            Err(e) => Err(format!("HTTP error: {}", e)),
        }
    }

    /// Fetch order book (top N levels)
    pub async fn get_orderbook(&self, ticker: &str, depth: usize) -> Result<DydxOrderBook, String> {
        // dydx's REST API doesn't expose full orderbook; this would need WebSocket
        // For now, return a synthetic orderbook based on price
        let price = self.get_price(ticker).await?;

        let mut bids = Vec::new();
        let mut asks = Vec::new();

        // Generate synthetic order book (in production, use WebSocket)
        for i in 1..=depth {
            let level_size = 1.0 / (i as f64);
            bids.push(OrderBookLevel {
                price: price.bid - (i as f64) * 0.001,
                quantity: level_size,
                notional: (price.bid - (i as f64) * 0.001) * level_size,
            });
            asks.push(OrderBookLevel {
                price: price.ask + (i as f64) * 0.001,
                quantity: level_size,
                notional: (price.ask + (i as f64) * 0.001) * level_size,
            });
        }

        Ok(DydxOrderBook {
            ticker: ticker.to_string(),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            bids,
            asks,
        })
    }
}

/// Market feed aggregator (maintains latest prices for multiple tickers)
pub struct MarketFeed {
    client: DydxClient,
    prices: Arc<RwLock<std::collections::HashMap<String, DydxPrice>>>,
    #[allow(dead_code)] // reserved for orderbook polling
    orderbooks: Arc<RwLock<std::collections::HashMap<String, DydxOrderBook>>>,
}

impl Default for MarketFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl MarketFeed {
    pub fn new() -> Self {
        Self {
            client: DydxClient::new(),
            prices: Arc::new(RwLock::new(std::collections::HashMap::new())),
            orderbooks: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Poll market feed (should be called in a background task)
    pub async fn poll_prices(&self, tickers: &[&str]) {
        for ticker in tickers {
            match self.client.get_price(ticker).await {
                Ok(price) => {
                    let price_val = price.price;
                    self.prices.write().await.insert(ticker.to_string(), price);
                    info!("[{}] Price: ${:.2}", ticker, price_val);
                }
                Err(e) => {
                    warn!("[{}] Failed to fetch price: {}", ticker, e);
                }
            }
        }
    }

    /// Get latest cached price
    pub async fn get_latest_price(&self, ticker: &str) -> Option<f64> {
        self.prices.read().await.get(ticker).map(|p| p.price)
    }

    /// Get bid-ask spread in basis points
    pub async fn get_spread_bps(&self, ticker: &str) -> Option<f64> {
        self.prices.read().await.get(ticker).map(|p| p.spread_bps())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live network call — not run in default/CI `cargo test` (no timeout / flaky).
    /// Run with: `cargo test test_dydx_price_fetch -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "live dYdX network call; exclude from CI"]
    async fn test_dydx_price_fetch() {
        let client = DydxClient::new();

        match client.get_price("BTC").await {
            Ok(price) => {
                println!("BTC Price: ${:.2}", price.price);
                assert!(price.price > 0.0);
                assert!(!price.ticker.is_empty());
            }
            Err(e) => panic!("dYdX fetch failed: {e}"),
        }
    }

    #[test]
    fn test_spread_calculation() {
        let price = DydxPrice {
            ticker: "BTC-USD".to_string(),
            price: 65000.0,
            bid: 64990.0,
            ask: 65010.0,
            mid_price: 65000.0,
            timestamp_ms: 0,
        };

        let spread = price.spread();
        assert_eq!(spread, 20.0);

        let spread_bps = price.spread_bps();
        assert!((spread_bps - 3.08).abs() < 0.1); // ~3.08 bps
    }
}
