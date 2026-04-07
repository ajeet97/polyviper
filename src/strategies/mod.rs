pub mod cex_arb;
pub mod edge_strategy;
pub mod latency_arb;

use crate::core::bus::WatchBus;
use crate::core::models::OrderIntent;
use crate::oms::risk::RiskManager;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

pub trait Strategy: Send + Sync {
    fn run(
        &mut self,
        bus: WatchBus,
        risk: Arc<Mutex<RiskManager>>,
        intent_tx: mpsc::Sender<OrderIntent>,
    ) -> impl std::future::Future<Output = ()> + Send;
}
