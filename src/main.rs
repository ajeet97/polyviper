pub mod core;
pub mod feeders;
pub mod market_config;
pub mod oms;
pub mod polymarket_api;
pub mod strategies;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};
use tracing::{info, warn};

use crate::core::bus::WatchBus;
use crate::core::models::{OrderIntent, BBO};
use crate::feeders::binance::BinanceFeeder;
use crate::feeders::deribit::DeribitFeeder;
use crate::feeders::polymarket::PolymarketFeeder;
use crate::market_config::MarketConfig;
use crate::oms::bootstrapper::Bootstrapper;
use crate::oms::handler::OmsHandler;
use crate::oms::risk::RiskManager;
use crate::strategies::Strategy;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "polyviper=info".parse().unwrap()),
        )
        .with_target(false)
        .init();

    info!("Starting PolyViper HFT Engine (Maker-First)");

    // 1. Setup Event Bus (watch channels)
    // We initialize them with default/empty values
    let (binance_eth_tx, binance_eth_rx) = watch::channel(BBO::default());
    let (binance_btc_tx, binance_btc_rx) = watch::channel(BBO::default());
    let (coinbase_eth_tx, coinbase_eth_rx) = watch::channel(BBO::default());
    let (dvol_tx, dvol_rx) = watch::channel(0.0);
    let (pm_tx, pm_rx) = watch::channel(Default::default()); // Option<(Market, HashMap<String, PolymarketBook>)>
                                                             // wait, my WatchBus has watch::Receiver<HashMap<String, PolymarketBook>>

    // 2. Setup OMS Channels
    let (intent_tx, intent_rx) = mpsc::channel::<OrderIntent>(1000);

    // 3. Initialize Shared State (Risk)
    let risk_manager = Arc::new(Mutex::new(RiskManager::new(
        1000.0, // INITIAL PORTFOLIO ($1000)
        2.0,    // KILLSWITCH (e.g. $2 divergence)
    )));

    // 4. Initialize Core Engine Modules (Simulated credentials for now if not in env)
    let private_key = std::env::var("PRIVATE_KEY").unwrap_or_else(|_| {
        "0x0000000000000000000000000000000000000000000000000000000000000001".to_string()
    });

    // Spawn OMS Handler mapping Intent -> Clob orders
    let mut oms = OmsHandler::new(&private_key, None, intent_rx).await?;

    // Bootstrapper Sequence (Ghost Test & Clean Slate)
    let is_simulation = std::env::var("SIMULATE").unwrap_or_else(|_| "true".to_string()) == "true";
    if !is_simulation {
        // Will fail cleanly if API keys are fake 0x1
        Bootstrapper::run_startup_sequence(&oms, "TEST_MARKET_ID")
            .await
            .unwrap_or_else(|e| {
                warn!("Bootstrapper sequence hit an error: {}", e);
            });

        tokio::spawn(async move {
            oms.run().await;
        });
    } else {
        info!("SIMULATE mode enabled, OMS routing bypassed for real API.");
    }

    // 5. Spawn Feeders
    let binance = BinanceFeeder::new("ethusdt", binance_eth_tx);
    tokio::spawn(async move {
        binance.run().await;
    });

    let deribit = DeribitFeeder::new(dvol_tx);
    tokio::spawn(async move {
        deribit.run().await;
    });

    let req_client = reqwest::Client::new();
    let pm_config = MarketConfig::btc_5m(); // Example
    let mut pm_feeder = PolymarketFeeder::new(req_client, pm_config, pm_tx);
    // Let's adapt WatchBus
    // Note: the WatchBus expects simple HashMap, but PM channel uses Option<(Market, book)>
    // So we need a mapper task if we want to extract just the map for WatchBus, or we update WatchBus definition.
    // For now, let's just spawn it.
    tokio::spawn(async move {
        pm_feeder.run().await;
    });

    // 6. Start the Maker Strategy Loop (Blocking indefinitely)
    // Since pm_rx is an Option tuple, the CexArbStrategy will need to map it inside, so we'll
    // update WatchBus manually out of band to match, but here we just instantiate what we have.

    // Let's fix the WatchBus construct
    let (pm_map_tx, pm_map_rx) = watch::channel(HashMap::new());

    let mut pm_rx_loop = pm_rx.clone();
    tokio::spawn(async move {
        while pm_rx_loop.changed().await.is_ok() {
            if let Some((_, books)) = pm_rx_loop.borrow().clone() {
                let _ = pm_map_tx.send(books);
            }
        }
    });
    let bus = WatchBus {
        binance_eth: binance_eth_rx,
        binance_btc: binance_btc_rx,
        coinbase_eth: coinbase_eth_rx,
        dvol: dvol_rx,
        polymarket_books: pm_map_rx,
    };

    use crate::strategies::latency_arb::LatencyArbStrategy;
    let mut strategy = LatencyArbStrategy::new("ethusdt", 0.035); // 3.5% divergence

    info!("All systems green. Transferring control to Latency Arb Strategy.");
    strategy.run(bus, risk_manager, intent_tx).await;

    Ok(())
}
