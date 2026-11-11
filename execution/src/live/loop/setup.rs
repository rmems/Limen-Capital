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
        notifier: Box::new(crate::live::notify::DesktopNotifier),
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
        MarketSourceConfig::Alpaca => build_alpaca_market(cfg, secrets).await,
    }
}

async fn build_alpaca_market(
    cfg: &LiveConfig,
    secrets: &Secrets,
) -> Result<MarketParts, LoopError> {
    let (api_key, secret_key) = secrets
        .alpaca()
        .map_err(|e| LoopError::Config(e.to_string()))?;
    let client = AlpacaClient::new(api_key, secret_key)?;
    let account = client.get_account().await?;
    let asset = client.get_asset(&cfg.symbol).await?;
    // Conservative equity rules for the paper book. Live submission remains disabled.
    let pair = PairRules {
        symbol: asset.symbol,
        asset_increment: if asset.fractionable { 1e-9 } else { 1.0 },
        quote_increment: 0.01,
        min_order_amount: Some(1.0),
        max_order_size: None,
        is_api_tradable: asset.tradable && asset.status == "active",
    };
    let line = format!(
        "Alpaca status={} buying_power={} symbol={} tradable={} fractional={}",
        account.status, account.buying_power, pair.symbol, pair.is_api_tradable, asset.fractionable
    );
    Ok(MarketParts {
        market: Box::new(AlpacaMarket { client }),
        pair,
        account_line: Some(line),
    })
}
