use crate::market_config::MarketConfig;
use crate::market_watcher::MarketWatcher;
use crate::strategies::edge_strategy::EdgeStrategy;

mod market_config;
mod market_watcher;
mod polymarket_api;
mod strategies;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "polyviper=info".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let client = reqwest::Client::new();

    // ── Add / remove configs to watch more markets in parallel ────────────────
    let configs = vec![MarketConfig::btc_5m()];

    // Each config gets its own OS thread + tokio runtime. Fully self-contained.
    let handles: Vec<_> = configs
        .into_iter()
        .map(|config| {
            let label = config.label();
            MarketWatcher::spawn_thread(
                client.clone(),
                config,
                EdgeStrategy::new("0.001", true),
                tracing::info_span!("watcher", market = label),
            )
        })
        .collect();

    for h in handles {
        h.join().ok();
    }
}
