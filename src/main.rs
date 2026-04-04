use polymarket_client_sdk::types::Decimal;
use tracing::info;

use crate::market_config::{Market, MarketConfig};
use crate::market_watcher::{MarketBook, MarketWatcher, Strategy};

mod market_config;
mod market_watcher;
mod polymarket_api;

// ── Strategy ──────────────────────────────────────────────────────────────────

struct LogStrategy;

impl Strategy for LogStrategy {
    fn on_market_change(&mut self, previous: Option<&Market>, current: &Market) {
        match previous {
            Some(prev) => info!(from = %prev.slug, to = %current.slug, "🔄 market rotated"),
            None => info!(market = %current.slug, "🚀 first market"),
        }
    }

    fn on_book_update(&mut self, market: &Market, book: &MarketBook) {
        if !book.is_ready() {
            return;
        }
        let (Some(up_ask), Some(down_ask)) = (book.up.best_ask, book.down.best_ask) else {
            return;
        };
        let cost = up_ask + down_ask;
        let edge = Decimal::ONE - cost;
        if edge > Decimal::ZERO {
            info!(
                market = %market.slug,
                up_ask = %up_ask,
                down_ask = %down_ask,
                edge = %edge,
                "⚡ SIMULATE EXECUTE"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

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
                LogStrategy,
                tracing::info_span!("watcher", market = label),
            )
        })
        .collect();

    for h in handles {
        h.join().ok();
    }
}
