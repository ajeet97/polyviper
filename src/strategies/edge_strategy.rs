use polymarket_client_sdk::types::Decimal;
use std::str::FromStr;
use tracing::info;

use crate::market_config::Market;
use crate::market_watcher::{MarketBook, Strategy};

pub struct EdgeStrategy {
    min_edge: Decimal,
    simulate: bool,
}

impl EdgeStrategy {
    pub fn new(min_edge_str: &str, simulate: bool) -> Self {
        Self {
            min_edge: Decimal::from_str(min_edge_str).expect("invalid decimal string for min_edge"),
            simulate: simulate,
        }
    }
}

impl Strategy for EdgeStrategy {
    fn on_market_change(&mut self, previous: Option<&Market>, current: &Market) {
        match previous {
            Some(prev) => info!(from = %prev.slug, to = %current.slug, "🔄 market rotated"),
            None => info!(market = %current.slug, "🚀 first market"),
        }
    }

    fn on_book_update(&mut self, market: &Market, book: &MarketBook) {
        let (Some(up_ask), Some(down_ask)) = (book.up.best_ask, book.down.best_ask) else {
            return;
        };

        let cost = up_ask + down_ask;
        let edge = Decimal::ONE - cost;

        let up_vol_usd = up_ask * book.up.best_ask_size.unwrap_or(Decimal::ZERO);
        let down_vol_usd = down_ask * book.down.best_ask_size.unwrap_or(Decimal::ZERO);
        let volume_usd = up_vol_usd + down_vol_usd;

        if edge >= self.min_edge {
            if self.simulate {
                info!(
                    market = %market.slug,
                    up_ask = %up_ask,
                    down_ask = %down_ask,
                    edge = %edge,
                    up_vol_usd = %up_vol_usd,
                    down_vol_usd = %down_vol_usd,
                    volume_usd = %volume_usd,
                    "🤑 SIMULATE EXECUTE"
                );
            } else {
                // Actual execution here
            }
        }
    }
}
