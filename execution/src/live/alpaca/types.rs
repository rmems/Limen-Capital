//! Alpaca Trading API models. Decimal wire values remain strings on requests.
use serde::{Deserialize, Serialize};
use std::fmt;

pub fn de_f64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Decimal {
        Number(f64),
        Text(String),
    }
    let n = match Decimal::deserialize(d)? {
        Decimal::Number(n) => n,
        Decimal::Text(s) => s.parse().map_err(serde::de::Error::custom)?,
    };
    if n.is_finite() && n >= 0.0 {
        Ok(n)
    } else {
        Err(serde::de::Error::custom(
            "expected a finite nonnegative decimal",
        ))
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct BestBidAsk {
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
}
impl BestBidAsk {
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }
    pub fn spread_bps(&self) -> f64 {
        if self.mid() > 0.0 {
            (self.ask - self.bid) / self.mid() * 10_000.0
        } else {
            0.0
        }
    }
    pub fn is_sane(&self) -> bool {
        self.bid.is_finite() && self.ask.is_finite() && self.bid > 0.0 && self.ask >= self.bid
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}
impl fmt::Display for OrderSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        })
    }
}

/// Unknown statuses must never be treated as a fill or terminal.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    #[serde(rename = "new")]
    Open,
    Accepted,
    PendingNew,
    AcceptedForBidding,
    PartiallyFilled,
    PendingCancel,
    PendingReplace,
    DoneForDay,
    Stopped,
    Suspended,
    Calculated,
    Replaced,
    Filled,
    Canceled,
    Expired,
    Rejected,
    #[serde(other)]
    Unknown,
}
impl OrderState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Filled | Self::Canceled | Self::Expired | Self::Rejected
        )
    }
}

impl fmt::Display for OrderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Open => "new",
            Self::Accepted => "accepted",
            Self::PendingNew => "pending_new",
            Self::AcceptedForBidding => "accepted_for_bidding",
            Self::PartiallyFilled => "partially_filled",
            Self::PendingCancel => "pending_cancel",
            Self::PendingReplace => "pending_replace",
            Self::DoneForDay => "done_for_day",
            Self::Stopped => "stopped",
            Self::Suspended => "suspended",
            Self::Calculated => "calculated",
            Self::Replaced => "replaced",
            Self::Filled => "filled",
            Self::Canceled => "canceled",
            Self::Expired => "expired",
            Self::Rejected => "rejected",
            Self::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Order {
    pub id: String,
    pub client_order_id: String,
    pub symbol: String,
    pub side: OrderSide,
    pub status: OrderState,
    /// Venue cumulative total across all partial fills, not a delta.
    #[serde(deserialize_with = "de_f64")]
    pub filled_qty: f64,
    pub filled_avg_price: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LimitOnly {
    Limit,
}
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TimeInForce {
    #[default]
    Day,
}

/// Exactly one sizing field is representable, including fractional quantities.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum LimitSize {
    Qty { qty: String },
    Notional { notional: String },
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlaceLimitOrderRequest {
    pub client_order_id: String,
    pub symbol: String,
    pub side: OrderSide,
    #[serde(rename = "type")]
    pub order_type: LimitOnly,
    #[serde(flatten)]
    pub size: LimitSize,
    pub limit_price: String,
    pub time_in_force: TimeInForce,
    pub extended_hours: bool,
}
impl PlaceLimitOrderRequest {
    pub fn new(
        client_order_id: impl Into<String>,
        side: OrderSide,
        symbol: impl Into<String>,
        qty: impl Into<String>,
        limit_price: impl Into<String>,
    ) -> Self {
        Self {
            client_order_id: client_order_id.into(),
            symbol: symbol.into(),
            side,
            order_type: LimitOnly::Limit,
            size: LimitSize::Qty { qty: qty.into() },
            limit_price: limit_price.into(),
            time_in_force: TimeInForce::Day,
            extended_hours: false,
        }
    }
}
impl From<&crate::live::risk::ApprovedOrder> for PlaceLimitOrderRequest {
    fn from(o: &crate::live::risk::ApprovedOrder) -> Self {
        Self::new(
            &o.client_order_id,
            o.side,
            &o.symbol,
            &o.asset_qty_str,
            &o.limit_price_str,
        )
    }
}

#[derive(Debug, Deserialize)]
pub struct Account {
    pub status: String,
    pub buying_power: String,
}
#[derive(Debug, Deserialize)]
pub struct Asset {
    pub symbol: String,
    pub tradable: bool,
    pub fractionable: bool,
    pub status: String,
}
