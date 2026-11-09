//! Construct the paper loop's venue, account metadata, and decider.

use super::*;

struct MarketParts {
    market: Box<dyn MarketSource>,
    pair: PairRules,
    account_line: Option<String>,
}

/// Resolve market source, pair rules, decider and paper book.
pub async fn build_parts(cfg: &LiveConfig, secrets: &Secrets) -> Result<LoopParts, LoopError> {
    cfg.validate()
        .map_err(|e| LoopError::Config(e.to_string()))?;
    if !cfg.paper {
        return Err(LoopError::Config(
            "live order submission is not implemented yet; run with paper = true / --paper".into(),
        ));
    }
    let source = build_market(cfg, secrets).await?;
    let decider: Box<dyn Decider> = match cfg.decider {
        DeciderKind::Rules => Box::new(RulesDecider::new(cfg.rules.clone())),
        DeciderKind::Claude => {
            return Err(LoopError::Config(
                "decider = \"claude\" is not implemented yet; use decider = \"rules\"".into(),
            ))
        }
    };
    Ok(LoopParts {
        market: source.market,
        pair: source.pair,
        decider,
        book: PaperBook::new(cfg.paper_starting_cash_usd, cfg.paper_fee_ratio),
        journal: Journal::new(&cfg.journal_path),
        account_line: source.account_line,
        day_start_realized: None,
    })
}

async fn build_market(cfg: &LiveConfig, secrets: &Secrets) -> Result<MarketParts, LoopError> {
    match &cfg.market {
        MarketSourceConfig::Synthetic {
            seed,
            start_price,
            vol,
            half_spread_bps,
        } => Ok(MarketParts {
            market: Box::new(SyntheticMarket::new(
                *seed,
                *start_price,
                *vol,
                *half_spread_bps,
            )),
            pair: SyntheticMarket::pair_rules(&cfg.symbol),
            account_line: None,
        }),
        MarketSourceConfig::Robinhood => build_robinhood_market(cfg, secrets).await,
    }
}

async fn build_robinhood_market(
    cfg: &LiveConfig,
    secrets: &Secrets,
) -> Result<MarketParts, LoopError> {
    let (api_key, priv_key) = secrets
        .robinhood()
        .map_err(|e| LoopError::Config(e.to_string()))?;
    let mut client = RobinhoodClient::new(api_key, priv_key, &cfg.base_url)?;
    let account = client.get_account().await?;
    let pair = client.trading_pair(&cfg.symbol).await?;
    let holding = client.holding(cfg.asset_code()).await?;
    let line = format!(
        "account {} status={} buying_power={:.2} {} api_tradable={:?} | holding {}={} | pair {} api_tradable={} asset_inc={} quote_inc={} min_order={:?}",
        account.account_number,
        account.status,
        account.buying_power,
        account.buying_power_currency,
        account.is_api_tradable,
        cfg.asset_code(),
        holding.map(|h| h.total_quantity).unwrap_or(0.0),
        pair.symbol,
        pair.is_api_tradable,
        pair.asset_increment,
        pair.quote_increment,
        pair.min_order_amount,
    );
    Ok(MarketParts {
        market: Box::new(RobinhoodMarket { client }),
        pair: PairRules::from(&pair),
        account_line: Some(line),
    })
}
