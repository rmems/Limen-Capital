//! Quote sources: the venue, or a seeded synthetic walk for offline paper runs.

use crate::live::risk::PairRules;
use crate::live::robinhood::types::BestBidAsk;
use crate::live::robinhood::{ClientError, RobinhoodClient};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::future::Future;
use std::pin::Pin;

pub type QuoteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BestBidAsk, ClientError>> + Send + 'a>>;

pub trait MarketSource: Send {
    fn label(&self) -> String;
    fn quote<'a>(&'a mut self, symbol: &'a str) -> QuoteFuture<'a>;
}

/// Live quotes from Robinhood.
pub struct RobinhoodMarket {
    pub client: RobinhoodClient,
}

impl MarketSource for RobinhoodMarket {
    fn label(&self) -> String {
        format!("robinhood ({})", self.client.base_url())
    }

    fn quote<'a>(&'a mut self, symbol: &'a str) -> QuoteFuture<'a> {
        Box::pin(async move { self.client.best_bid_ask(symbol).await })
    }
}

/// Seeded geometric random walk with a fixed half-spread. Paper mode only.
pub struct SyntheticMarket {
    rng: StdRng,
    price: f64,
    vol: f64,
    half_spread_bps: f64,
    seed: u64,
    quote_increment: f64,
}

impl SyntheticMarket {
    pub fn new(seed: u64, start_price: f64, vol: f64, half_spread_bps: f64) -> Self {
        Self {
            rng: StdRng::seed_from_u64(seed),
            price: start_price,
            vol,
            half_spread_bps,
            seed,
            quote_increment: 0.01,
        }
    }

    /// Standard normal via Box–Muller (keeps `rand_distr` out of the tree).
    fn gauss(&mut self) -> f64 {
        let u1: f64 = self.rng.gen_range(f64::EPSILON..1.0);
        let u2: f64 = self.rng.gen_range(0.0..1.0);
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    pub fn next_quote(&mut self, symbol: &str) -> BestBidAsk {
        let z = self.gauss();
        self.price *= (self.vol * z).exp();
        let half = self.price * self.half_spread_bps / 10_000.0;
        let q = self.quote_increment;
        let bid = ((self.price - half) / q).floor() * q;
        let ask = ((self.price + half) / q).ceil() * q;
        BestBidAsk {
            symbol: symbol.to_string(),
            bid,
            ask: ask.max(bid),
        }
    }

    pub fn pair_rules(symbol: &str) -> PairRules {
        PairRules {
            symbol: symbol.to_string(),
            ..PairRules::btc_usd_defaults()
        }
    }
}

impl MarketSource for SyntheticMarket {
    fn label(&self) -> String {
        format!(
            "synthetic (seed={}, vol={}, half_spread_bps={})",
            self.seed, self.vol, self.half_spread_bps
        )
    }

    fn quote<'a>(&'a mut self, symbol: &'a str) -> QuoteFuture<'a> {
        let q = self.next_quote(symbol);
        Box::pin(async move { Ok(q) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_is_seeded_and_sane() {
        let mut a = SyntheticMarket::new(7, 100_000.0, 0.002, 1.0);
        let mut b = SyntheticMarket::new(7, 100_000.0, 0.002, 1.0);
        for _ in 0..50 {
            let qa = a.next_quote("BTC-USD");
            let qb = b.next_quote("BTC-USD");
            assert_eq!(qa, qb, "same seed → same path");
            assert!(qa.is_sane());
            assert!(
                qa.spread_bps() >= 1.9 && qa.spread_bps() < 3.0,
                "{}",
                qa.spread_bps()
            );
            assert!(qa.mid() > 50_000.0 && qa.mid() < 200_000.0);
        }
        let mut c = SyntheticMarket::new(8, 100_000.0, 0.002, 1.0);
        assert_ne!(
            c.next_quote("BTC-USD"),
            SyntheticMarket::new(7, 100_000.0, 0.002, 1.0).next_quote("BTC-USD")
        );
        assert!(c.label().contains("seed=8"));
    }
}
