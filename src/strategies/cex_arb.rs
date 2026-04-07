use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, debug};

use crate::core::bus::WatchBus;
use crate::core::models::OrderIntent;
use crate::oms::risk::RiskManager;
use crate::strategies::Strategy;

/// Market Maker Arbitrage Strategy:
/// Quotes resting limit orders (as a Maker) based on the Binance/Coinbase
/// fair value to capture Polymarket spread + rebates securely.
pub struct CexArbStrategy {
    symbol: String,
    target_spread_bps: f64,
}

impl CexArbStrategy {
    pub fn new(symbol: &str, target_spread_bps: f64) -> Self {
        Self {
            symbol: symbol.to_lowercase(),
            target_spread_bps,
        }
    }
}

impl Strategy for CexArbStrategy {
    async fn run(
        &mut self,
        mut bus: WatchBus,
        risk: Arc<Mutex<RiskManager>>,
        intent_tx: mpsc::Sender<OrderIntent>,
    ) {
        info!("Starting CexArb Strategy for {}", self.symbol);
        
        loop {
            // Wait for binance tick
            if let Ok(()) = bus.binance_eth.changed().await {
                let eth_bbo = *bus.binance_eth.borrow();
                let pm_books = bus.polymarket_books.borrow().clone();
                
                // Example macro filter
                let dvol = *bus.dvol.borrow();
                
                // --- INSERT FAIR VALUE MATH HERE ---
                let fair_value = (eth_bbo.bid + eth_bbo.ask) / 2.0;

                // Simple debug output
                debug!(
                    fair_value = fair_value,
                    dvol = dvol,
                    "Tick Evaluated"
                );

                // --- INSERT ORDER BATCHING HERE ---
                // If fair value diverges from our resting orders, we
                // cancel them and send new OrderIntents to `intent_tx`.
            }
        }
    }
}
