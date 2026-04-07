use futures::StreamExt;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{error, info};
use url::Url;

use crate::core::models::BBO;

pub struct BinanceFeeder {
    symbol: String,
    tx: watch::Sender<BBO>,
}

impl BinanceFeeder {
    pub fn new(symbol: &str, tx: watch::Sender<BBO>) -> Self {
        Self {
            symbol: symbol.to_lowercase(),
            tx,
        }
    }

    pub async fn run(&self) {
        let ws_url = format!("wss://stream.binance.com:9443/ws/{}@bookTicker", self.symbol);
        let url = Url::parse(&ws_url).expect("Invalid Binance WS URL");

        info!("BinanceFeeder starting for {}", self.symbol);

        loop {
            match connect_async(url.clone()).await {
                Ok((ws_stream, _)) => {
                    info!("Connected to Binance WS for {}", self.symbol);
                    let (_, mut read) = ws_stream.split();

                    while let Some(msg) = read.next().await {
                        if let Ok(Message::Text(text)) = msg {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                if let (Some(b_str), Some(a_str)) = (json["b"].as_str(), json["a"].as_str()) {
                                    if let (Ok(bid), Ok(ask)) = (b_str.parse::<f64>(), a_str.parse::<f64>()) {
                                        let bbo = BBO { bid, ask };
                                        let _ = self.tx.send(bbo); // Non-blocking
                                    }
                                }
                            }
                        }
                    }
                    error!("Binance WS stream disconnected, reconnecting...");
                }
                Err(e) => {
                    error!("Binance WS Error: {}. Reconnecting in 5s...", e);
                    sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}
