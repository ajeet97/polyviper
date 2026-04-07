use crate::core::models::{PolymarketBook, BBO};
use std::collections::HashMap;
use tokio::sync::watch;

/// The internal WatchBus. Used for non-blocking, always-latest state reads by the Strategy Engine.
#[derive(Clone)]
pub struct WatchBus {
    pub binance_eth: watch::Receiver<BBO>,
    pub binance_btc: watch::Receiver<BBO>,
    pub coinbase_eth: watch::Receiver<BBO>,
    pub dvol: watch::Receiver<f64>,
    pub polymarket_books: watch::Receiver<HashMap<String, PolymarketBook>>,
}
