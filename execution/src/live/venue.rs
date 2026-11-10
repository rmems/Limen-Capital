//! Venue book contract for live mode (Alpaca-shaped).
//!
//! The venue is the source of truth for cash, holdings and order state; the
//! local durable state (see [`crate::live::durable`]) only remembers what this
//! process asked for and which fills it has already accounted for.
//!
//! Field names follow the Alpaca Trading API objects (`GET /v2/account`,
//! `GET /v2/positions/{symbol}`, `GET /v2/orders`), which send decimals as
//! strings. The HTTP client implements [`Venue`]; [`mock::MockVenue`] is an
//! in-process stand-in with the same semantics for tests.

pub mod mock;

use crate::live::risk::ApprovedOrder;
use crate::live::robinhood::types::OrderSide;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::future::Future;
use std::pin::Pin;

/// Decimal that arrives as a JSON number or a decimal string.
fn de_decimal<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    de_opt_decimal(d).map(|v| v.unwrap_or(0.0))
}

fn de_opt_decimal<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(f64),
        Str(String),
    }
    match Option::<NumOrStr>::deserialize(d)? {
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

/// `GET /v2/account` (the fields the book needs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueAccount {
    /// Alpaca account id; pins the durable state to one account.
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(deserialize_with = "de_decimal")]
    pub cash: f64,
    #[serde(deserialize_with = "de_decimal")]
    pub buying_power: f64,
}

/// `GET /v2/positions/{symbol}`. Alpaca answers 404 when nothing is held.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenuePosition {
    pub symbol: String,
    #[serde(deserialize_with = "de_decimal")]
    pub qty: f64,
}

/// Alpaca order status. Anything outside the known set is not terminal and
/// is never read as a fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VenueOrderStatus {
    New,
    PartiallyFilled,
    Filled,
    DoneForDay,
    Canceled,
    Expired,
    Replaced,
    PendingCancel,
    PendingReplace,
    PendingNew,
    Accepted,
    AcceptedForBidding,
    Stopped,
    Rejected,
    Suspended,
    Calculated,
    Held,
    #[serde(other)]
    Unknown,
}

impl VenueOrderStatus {
    /// The order will never change again.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Filled | Self::Canceled | Self::Expired | Self::Rejected | Self::Replaced
        )
    }
}

/// One order as the venue reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueOrder {
    pub id: String,
    #[serde(default)]
    pub client_order_id: Option<String>,
    pub symbol: String,
    pub side: OrderSide,
    #[serde(default, deserialize_with = "de_decimal")]
    pub qty: f64,
    #[serde(default, deserialize_with = "de_opt_decimal")]
    pub limit_price: Option<f64>,
    pub status: VenueOrderStatus,
    /// Cumulative filled quantity across partial fills.
    #[serde(default, deserialize_with = "de_decimal")]
    pub filled_qty: f64,
    #[serde(default, deserialize_with = "de_opt_decimal")]
    pub filled_avg_price: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VenueError {
    /// The request may or may not have reached the venue (timeout, reset,
    /// lost response). The outcome must be looked up, never assumed.
    Transport(String),
    /// The venue answered with a definitive refusal.
    Rejected { status: u16, body: String },
    /// Anything else (bad body, missing account, ...).
    Other(String),
}

impl fmt::Display for VenueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VenueError::Transport(e) => write!(f, "venue transport (outcome unknown): {e}"),
            VenueError::Rejected { status, body } => write!(f, "venue rejected ({status}): {body}"),
            VenueError::Other(e) => write!(f, "venue: {e}"),
        }
    }
}

impl std::error::Error for VenueError {}

pub type VenueFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, VenueError>> + Send + 'a>>;

/// What the durable book needs from the venue. Writes are sent exactly once;
/// implementations never retry `place_limit`.
pub trait Venue: Send + Sync {
    fn label(&self) -> String;
    /// `GET /v2/account`.
    fn account(&self) -> VenueFuture<'_, VenueAccount>;
    /// `GET /v2/positions/{symbol}`; `None` when nothing is held.
    fn position<'a>(&'a self, symbol: &'a str) -> VenueFuture<'a, Option<VenuePosition>>;
    /// `GET /v2/orders?status=open&symbols={symbol}`.
    fn open_orders<'a>(&'a self, symbol: &'a str) -> VenueFuture<'a, Vec<VenueOrder>>;
    /// `GET /v2/orders:by_client_order_id?client_order_id=…`; `None` on 404.
    fn order_by_client_id<'a>(
        &'a self,
        client_order_id: &'a str,
    ) -> VenueFuture<'a, Option<VenueOrder>>;
    /// `POST /v2/orders` (limit, sized by `qty`, caller-supplied `client_order_id`).
    fn place_limit<'a>(&'a self, order: &'a ApprovedOrder) -> VenueFuture<'a, VenueOrder>;
    /// `DELETE /v2/orders/{order_id}`.
    fn cancel<'a>(&'a self, order_id: &'a str) -> VenueFuture<'a, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpaca_shaped_objects_parse_with_string_decimals() {
        let a: VenueAccount = serde_json::from_str(
            r#"{"id":"904837e3","status":"ACTIVE","cash":"10.25","buying_power":"20.5","currency":"USD"}"#,
        )
        .unwrap();
        assert_eq!(a.cash, 10.25);
        assert_eq!(a.buying_power, 20.5);
        let p: VenuePosition =
            serde_json::from_str(r#"{"symbol":"SPY","qty":"0.0125","side":"long"}"#).unwrap();
        assert_eq!(p.qty, 0.0125);
        let o: VenueOrder = serde_json::from_str(
            r#"{"id":"61e69015","client_order_id":"eb9e2aaa","symbol":"SPY","side":"buy","type":"limit","qty":"0.02","limit_price":"500.10","status":"partially_filled","filled_qty":"0.01","filled_avg_price":"500.05"}"#,
        )
        .unwrap();
        assert_eq!(o.status, VenueOrderStatus::PartiallyFilled);
        assert!(!o.status.is_terminal());
        assert_eq!(o.filled_qty, 0.01);
        assert_eq!(o.filled_avg_price, Some(500.05));
        assert_eq!(o.limit_price, Some(500.10));
    }

    #[test]
    fn unknown_status_is_neither_terminal_nor_a_fill() {
        let o: VenueOrder = serde_json::from_str(
            r#"{"id":"1","symbol":"SPY","side":"sell","status":"something_new","filled_qty":null,"filled_avg_price":null}"#,
        )
        .unwrap();
        assert_eq!(o.status, VenueOrderStatus::Unknown);
        assert!(!o.status.is_terminal());
        assert_eq!(o.filled_qty, 0.0);
        for s in ["filled", "canceled", "expired", "rejected", "replaced"] {
            let st: VenueOrderStatus = serde_json::from_str(&format!("{s:?}")).unwrap();
            assert!(st.is_terminal(), "{s}");
        }
        for s in [
            "new",
            "accepted",
            "pending_new",
            "done_for_day",
            "pending_cancel",
        ] {
            let st: VenueOrderStatus = serde_json::from_str(&format!("{s:?}")).unwrap();
            assert!(!st.is_terminal(), "{s}");
        }
    }
}
