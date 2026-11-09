//! Serde models for the Robinhood Crypto Trading API (V2 endpoints).
//!
//! Field shapes follow the response samples at
//! <https://docs.robinhood.com/crypto/trading/> (verified 2026-11-08). The
//! venue is inconsistent about numerics (some are JSON numbers, some are
//! decimal strings), so every numeric field accepts either via
//! [`de_f64_flex`].

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Deserialize an `f64` from a JSON number **or** a decimal string.
pub fn de_f64_flex<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(f64),
        Str(String),
    }
    match NumOrStr::deserialize(d)? {
        NumOrStr::Num(n) => Ok(n),
        NumOrStr::Str(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|e| de::Error::custom(format!("bad decimal string {s:?}: {e}"))),
    }
}

/// Like [`de_f64_flex`] but tolerates `null` / absent.
pub fn de_opt_f64_flex<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(f64),
        Str(String),
    }
    let v: Option<NumOrStr> = Option::deserialize(d)?;
    match v {
        None => Ok(None),
        Some(NumOrStr::Num(n)) => Ok(Some(n)),
        Some(NumOrStr::Str(s)) if s.trim().is_empty() => Ok(None),
        Some(NumOrStr::Str(s)) => s
            .trim()
            .parse::<f64>()
            .map(Some)
            .map_err(|e| de::Error::custom(format!("bad decimal string {s:?}: {e}"))),
    }
}

/// Cursor-paginated list envelope used by accounts / trading_pairs / holdings / orders.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Paginated<T> {
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub previous: Option<String>,
    #[serde(default = "Vec::new")]
    pub results: Vec<T>,
}

/// Plain `{ "results": [...] }` envelope (best_bid_ask, estimated_price).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Results<T> {
    #[serde(default = "Vec::new")]
    pub results: Vec<T>,
}

/// `GET /api/v2/crypto/trading/accounts/`
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Account {
    pub account_number: String,
    #[serde(default)]
    pub status: String,
    #[serde(deserialize_with = "de_f64_flex")]
    pub buying_power: f64,
    #[serde(default)]
    pub buying_power_currency: String,
    #[serde(default)]
    pub account_type: Option<String>,
    #[serde(default)]
    pub is_api_tradable: Option<bool>,
    #[serde(default)]
    pub fee_tier_status: Option<FeeTierStatus>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct FeeTierStatus {
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub fee_ratio: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub thirty_day_volume: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub next_fee_tier_ratio: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub next_fee_tier_threshold: Option<f64>,
}

/// `GET /api/v2/crypto/marketdata/best_bid_ask/?symbol=…` (one element per symbol).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct BestBidAsk {
    pub symbol: String,
    #[serde(deserialize_with = "de_f64_flex")]
    pub bid: f64,
    #[serde(deserialize_with = "de_f64_flex")]
    pub ask: f64,
}

impl BestBidAsk {
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }

    /// Spread in basis points of mid; `0.0` when mid is not positive.
    pub fn spread_bps(&self) -> f64 {
        let mid = self.mid();
        if mid > 0.0 {
            (self.ask - self.bid) / mid * 10_000.0
        } else {
            0.0
        }
    }

    /// Quote sanity: finite, positive, and not crossed.
    pub fn is_sane(&self) -> bool {
        self.bid.is_finite() && self.ask.is_finite() && self.bid > 0.0 && self.ask >= self.bid
    }
}

/// `GET /api/v2/crypto/marketdata/estimated_price/?symbol=…&side=…&quantity=…`
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EstimatedPrice {
    pub symbol: String,
    pub side: String,
    #[serde(deserialize_with = "de_f64_flex")]
    pub quantity: f64,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub bid: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub ask: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub fee_ratio: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub est_fee: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub est_total_cost: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub est_total_credit: Option<f64>,
}

