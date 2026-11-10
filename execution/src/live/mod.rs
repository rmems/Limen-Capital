//! Live micro-capital experiment (RM-2436).
//!
//! A deliberately small, explicitly **non-research** live trading experiment:
//! a Robinhood Crypto venue adapter plus a decision loop that trades a fixed
//! micro-budget with hard limits enforced in code (`risk`). The decider is
//! advisory; the risk module owns execution and rejects before any HTTP call
//! reaches the venue.
//!
//! Everything here is behind the `live-robinhood` Cargo feature (off by
//! default) and never touches the Julia brain, wire fixtures or the paper
//! simulation contracts.
//!
//! Secrets are read from the environment only:
//! `ROBINHOOD_API_KEY`, `ROBINHOOD_PRIVATE_KEY`, `ANTHROPIC_API_KEY`.

pub mod config;
pub mod cost;
pub mod decider;
pub mod journal;
pub mod r#loop;
pub mod market;
pub mod market_state;
pub mod paper;
pub mod postmortem;
pub mod risk;
pub mod robinhood;
#[cfg(test)]
pub(crate) mod test_support;

pub use config::{LiveConfig, Secrets};
pub use decider::{Action, Decider, Decision};
pub use journal::Journal;
pub use market_state::MarketState;
pub use r#loop::{run, RunOptions, RunSummary};
pub use risk::{ApprovedOrder, RiskLimits, RiskReject, RiskVerdict};
pub use robinhood::sign;
pub use robinhood::RobinhoodClient;
