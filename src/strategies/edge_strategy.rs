use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::info;

use crate::core::bus::WatchBus;
use crate::core::models::OrderIntent;
use crate::oms::risk::RiskManager;
use crate::strategies::Strategy;

/// Pure Arbitrage Strategy:
/// Looks for instances where the best ask of UP + best ask of DOWN < 1.0.
/// This guarantees a risk-free profit minus fees.
pub struct EdgeStrategy {
    min_edge: f64,
}

impl EdgeStrategy {
    pub fn new(min_edge: f64) -> Self {
        Self {
            min_edge,
        }
    }
}

impl Strategy for EdgeStrategy {
    async fn run(
        &mut self,
        mut bus: WatchBus,
        _risk: Arc<Mutex<RiskManager>>,
        _intent_tx: mpsc::Sender<OrderIntent>,
    ) {
        info!("Starting Pure Edge Strategy (min_edge: {})", self.min_edge);
        
        loop {
            if let Ok(()) = bus.polymarket_books.changed().await {
                let books = bus.polymarket_books.borrow().clone();

                // If we don't have exactly 2 keys (UP and DOWN), we skip safely.
                if books.len() == 2 {
                    let mut asks = books.values().map(|b| b.best_ask);
                    if let (Some(a1), Some(a2)) = (asks.next(), asks.next()) {
                        let cost = a1 + a2;
                        let edge = 1.0 - cost;
                        
                        // If taking both sides yields positive edge
                        if cost > 0.0 && edge >= self.min_edge {
                            info!("Pure Arbitrage Found! Edge: {:.4} (Cost: {:.4})", edge, cost);
                            // Normally we would blast 2 limit taker OrderIntents here.
                        }
                    }
                }
            }
        }
    }
}
