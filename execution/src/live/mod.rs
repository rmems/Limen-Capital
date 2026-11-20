//! Live micro-capital experiment (RM-2436).
//!
//! A deliberately small, explicitly **non-research** live trading experiment:
//! an Alpaca equity venue adapter plus a read-only Robinhood Crypto adapter
//! and a decision loop that trades a fixed
//! micro-budget with hard limits enforced in code (`risk`). The decider is
//! advisory; the risk module owns execution and rejects before any HTTP call
//! reaches the venue.
//!
//! Everything here is behind the `live-alpaca` Cargo feature (off by
//! default) and never touches the Julia brain, wire fixtures or the paper
//! simulation contracts.
//!
//! With `decider = "claude"`, Sonnet proposes advisory decisions through one
//! strict tool call, an optional Haiku helper writes journal tags only, and a
//! persistent ledger (`decider::budget`) meters all Claude spend against a
//! cumulative cap and the credit-expiry cutoff before any paid call.
//!
//! Secrets are read from the environment only:
//! `APCA_API_KEY_ID`, `APCA_API_SECRET_KEY`, `ROBINHOOD_API_KEY`,
//! `ROBINHOOD_PRIVATE_KEY`, `ANTHROPIC_API_KEY`.

pub mod alpaca;
pub mod config;
pub mod cost;
pub mod decider;
pub mod durable;
pub mod journal;
pub mod kill;
pub mod ledger_bridge;
pub mod live_book;
pub mod r#loop;
pub mod market;
pub mod market_state;
pub mod notify;
pub mod paper;
pub mod postmortem;
pub mod risk;
pub mod robinhood;
#[cfg(test)]
pub(crate) mod test_support;
pub mod venue;

pub use alpaca::AlpacaClient;
pub use config::{LiveConfig, Secrets};
pub use decider::{Action, Decider, Decision};
pub use journal::Journal;
pub use market_state::MarketState;
pub use r#loop::{run, RunOptions, RunSummary};
pub use risk::{ApprovedOrder, RiskLimits, RiskReject, RiskVerdict};
pub use robinhood::sign;
pub use robinhood::RobinhoodClient;
