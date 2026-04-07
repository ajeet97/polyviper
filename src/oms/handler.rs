use alloy_signer_local::LocalSigner;
use k256::ecdsa::SigningKey;
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::Normal;
use polymarket_client_sdk::auth::Signer;
use polymarket_client_sdk::clob::types::SignatureType;
use polymarket_client_sdk::clob::{Client as ClobClient, Config as ClobConfig};
use polymarket_client_sdk::types::Decimal;
use polymarket_client_sdk::POLYGON;
use std::str::FromStr;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::core::models::OrderIntent;

pub struct OmsHandler {
    client: ClobClient<Authenticated<Normal>>,
    signer: LocalSigner<SigningKey>,
    rx: mpsc::Receiver<OrderIntent>,
}

impl OmsHandler {
    pub async fn new(
        private_key: &str,
        funder_address: Option<&str>,
        rx: mpsc::Receiver<OrderIntent>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let signer = LocalSigner::from_str(private_key)?.with_chain_id(Some(POLYGON));
        let client_builder = ClobClient::new("https://clob.polymarket.com", ClobConfig::default())?;

        let mut auth_builder = client_builder.authentication_builder(&signer);

        if let Some(funder) = funder_address {
            let addr = polymarket_client_sdk::types::Address::from_str(funder)?;
            auth_builder = auth_builder.funder(addr);
            auth_builder = auth_builder.signature_type(SignatureType::GnosisSafe);
            info!("Initialized OMS with Proxy Funder Address: {}", funder);
        } else {
            info!("Initialized OMS with Direct EOA");
        }

        let client = auth_builder.authenticate().await?;

        Ok(Self { client, signer, rx })
    }

    pub fn client(&self) -> &ClobClient<Authenticated<Normal>> {
        &self.client
    }

    pub fn signer(&self) -> &LocalSigner<SigningKey> {
        &self.signer
    }

    pub async fn run(&mut self) {
        info!("OmsHandler queue listener started");
        while let Some(intent) = self.rx.recv().await {
            self.execute_intent(intent).await;
        }
    }

    async fn execute_intent(&self, intent: OrderIntent) {
        let price_dec = Decimal::from_str(&format!("{:.2}", intent.price)).unwrap_or_default();
        let size_dec = Decimal::from_str(&format!("{:.2}", intent.size)).unwrap_or_default();

        if price_dec <= Decimal::ZERO || size_dec <= Decimal::ZERO {
            error!(intent_id = %intent.intent_id, "Ignored order with 0 size or price");
            return;
        }

        let u256_token =
            polymarket_client_sdk::types::U256::from_str(&intent.token_id).unwrap_or_default();

        let order_future = self
            .client
            .limit_order()
            .token_id(u256_token)
            .price(price_dec)
            .size(size_dec)
            .side(intent.side)
            .build();

        match order_future.await {
            Ok(order) => match self.client.sign(&self.signer, order).await {
                Ok(signed_order) => match self.client.post_order(signed_order).await {
                    Ok(resp) => {
                        info!(
                            intent_id = %intent.intent_id,
                            order_id = %resp.order_id,
                            "Order Successfully Submitted"
                        );
                    }
                    Err(e) => error!(intent_id = %intent.intent_id, "Failed to post order: {}", e),
                },
                Err(e) => error!(intent_id = %intent.intent_id, "Failed to sign order: {}", e),
            },
            Err(e) => error!(intent_id = %intent.intent_id, "Failed to build order: {}", e),
        }
    }
}
