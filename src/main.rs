use polymarket_client_sdk::types::Decimal;
use tracing::{info, Instrument as _};

use crate::market_finder::{AssetID, Market, MarketFinder};
use crate::market_watcher::{MarketBook, MarketWatcher, Strategy};

mod market_finder;
mod market_watcher;
mod polymarket_api;

// ── Demo strategy ─────────────────────────────────────────────────────────────

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

        // In a binary market UP + DOWN always settle to 1.0 combined.
        // Buying both legs costs (up_ask + down_ask).
        // If that total < 1.0 you lock in (1.0 - total) risk-free profit.
        let cost = up_ask + down_ask;
        let edge = Decimal::ONE - cost;

        if edge > Decimal::ZERO {
            info!(
                market = %market.slug,
                up_ask = %up_ask,
                down_ask = %down_ask,
                cost = %cost,
                edge = %edge,
                "⚡ SIMULATE EXECUTE"
            );
        } else {
            // info!(
            //     market = %market.slug,
            //     up_ask = %up_ask,
            //     down_ask = %down_ask,
            //     cost = %cost,
            //     edge = %edge,
            //     "no edge"
            // );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // Initialise tracing. Set RUST_LOG to control verbosity:
    //   RUST_LOG=info cargo run      → info + above
    //   RUST_LOG=debug cargo run     → includes latency fields
    //   RUST_LOG=polyviper=debug     → scoped to this crate
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "polyviper=info".parse().unwrap()),
        )
        .with_target(false) // cleaner output without module paths
        .init();

    let client = reqwest::Client::new();
    let (finder, buf) = MarketFinder::new(client, AssetID::BTC);
    finder.spawn();

    let mut watcher = MarketWatcher::new(buf, LogStrategy);
    watcher
        .run()
        .instrument(tracing::info_span!("watcher"))
        .await;
}
