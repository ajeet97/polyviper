use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{error, info};
use url::Url;

pub struct DeribitFeeder {
    tx: watch::Sender<f64>,
}

impl DeribitFeeder {
    pub fn new(tx: watch::Sender<f64>) -> Self {
        Self { tx }
    }

    pub async fn run(&self) {
        let url = Url::parse("wss://www.deribit.com/ws/api/v2").unwrap();
        info!("DeribitFeeder starting for DVOL");

        loop {
            match connect_async(url.clone()).await {
                Ok((mut ws_stream, _)) => {
                    info!("Connected to Deribit WS");
                    let subscribe_msg = serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "public/subscribe",
                        "id": 1,
                        "params": {
                            "channels": ["deribit_volatility_index.eth_usd"] // Also captures global crypto vol generally
                        }
                    });

                    if ws_stream
                        .send(Message::Text(subscribe_msg.to_string()))
                        .await
                        .is_err()
                    {
                        error!("Failed to subscribe to Deribit DVOL");
                        sleep(Duration::from_secs(5)).await;
                        continue;
                    }

                    let (_, mut read) = ws_stream.split();
                    while let Some(msg) = read.next().await {
                        if let Ok(Message::Text(text)) = msg {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                if json["method"] == "subscription" {
                                    if let Some(val) = json["params"]["data"]["volatility"].as_f64()
                                    {
                                        let _ = self.tx.send(val);
                                    }
                                }
                            }
                        }
                    }
                    error!("Deribit WS stream disconnected, reconnecting...");
                }
                Err(e) => {
                    error!("Deribit WS Error: {}. Reconnecting in 5s...", e);
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}
