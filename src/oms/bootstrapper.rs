use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::types::Decimal;
use std::str::FromStr;
use tracing::{error, info};

use crate::oms::handler::OmsHandler;

pub struct Bootstrapper;

impl Bootstrapper {
    /// Executes the "Clean Slate" initialization protocol
    pub async fn run_startup_sequence(
        oms: &OmsHandler,
        test_market_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("=== Running Bootstrapper Sequence ===");

        // 1. Ghost Test
        Self::run_ghost_test(oms, test_market_id).await?;

        // 2. Clean Slate: Cancel All Open Orders
        Self::cancel_all_orders(oms).await?;

        // 3. (Optional) Sync External Balances here using viem/ethers equivalents
        info!("=== Bootstrapper Sequence Complete ===");

        Ok(())
    }

    async fn run_ghost_test(
        oms: &OmsHandler,
        market_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("Running Ghost Test...");
        let price = Decimal::from_str("0.01")?;
        let size = Decimal::from_str("10.0")?; // Total $0.10, the exact minimum that won't execute normally

        let u256_token =
            polymarket_client_sdk::types::U256::from_str(market_id).unwrap_or_default();

        let order = oms
            .client()
            .limit_order()
            .token_id(u256_token)
            .price(price)
            .size(size)
            .side(Side::Buy)
            .build()
            .await?;

        let signed_order = oms.client().sign(oms.signer(), order).await?;
        match oms.client().post_order(signed_order).await {
            Ok(resp) => {
                info!(
                    "Ghost Test Order POST successful (ID: {}). Cancelling...",
                    resp.order_id
                );
                // Immediately cancel
                let _ = oms.client().cancel_order(&resp.order_id).await;
                info!("Ghost Test Passed");
                Ok(())
            }
            Err(e) => {
                error!("Ghost Test Failed: {}", e);
                Err(Box::new(e))
            }
        }
    }

    async fn cancel_all_orders(oms: &OmsHandler) -> Result<(), Box<dyn std::error::Error>> {
        info!("Cleaning slate: Canceling all open orders.");
        match oms.client().cancel_all_orders().await {
            Ok(_) => {
                info!("All open orders canceled successfully.");
                Ok(())
            }
            Err(e) => {
                error!("Failed to cancel open orders: {}", e);
                // Depending on strictness, we might want to return Err here.
                Ok(())
            }
        }
    }
}
