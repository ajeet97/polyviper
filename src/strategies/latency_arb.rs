use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::info;

use crate::core::bus::WatchBus;
use crate::core::models::OrderIntent;
use crate::oms::risk::RiskManager;
use crate::strategies::Strategy;
use polymarket_client_sdk::clob::types::Side;

/// The "0x8dxd" Latency Arbitrage Strategy:
/// Monitors Binance WS for sharp price moves. If 5-min/15-min PM contracts deviate
/// from Binance True Price by more than 3-5%, instantly hits the book as a taker.
pub struct LatencyArbStrategy {
    symbol: String,
    divergence_threshold: f64, // e.g., 0.035 for 3.5%
}

impl LatencyArbStrategy {
    pub fn new(symbol: &str, divergence_threshold: f64) -> Self {
        Self {
            symbol: symbol.to_lowercase(),
            divergence_threshold,
        }
    }
}

impl Strategy for LatencyArbStrategy {
    async fn run(
        &mut self,
        mut bus: WatchBus,
        risk: Arc<Mutex<RiskManager>>,
        intent_tx: mpsc::Sender<OrderIntent>,
    ) {
        info!(
            "Starting Latency Arbitrage Strategy (divergence threshold: {:.2}%)",
            self.divergence_threshold * 100.0
        );

        loop {
            // Wake up ideally the millisecond Binance pushes a new price
            if let Ok(()) = bus.binance_eth.changed().await {
                let eth_bbo = *bus.binance_eth.borrow();
                let pm_books = bus.polymarket_books.borrow().clone();

                // Pure proxy metric for "true" probability.
                // Actual production system would map the CEX price directly
                // to a Black-Scholes binary option probability formula.
                // We mock it for the skeleton logic.
                let true_prob = 0.50; // Mock derived from Binance price standard dev

                // Compare true_prob to the implied probability on Polymarket
                // (e.g. up_token best ask and bid)

                for (token_id, book) in pm_books {
                    let implied_prob = book.best_ask; // What PM thinks the probability is
                    if implied_prob == 0.0 {
                        continue;
                    }

                    let edge = (true_prob - implied_prob).abs();

                    if edge > self.divergence_threshold {
                        info!(
                            token_id = %token_id,
                            edge = edge,
                            pm_implied = implied_prob,
                            true_p = true_prob,
                            "LATENCY ARBITRAGE OPPORTUNITY DETECTED"
                        );

                        let r = risk.lock().await;

                        // Check system safety
                        if r.validate_order(1.0).is_err() {
                            continue; // Skip, killswitches active
                        }

                        // Determine position size based on edge utilizing 8% Kelly cap
                        let order_size_usd = r.calculate_kelly_size(edge);

                        let intent = OrderIntent {
                            intent_id: format!("lat_arb_{}", uuid::Uuid::new_v4()),
                            market_id: String::new(), // Passed manually if needed
                            token_id: token_id.clone(),
                            price: implied_prob, // Taker taking the ask
                            size: order_size_usd / implied_prob, // number of shares
                            side: Side::Buy,
                        };

                        let _ = intent_tx.send(intent).await;
                    }
                }
            }
        }
    }
}