/// `GET /api/v2/crypto/trading/trading_pairs/?symbol=…`
///
/// Increments / limits are decimal strings in the API; parsed to `f64` here.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct TradingPair {
    pub symbol: String,
    #[serde(default)]
    pub asset_code: String,
    #[serde(default)]
    pub quote_code: String,
    /// Smallest asset quantity step (e.g. `0.00000001` BTC).
    #[serde(deserialize_with = "de_f64_flex")]
    pub asset_increment: f64,
    /// Smallest quote price step (e.g. `0.01` USD).
    #[serde(deserialize_with = "de_f64_flex")]
    pub quote_increment: f64,
    /// Max asset quantity per order.
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub max_order_size: Option<f64>,
    /// Minimum order notional in quote currency (USD).
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub min_order_amount: Option<f64>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub is_api_tradable: bool,
}

/// `GET /api/v2/crypto/trading/holdings/?account_number=…&asset_code=…`
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Holding {
    #[serde(default)]
    pub account_number: String,
    pub asset_code: String,
    #[serde(deserialize_with = "de_f64_flex")]
    pub total_quantity: f64,
    #[serde(deserialize_with = "de_f64_flex")]
    pub quantity_available_for_trading: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}

impl OrderSide {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderSide::Buy => "buy",
            OrderSide::Sell => "sell",
        }
    }
}

impl fmt::Display for OrderSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    Limit,
    Market,
    StopLimit,
    StopLoss,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OrderState {
    Open,
    Canceled,
    Filled,
    Failed,
    Pending,
    /// Forward-compatible catch-all; treated as "not terminal, not open".
    #[serde(other)]
    Unknown,
}

impl OrderState {
    /// Terminal states never change again.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderState::Canceled | OrderState::Filled | OrderState::Failed
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OrderState::Open => "open",
            OrderState::Canceled => "canceled",
            OrderState::Filled => "filled",
            OrderState::Failed => "failed",
            OrderState::Pending => "pending",
            OrderState::Unknown => "unknown",
        }
    }
}

impl fmt::Display for OrderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One (partial) fill on an order.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Execution {
    #[serde(deserialize_with = "de_f64_flex")]
    pub effective_price: f64,
    #[serde(deserialize_with = "de_f64_flex")]
    pub quantity: f64,
    #[serde(default)]
    pub timestamp: Option<String>,
}

/// `limit_order_config` as it appears on an order object.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct LimitOrderConfig {
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub quote_amount: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub asset_quantity: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub limit_price: Option<f64>,
    #[serde(default)]
    pub time_in_force: Option<String>,
}

/// Order object returned by list / place / cancel.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Order {
    pub id: String,
    #[serde(default)]
    pub account_number: String,
    pub symbol: String,
    #[serde(default)]
    pub client_order_id: Option<String>,
    pub side: OrderSide,
    #[serde(default)]
    pub executions: Vec<Execution>,
    #[serde(rename = "type")]
    pub order_type: OrderType,
    pub state: OrderState,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub average_price: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub filled_asset_quantity: Option<f64>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub limit_order_config: Option<LimitOrderConfig>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub fee_charged: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64_flex")]
    pub estimated_fee_remaining: Option<f64>,
}

impl Order {
    /// Filled quantity: `filled_asset_quantity` if present, else sum of executions.
    pub fn filled_qty(&self) -> f64 {
        self.filled_asset_quantity
            .unwrap_or_else(|| self.executions.iter().map(|e| e.quantity).sum())
    }

    /// Quantity-weighted average fill price (falls back to `average_price`).
    pub fn avg_fill_price(&self) -> Option<f64> {
        let qty: f64 = self.executions.iter().map(|e| e.quantity).sum();
        if qty > 0.0 {
            let notional: f64 = self
                .executions
                .iter()
                .map(|e| e.quantity * e.effective_price)
                .sum();
            Some(notional / qty)
        } else {
            self.average_price
        }
    }
}

/// The only order type this experiment ever sends. A dedicated marker (rather
/// than [`OrderType`]) makes a market / stop request unrepresentable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LimitOnly {
    #[default]
    Limit,
}

/// Exactly one way to size a limit order. Decimals are sent as strings,
/// matching the docs' request samples (`"asset_quantity": "0.1"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum LimitSize {
    QuoteAmount { quote_amount: String },
    AssetQuantity { asset_quantity: String },
}

