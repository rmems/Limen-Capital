//! Robinhood Crypto Trading API adapter.
//!
//! Docs: <https://docs.robinhood.com/crypto/trading/> (verified 2026-11-08).
//! Base URL `https://trading.robinhood.com`; every request carries
//! `x-api-key`, `x-signature`, `x-timestamp` (see [`sign`]).

pub mod sign;
