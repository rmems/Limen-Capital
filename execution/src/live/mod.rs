//! Live micro-capital experiment (RM-2436).
//!
//! A deliberately small, explicitly **non-research** live trading experiment:
//! an Alpaca equity venue adapter plus a decision loop that trades a fixed
//! micro-budget with hard limits enforced in code (`risk`). The decider is
//! advisory; the risk module owns execution and rejects before any HTTP call
//! reaches the venue.
//!
//! Everything here is behind the `live-alpaca` Cargo feature (off by
//! default) and never touches the Julia brain, wire fixtures or the paper
//! simulation contracts.
//!
//! Secrets are read from the environment only:
//! `APCA_API_KEY_ID`, `APCA_API_SECRET_KEY`, `ANTHROPIC_API_KEY`.

pub mod alpaca;
pub mod config;
pub mod decider;
pub mod journal;
pub mod r#loop;
pub mod market;
pub mod market_state;
pub mod notify;
pub mod paper;
pub mod risk;
#[cfg(test)]
pub(crate) mod test_support;

pub use alpaca::AlpacaClient;
pub use config::{LiveConfig, Secrets};
pub use decider::{Action, Decider, Decision};
pub use journal::Journal;
pub use market_state::MarketState;
pub use r#loop::{run, RunOptions, RunSummary};
pub use risk::{ApprovedOrder, RiskLimits, RiskReject, RiskVerdict};