/// Request body for `POST /api/v2/crypto/trading/orders/` (limit orders only).
///
/// The type system enforces the venue's rules: `type` is always `limit`
/// and exactly one of `quote_amount` / `asset_quantity` is present.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlaceLimitOrderRequest {
    pub client_order_id: String,
    pub side: OrderSide,
    #[serde(rename = "type")]
    pub order_type: LimitOnly,
    pub symbol: String,
    pub limit_order_config: LimitOrderRequestConfig,
}

impl PlaceLimitOrderRequest {
    /// GTC limit order sized by asset quantity (what `risk::check` approves).
    pub fn new(
        client_order_id: impl Into<String>,
        side: OrderSide,
        symbol: impl Into<String>,
        asset_quantity: impl Into<String>,
        limit_price: impl Into<String>,
    ) -> Self {
        Self {
            client_order_id: client_order_id.into(),
            side,
            order_type: LimitOnly::Limit,
            symbol: symbol.into(),
            limit_order_config: LimitOrderRequestConfig {
                size: LimitSize::AssetQuantity {
                    asset_quantity: asset_quantity.into(),
                },
                limit_price: limit_price.into(),
                time_in_force: "gtc".to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LimitOrderRequestConfig {
    #[serde(flatten)]
    pub size: LimitSize,
    pub limit_price: String,
    pub time_in_force: String,
}

/// Error envelope: `{ "type": "validation_error", "errors": [{ "attr", "detail" }] }`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ApiErrorBody {
    #[serde(rename = "type", default)]
    pub error_type: String,
    #[serde(default)]
    pub errors: Vec<ApiErrorDetail>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ApiErrorDetail {
    #[serde(default)]
    pub attr: Option<String>,
    #[serde(default)]
    pub detail: String,
}

impl fmt::Display for ApiErrorBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error_type)?;
        for e in &self.errors {
            match &e.attr {
                Some(a) => write!(f, "; {a}: {}", e.detail)?,
                None => write!(f, "; {}", e.detail)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_sample_parses_with_string_buying_power() {
        let json = r#"{"next":null,"previous":null,"results":[{"account_number":"ACC123","status":"active","buying_power":"10.25","buying_power_currency":"USD","account_type":"crypto","is_api_tradable":true,"fee_tier_status":{"fee_ratio":0.0045,"thirty_day_volume":"0","next_fee_tier_ratio":0.0035,"next_fee_tier_threshold":10000}}]}"#;
        let p: Paginated<Account> = serde_json::from_str(json).unwrap();
        let a = &p.results[0];
        assert_eq!(a.account_number, "ACC123");
        assert_eq!(a.buying_power, 10.25);
        assert_eq!(a.fee_tier_status.as_ref().unwrap().fee_ratio, Some(0.0045));
        assert_eq!(
            a.fee_tier_status.as_ref().unwrap().thirty_day_volume,
            Some(0.0)
        );
    }

    #[test]
    fn best_bid_ask_numbers_or_strings() {
        let a: Results<BestBidAsk> = serde_json::from_str(
            r#"{"results":[{"symbol":"BTC-USD","bid":99990.5,"ask":100009.5}]}"#,
        )
        .unwrap();
        let b: Results<BestBidAsk> = serde_json::from_str(
            r#"{"results":[{"symbol":"BTC-USD","bid":"99990.5","ask":"100009.5"}]}"#,
        )
        .unwrap();
        assert_eq!(a.results, b.results);
        let q = &a.results[0];
        assert_eq!(q.mid(), 100_000.0);
        assert!((q.spread_bps() - 1.9).abs() < 1e-9);
        assert!(q.is_sane());
        let crossed = BestBidAsk {
            symbol: "X".into(),
            bid: 2.0,
            ask: 1.0,
        };
        assert!(!crossed.is_sane());
    }

    #[test]
    fn trading_pair_sample_parses() {
        let json = r#"{"next":null,"previous":null,"results":[{"symbol":"BTC-USD","asset_code":"BTC","quote_code":"USD","asset_increment":"0.000000010000000000","quote_increment":"0.010000000000000000","max_order_size":"100.0","min_order_amount":"1.0","status":"tradable","is_api_tradable":true}]}"#;
        let p: Paginated<TradingPair> = serde_json::from_str(json).unwrap();
        let t = &p.results[0];
        assert_eq!(t.asset_increment, 1e-8);
        assert_eq!(t.quote_increment, 0.01);
        assert_eq!(t.min_order_amount, Some(1.0));
        assert!(t.is_api_tradable);
    }

    #[test]
    fn holdings_sample_parses() {
        let json = r#"{"next":null,"previous":null,"results":[{"account_number":"ACC","asset_code":"BTC","total_quantity":"0.00010000","quantity_available_for_trading":"0.00010000"}]}"#;
        let p: Paginated<Holding> = serde_json::from_str(json).unwrap();
        assert_eq!(p.results[0].total_quantity, 1e-4);
    }

    #[test]
    fn order_sample_parses_and_computes_fills() {
        let json = r#"{"id":"497f6eca-6276-4993-bfeb-53cbbbba6f08","account_number":"ACC","symbol":"BTC-USD","client_order_id":"11299b2b-61e3-43e7-b9f7-dee77210bb29","side":"buy","executions":[{"effective_price":"100000.00","quantity":"0.00002","timestamp":"2019-08-24T14:15:22Z"},{"effective_price":"100010.00","quantity":"0.00002","timestamp":"2019-08-24T14:15:23Z"}],"type":"limit","state":"filled","average_price":100005.0,"filled_asset_quantity":0.00004,"created_at":"x","updated_at":"y","limit_order_config":{"quote_amount":null,"asset_quantity":"0.00004","limit_price":"100010.00","time_in_force":"gtc"},"fee_charged":"0.01","estimated_fee_remaining":0}"#;
        let o: Order = serde_json::from_str(json).unwrap();
        assert_eq!(o.side, OrderSide::Buy);
        assert_eq!(o.order_type, OrderType::Limit);
        assert_eq!(o.state, OrderState::Filled);
        assert!(o.state.is_terminal());
        assert!((o.filled_qty() - 0.00004).abs() < 1e-12);
        assert!((o.avg_fill_price().unwrap() - 100_005.0).abs() < 1e-6);
        assert_eq!(
            o.limit_order_config.as_ref().unwrap().limit_price,
            Some(100_010.0)
        );
        assert_eq!(o.fee_charged, Some(0.01));
    }

    #[test]
    fn unknown_order_state_is_tolerated() {
        let json =
            r#"{"id":"1","symbol":"BTC-USD","side":"sell","type":"limit","state":"something_new"}"#;
        let o: Order = serde_json::from_str(json).unwrap();
        assert_eq!(o.state, OrderState::Unknown);
        assert!(!o.state.is_terminal());
        assert_eq!(o.filled_qty(), 0.0);
        assert_eq!(o.avg_fill_price(), None);
    }

    #[test]
    fn place_limit_order_request_serializes_only_one_size_field() {
        let req = PlaceLimitOrderRequest::new(
            "131de903-5a9c-4260-abc1-28d562a5dcf0",
            OrderSide::Buy,
            "BTC-USD",
            "0.00001000",
            "99990.50",
        );
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"client_order_id":"131de903-5a9c-4260-abc1-28d562a5dcf0","side":"buy","type":"limit","symbol":"BTC-USD","limit_order_config":{"asset_quantity":"0.00001000","limit_price":"99990.50","time_in_force":"gtc"}}"#
        );
        assert!(!s.contains("quote_amount"));
        assert!(!s.contains("market"));
        // Round-trips, and a quote-sized variant serializes its one field.
        let back: PlaceLimitOrderRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, req);
        let quote = LimitOrderRequestConfig {
            size: LimitSize::QuoteAmount {
                quote_amount: "5.00".into(),
            },
            limit_price: "1".into(),
            time_in_force: "gtc".into(),
        };
        assert_eq!(
            serde_json::to_string(&quote).unwrap(),
            r#"{"quote_amount":"5.00","limit_price":"1","time_in_force":"gtc"}"#
        );
        // `type` can only ever be "limit".
        assert!(serde_json::from_str::<LimitOnly>(r#""market""#).is_err());
    }

    #[test]
    fn api_error_body_displays() {
        let e: ApiErrorBody = serde_json::from_str(
            r#"{"type":"validation_error","errors":[{"detail":"Must be a valid UUID.","attr":"client_order_id"}]}"#,
        )
        .unwrap();
        assert_eq!(
            e.to_string(),
            "validation_error; client_order_id: Must be a valid UUID."
        );
    }
}
