use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use polymarket_client_sdk::types::Utc;
use tokio::time::sleep;
use tracing::{debug, info, warn, Instrument as _};

use crate::polymarket_api;

const MAX_MARKETS_BUFFER: usize = 5;

// ─── AssetID ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum AssetID {
    BTC,
    // ETH,
}

impl AssetID {
    pub fn as_str(&self) -> &'static str {
        match self {
            AssetID::BTC => "btc",
            // AssetID::ETH => "eth",
        }
    }
}

// ─── Market ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Market {
    pub slug: String,
    pub up_token: String,
    pub down_token: String,
    /// Unix timestamp (seconds) at which this market closes.
    pub end_time: i64,
}

impl Market {
    pub fn is_expired(&self) -> bool {
        Utc::now().timestamp() >= self.end_time
    }

    /// Seconds until this market expires. Negative if already expired.
    pub fn seconds_until_expiry(&self) -> i64 {
        self.end_time - Utc::now().timestamp()
    }
}

// ─── Shared buffer ────────────────────────────────────────────────────────────

/// Thread-safe, clonable handle to the shared market buffer.
#[derive(Clone)]
pub struct MarketBuffer(Arc<Mutex<VecDeque<Market>>>);

impl MarketBuffer {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(VecDeque::new())))
    }

    /// Peek at the first non-expired market without removing it.
    pub fn current(&self) -> Option<Market> {
        let mut buf = self.0.lock().unwrap();
        while let Some(m) = buf.front() {
            if m.is_expired() {
                let expired = buf.pop_front().unwrap();
                info!(market = %expired.slug, "purged expired market from buffer");
            } else {
                break;
            }
        }
        buf.front().cloned()
    }

    /// Peek at the *next* market after the current (index 1).
    pub fn peek_next(&self) -> Option<Market> {
        let buf = self.0.lock().unwrap();
        buf.iter().nth(1).cloned()
    }

    /// Number of markets currently buffered.
    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    fn push_back(&self, market: Market) {
        self.0.lock().unwrap().push_back(market);
    }

    fn next_slug_to_fetch(&self, asset_id: &AssetID) -> String {
        let buf = self.0.lock().unwrap();
        let ts = buf
            .back()
            .map(|m| m.end_time)
            .unwrap_or_else(|| round_down_5m(Utc::now().timestamp()));
        format!("{}-updown-5m-{}", asset_id.as_str(), ts)
    }
}

// ─── MarketFinder ─────────────────────────────────────────────────────────────

pub struct MarketFinder {
    client: reqwest::Client,
    asset_id: AssetID,
    buf: MarketBuffer,
}

impl MarketFinder {
    pub fn new(client: reqwest::Client, asset_id: AssetID) -> (Self, MarketBuffer) {
        let buf = MarketBuffer::new();
        let finder = Self {
            client,
            asset_id,
            buf: buf.clone(),
        };
        (finder, buf)
    }

    pub fn spawn(mut self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move { self.run().await }.instrument(tracing::info_span!("finder")))
    }

    async fn run(&mut self) {
        info!("finder background task started");
        loop {
            let buf_len = self.buf.len();

            if buf_len >= MAX_MARKETS_BUFFER {
                if let Some(current) = self.buf.current() {
                    let wait = current.seconds_until_expiry();
                    if wait > 0 {
                        info!(
                            buf = buf_len,
                            max = MAX_MARKETS_BUFFER,
                            wait_secs = wait,
                            market = %current.slug,
                            "buffer full, sleeping until current market expires"
                        );
                        sleep(Duration::from_secs(wait as u64)).await;
                    }
                }
                continue;
            }

            let slug = self.buf.next_slug_to_fetch(&self.asset_id);
            info!(%slug, "fetching next market");

            let t0 = Instant::now();
            match self.fetch_with_retry(&slug).await {
                Some(market) => {
                    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
                    let buf_after = self.buf.len() + 1;
                    info!(
                        market = %market.slug,
                        fetch_ms = format_args!("{elapsed_ms:.1}"),
                        buf = format_args!("{buf_after}/{MAX_MARKETS_BUFFER}"),
                        "market buffered"
                    );
                    self.buf.push_back(market);
                }
                None => {
                    warn!(%slug, "could not fetch market after retries, sleeping 1s");
                    sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    async fn fetch_with_retry(&self, slug: &str) -> Option<Market> {
        let mut delay_ms = 100u64;
        for attempt in 1..=20 {
            match polymarket_api::fetch_market_by_slug(&self.client, slug).await {
                Ok(Some(m)) => return Some(m),
                Ok(None) => {
                    debug!(%slug, attempt, "market not indexed yet");
                }
                Err(e) => {
                    warn!(%slug, attempt, error = %e, "fetch error");
                }
            }
            sleep(Duration::from_millis(delay_ms)).await;
            delay_ms = (delay_ms * 15 / 10).min(2000); // ×1.5, cap at 2 s
        }
        None
    }

    #[allow(dead_code)]
    pub fn slug_for_ts(asset_id: &AssetID, start_ts: i64) -> String {
        format!("{}-updown-5m-{}", asset_id.as_str(), start_ts)
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

pub fn round_down_5m(ts: i64) -> i64 {
    (ts / 300) * 300
}
