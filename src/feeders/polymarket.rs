use futures::StreamExt;
use polymarket_client_sdk::clob::ws::types::response::BookUpdate;
use polymarket_client_sdk::clob::ws::Client as WsClient;
use polymarket_client_sdk::types::U256;
use std::collections::HashMap;
use std::str::FromStr;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::time::sleep_until;
use tracing::{info, warn};

use crate::core::models::PolymarketBook;
use crate::market_config::{Market, MarketConfig};
use crate::polymarket_api::fetch_market_with_retry;

pub struct PolymarketFeeder {
    client: reqwest::Client,
    config: MarketConfig,
    ws: WsClient,
    tx: watch::Sender<Option<(Market, HashMap<String, PolymarketBook>)>>,
}

impl PolymarketFeeder {
    pub fn new(
        client: reqwest::Client,
        config: MarketConfig,
        tx: watch::Sender<Option<(Market, HashMap<String, PolymarketBook>)>>,
    ) -> Self {
        Self {
            client,
            config,
            ws: WsClient::default(),
            tx,
        }
    }

    pub async fn run(&mut self) {
        info!("PolymarketFeeder starting for {}", self.config.label());

        let first_slug = self.config.slug_for_end_ts(
            self.config
                .round_down(polymarket_client_sdk::types::Utc::now().timestamp()),
        );

        let first_market = loop {
            match fetch_market_with_retry(&self.client, &first_slug).await {
                Some(m) => break m,
                None => {
                    warn!("could not fetch first market, retrying in 1s");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        };

        let mut active_market = first_market;
        let mut active_stream = self.subscribe_market(&active_market).await;
        let mut active_books: HashMap<String, PolymarketBook> = HashMap::new();

        let mut standby_market: Option<Market> = None;
        let mut standby_fetch: Option<tokio::task::JoinHandle<Option<Market>>> = None;
        let mut standby_stream = None;

        // Publish the initial empty state
        let _ = self.tx.send(Some((active_market.clone(), active_books.clone())));

        loop {
            // Check Rotation
            if active_market.is_expired() {
                self.unsubscribe_market(&active_market);
                
                if let Some(h) = standby_fetch.take() {
                    h.abort();
                }

                if let Some(sm) = standby_market.take() {
                    if !sm.is_expired() {
                        info!(market = %sm.slug, "Promoting standby market");
                        active_market = sm;
                        active_stream = standby_stream.take();
                    } else {
                        active_market = self.fetch_next_market(&active_market).await;
                        active_stream = self.subscribe_market(&active_market).await;
                    }
                } else {
                    active_market = self.fetch_next_market(&active_market).await;
                    active_stream = self.subscribe_market(&active_market).await;
                }

                active_books.clear();
                let _ = self.tx.send(Some((active_market.clone(), active_books.clone())));
            }

            let stream = match active_stream.as_mut() {
                Some(s) => s,
                None => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };

            let deadline = tokio::time::Instant::now()
                + Duration::from_secs(active_market.seconds_until_expiry().max(0) as u64);

            'events: loop {
                if standby_fetch.is_none() && standby_market.is_none() {
                    let next_slug = self.config.slug_for_end_ts(active_market.end_time);
                    let client2 = self.client.clone();
                    standby_fetch = Some(tokio::spawn(async move {
                        fetch_market_with_retry(&client2, &next_slug).await
                    }));
                }

                tokio::select! {
                    biased;

                    maybe_event = stream.next() => match maybe_event {
                        None => {
                            warn!("active stream closed");
                            active_stream = None;
                            break 'events;
                        }
                        Some(Err(e)) => {
                            warn!(error = %e, "WS stream error");
                            active_stream = None;
                            break 'events;
                        }
                        Some(Ok(event)) => {
                            let mut changed = Self::apply_update(&event, &mut active_books);
                            // Drain loop
                            while let Ok(Some(Ok(ev))) = tokio::time::timeout(Duration::from_micros(1), stream.next()).await {
                                changed |= Self::apply_update(&ev, &mut active_books);
                            }
                            if changed {
                                // Broadcast!
                                let _ = self.tx.send(Some((active_market.clone(), active_books.clone())));
                            }
                        }
                    },

                    _ = sleep_until(deadline) => {
                        info!("Market expired from timer");
                        break 'events;
                    },

                    result = async {
                        match standby_fetch.as_mut() {
                            Some(h) => h.await.ok().unwrap_or(None),
                            None => std::future::pending().await,
                        }
                    } => {
                        standby_fetch = None;
                        if let Some(market) = result {
                            info!("Standby metadata ready for {}", market.slug);
                            standby_stream = self.subscribe_market(&market).await;
                            standby_market = Some(market);
                        }
                    }
                }
            }
        }
    }

    async fn fetch_next_market(&self, prev: &Market) -> Market {
        let next_slug = self.config.slug_for_end_ts(prev.end_time);
        info!("Cold subscribe to {}", next_slug);
        loop {
            match fetch_market_with_retry(&self.client, &next_slug).await {
                Some(m) => return m,
                None => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }

    async fn subscribe_market(
        &self,
        market: &Market,
    ) -> Option<std::pin::Pin<Box<dyn futures::Stream<Item = polymarket_client_sdk::Result<BookUpdate>> + Send>>> {
        let up = U256::from_str(&market.up_token).ok()?;
        let down = U256::from_str(&market.down_token).ok()?;

        match self.ws.subscribe_orderbook(vec![up, down]) {
            Ok(s) => Some(Box::pin(s)),
            Err(e) => {
                warn!("subscribe_orderbook failed: {}", e);
                None
            }
        }
    }

    fn unsubscribe_market(&self, market: &Market) {
        if let (Ok(up), Ok(down)) = (
            U256::from_str(&market.up_token),
            U256::from_str(&market.down_token),
        ) {
            let _ = self.ws.unsubscribe_prices(&[up, down]);
        }
    }

    fn apply_update(event: &BookUpdate, books: &mut HashMap<String, PolymarketBook>) -> bool {
        let token_id = event.asset_id.to_string();
        let book = books.entry(token_id).or_default();

        let mut changed = false;
        if let Some(best_bid) = event.bids.first() {
            let p = best_bid.price.to_string().parse::<f64>().unwrap_or(0.0);
            let s = best_bid.size.to_string().parse::<f64>().unwrap_or(0.0);
            if book.best_bid != p || book.best_bid_size != s {
                book.best_bid = p;
                book.best_bid_size = s;
                changed = true;
            }
        }

        if let Some(best_ask) = event.asks.last() {
            let p = best_ask.price.to_string().parse::<f64>().unwrap_or(0.0);
            let s = best_ask.size.to_string().parse::<f64>().unwrap_or(0.0);
            if book.best_ask != p || book.best_ask_size != s {
                book.best_ask = p;
                book.best_ask_size = s;
                changed = true;
            }
        }

        changed
    }
}
