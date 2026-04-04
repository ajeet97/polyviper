//! MarketWatcher — self-contained, single-thread market monitoring.
//!
//! Each watcher runs on its **own OS thread** with a dedicated `current_thread`
//! tokio runtime. All async work — Gamma API fetch, WebSocket subscription,
//! event processing, standby pre-arming — runs concurrently on that one thread.
//!
//! # Concurrency model
//!
//! ```text
//!  OS Thread (btc-5m)  ─── current_thread runtime
//!    │
//!    ├─ spawn_local: fetch standby metadata (HTTP, non-blocking)
//!    │                                          ↓ result via JoinHandle
//!    └─ 'events select! loop
//!         ├─ Arm 1 (biased): active WS price-change events   ← hot path
//!         ├─ Arm 2: expiry timer
//!         └─ Arm 3: standby JoinHandle ready → subscribe WS
//! ```
//!
//! The HTTP fetch runs as a `spawn_local` task. Its `JoinHandle` is stored and
//! polled as Arm 3 in `select!`. When Arm 1 fires (WS event), the handle is
//! NOT dropped — the task keeps running. On the next iteration Arm 3 picks up
//! the result as soon as the task completes.

use std::pin::Pin;
use std::time::{Duration, Instant};

use futures::{Stream, StreamExt as _};
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::clob::ws::types::response::{BookUpdate, PriceChange};
use polymarket_client_sdk::clob::ws::Client as WsClient;
use polymarket_client_sdk::types::{Decimal, U256};
use tokio::time::sleep_until;
use tracing::{debug, info, warn, Instrument as _};

use crate::market_config::{Market, MarketConfig};
use crate::polymarket_api::fetch_market_with_retry;

// ─── Stream types ─────────────────────────────────────────────────────────────

#[allow(dead_code)]
type BookStream = Pin<Box<dyn Stream<Item = polymarket_client_sdk::Result<BookUpdate>> + Send>>;
type PriceStream = Pin<Box<dyn Stream<Item = polymarket_client_sdk::Result<PriceChange>> + Send>>;

// ─── TokenBook ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct TokenBook {
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
}

impl TokenBook {
    #[inline]
    pub fn mid(&self) -> Option<Decimal> {
        match (self.best_bid, self.best_ask) {
            (Some(b), Some(a)) => Some((b + a) / Decimal::TWO),
            _ => None,
        }
    }

    #[inline]
    pub fn spread(&self) -> Option<Decimal> {
        match (self.best_bid, self.best_ask) {
            (Some(b), Some(a)) if a > b => Some(a - b),
            _ => None,
        }
    }
}

// ─── MarketBook ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct MarketBook {
    pub up: TokenBook,
    pub down: TokenBook,
}

impl MarketBook {
    #[inline]
    pub fn is_ready(&self) -> bool {
        self.up.best_bid.is_some() && self.down.best_bid.is_some()
    }
}

// ─── Strategy ─────────────────────────────────────────────────────────────────

pub trait Strategy: Send + 'static {
    fn on_market_change(&mut self, previous: Option<&Market>, current: &Market) {
        let _ = (previous, current);
    }
    fn on_book_update(&mut self, market: &Market, book: &MarketBook);
}

// ─── MarketWatcher ────────────────────────────────────────────────────────────

pub struct MarketWatcher<S: Strategy> {
    client: reqwest::Client,
    config: MarketConfig,
    ws: WsClient,
    strategy: S,
}

impl<S: Strategy> MarketWatcher<S> {
    fn new(client: reqwest::Client, config: MarketConfig, strategy: S) -> Self {
        Self {
            client,
            config,
            ws: WsClient::default(),
            strategy,
        }
    }

    /// Spawn this watcher on a dedicated OS thread with its own tokio runtime.
    /// `WsClient` is created inside the thread — no `Send` requirement.
    pub fn spawn_thread(
        client: reqwest::Client,
        config: MarketConfig,
        strategy: S,
        span: tracing::Span,
    ) -> std::thread::JoinHandle<()>
    where
        S: Send + 'static,
    {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build watcher runtime");
            let local = tokio::task::LocalSet::new();
            rt.block_on(local.run_until(async move {
                let mut watcher = MarketWatcher::new(client, config, strategy);
                watcher.run().instrument(span).await;
            }));
        })
    }

    pub async fn run(&mut self) {
        info!("watcher starting");

        // ── Phase 0: fetch first market ───────────────────────────────────────
        let t_boot = Instant::now();
        let first_slug = self.config.slug_for_end_ts(
            self.config
                .round_down(polymarket_client_sdk::types::Utc::now().timestamp()),
        );
        info!(slug = %first_slug, "booting — fetching first market");

        let first_market = loop {
            match fetch_market_with_retry(&self.client, &first_slug).await {
                Some(m) => break m,
                None => {
                    warn!("could not fetch first market, retrying in 1s");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        };
        info!(
            boot_ms = format_args!("{:.1}", t_boot.elapsed().as_secs_f64() * 1000.0),
            market = %first_market.slug,
            "first market fetched"
        );

        // ── Phase 1: cold-subscribe first market ──────────────────────────────
        warn!(market = %first_market.slug, "no standby ready — cold subscribe (~1 RTT)");
        let t_sub = Instant::now();
        let first_stream = self.subscribe_market(&first_market).await;
        info!(
            subscribe_ms = format_args!("{:.1}", t_sub.elapsed().as_secs_f64() * 1000.0),
            "cold subscribe latency"
        );

        let mut active_market: Option<Market> = Some(first_market);
        let mut active_stream: Option<PriceStream> = first_stream;
        let mut active_book = MarketBook::default();

        let mut standby_market: Option<Market> = None;
        let mut standby_stream: Option<PriceStream> = None;
        let mut standby_book = MarketBook::default();
        // JoinHandle for the concurrent standby metadata fetch (spawn_local task)
        let mut standby_fetch: Option<tokio::task::JoinHandle<Option<Market>>> = None;

        self.strategy
            .on_market_change(None, active_market.as_ref().unwrap());

        let mut t_rotation_end: Option<Instant> = Some(Instant::now());
        let mut rotation_was_promotion = false;

        // ── Main loop ─────────────────────────────────────────────────────────
        loop {
            // ── Rotation ─────────────────────────────────────────────────────
            let should_rotate = active_market
                .as_ref()
                .map(|m| m.is_expired())
                .unwrap_or(true);

            if should_rotate {
                let t_rotate = Instant::now();
                let prev = active_market.take();

                if let Some(ref p) = prev {
                    self.unsubscribe_market(p);
                }

                // Abort any in-flight standby fetch — it fetched for the wrong slot
                if let Some(h) = standby_fetch.take() {
                    h.abort();
                }

                // Promote standby if available and non-expired
                let can_promote = standby_market
                    .as_ref()
                    .map(|sm| !sm.is_expired())
                    .unwrap_or(false);

                if can_promote {
                    let sm = standby_market.take().unwrap();
                    info!(market = %sm.slug, "🔀 promoting standby → active");
                    active_stream = standby_stream.take();
                    active_book = std::mem::take(&mut standby_book);
                    active_market = Some(sm);
                    rotation_was_promotion = true;
                } else {
                    // No standby ready — fetch & cold-subscribe
                    let slug = prev
                        .as_ref()
                        .map(|p| self.config.slug_for_end_ts(p.end_time))
                        .unwrap_or_else(|| {
                            let ts = polymarket_client_sdk::types::Utc::now().timestamp();
                            self.config.slug_for_end_ts(self.config.round_down(ts))
                        });
                    warn!(market = %slug, "no standby ready — cold subscribe (~1 RTT)");
                    let next = loop {
                        match fetch_market_with_retry(&self.client, &slug).await {
                            Some(m) => break m,
                            None => tokio::time::sleep(Duration::from_millis(200)).await,
                        }
                    };
                    let t_s = Instant::now();
                    active_stream = self.subscribe_market(&next).await;
                    info!(
                        subscribe_ms = format_args!("{:.1}", t_s.elapsed().as_secs_f64() * 1000.0),
                        "cold subscribe latency"
                    );
                    active_book = MarketBook::default();
                    self.strategy.on_market_change(prev.as_ref(), &next);
                    active_market = Some(next);
                    rotation_was_promotion = false;
                }

                // Reset standby state
                standby_market = None;
                standby_stream = None;
                standby_book = MarketBook::default();

                info!(
                    rotation_ms = format_args!("{:.1}", t_rotate.elapsed().as_secs_f64() * 1000.0),
                    "rotation complete"
                );
                t_rotation_end = Some(Instant::now());
            }

            // ── Event loop ───────────────────────────────────────────────────
            let stream = match active_stream.as_mut() {
                Some(s) => s,
                None => {
                    warn!("no active stream — retrying in 100ms");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };

            let deadline = {
                let secs = active_market
                    .as_ref()
                    .map(|m| m.seconds_until_expiry().max(0) as u64)
                    .unwrap_or(300);
                tokio::time::Instant::now() + Duration::from_secs(secs)
            };

            'events: loop {
                // ── Arm standby fetch if not started ─────────────────────────
                // Spawn a local task to fetch next-market metadata concurrently.
                // The JoinHandle is stored and polled as Arm 3 in select! below.
                // If Arm 1 fires (WS event), the handle is NOT dropped — the
                // HTTP fetch task keeps running on the same runtime.
                if standby_fetch.is_none() && standby_market.is_none() {
                    if let Some(ref m) = active_market {
                        let next_slug = self.config.slug_for_end_ts(m.end_time);
                        let client2 = self.client.clone();
                        info!(slug = %next_slug, "🔍 standby fetch started");
                        standby_fetch = Some(tokio::task::spawn_local(async move {
                            fetch_market_with_retry(&client2, &next_slug).await
                        }));
                    }
                }

                // ── Drain standby stream to warm its book ────────────────────
                if let (Some(ref mut sb_s), Some(ref sb_m)) = (&mut standby_stream, &standby_market)
                {
                    while let Ok(Some(Ok(ev))) =
                        tokio::time::timeout(Duration::from_micros(1), sb_s.next()).await
                    {
                        apply_price_change(
                            &ev,
                            &mut standby_book,
                            &sb_m.up_token,
                            &sb_m.down_token,
                        );
                    }
                }

                // ── Three-arm select! ─────────────────────────────────────────
                tokio::select! {
                    biased;

                    // Arm 1: active WS events — highest priority
                    maybe = stream.next() => match maybe {
                        None => {
                            warn!("active stream closed — re-subscribing");
                            active_stream = None;
                            break 'events;
                        }
                        Some(Err(e)) => {
                            warn!(error = %e, "WS stream error");
                            active_stream = None;
                            break 'events;
                        }
                        Some(Ok(event)) => {
                            let t_event = Instant::now();
                            if let Some(t_rot) = t_rotation_end.take() {
                                info!(
                                    first_event_ms = format_args!("{:.1}", t_rot.elapsed().as_secs_f64() * 1000.0),
                                    via = if rotation_was_promotion { "standby" } else { "cold" },
                                    "⏱ first event after rotation"
                                );
                            }
                            if let Some(ref m) = active_market {
                                apply_price_change(&event, &mut active_book, &m.up_token, &m.down_token);
                                let t_strat = Instant::now();
                                self.strategy.on_book_update(m, &active_book);
                                debug!(
                                    strategy_ms = format_args!("{:.3}", t_strat.elapsed().as_secs_f64() * 1000.0),
                                    total_ms = format_args!("{:.3}", t_event.elapsed().as_secs_f64() * 1000.0),
                                    "event → strategy"
                                );
                            }
                        }
                    },

                    // Arm 2: expiry timer
                    _ = sleep_until(deadline) => {
                        info!(
                            market = active_market.as_ref().map(|m| m.slug.as_str()).unwrap_or("?"),
                            "⏰ market expired"
                        );
                        break 'events;
                    },

                    // Arm 3: standby metadata fetch complete → subscribe WS
                    //
                    // Uses an async block instead of a guarded arm to avoid
                    // a tokio::select! gotcha: even when `if guard` is false,
                    // the arm *expression* is still evaluated before the guard
                    // is checked, which would call `.unwrap()` on None.
                    //
                    // The async block is safe: it returns `pending()` when
                    // there is no in-flight fetch, effectively disabling Arm 3.
                    // When Arm 1 fires and drops this block, the JoinHandle in
                    // `standby_fetch` is NOT dropped — the task keeps running.
                    // The next iteration creates a fresh block that polls the
                    // same handle and resolves immediately once the task is done.
                    result = async {
                        match standby_fetch.as_mut() {
                            Some(h) => h.await.ok().unwrap_or(None),
                            None => std::future::pending().await,
                        }
                    } => {
                        standby_fetch = None;
                        match result {
                            Some(market) => {
                                info!(market = %market.slug, "🏹 standby metadata ready, subscribing WS");
                                let t = Instant::now();
                                standby_stream = self.subscribe_market(&market).await;
                                info!(
                                    subscribe_ms = format_args!("{:.1}", t.elapsed().as_secs_f64() * 1000.0),
                                    "standby WS subscribed"
                                );
                                standby_market = Some(market);
                            }
                            None => warn!("standby metadata fetch returned None — will retry"),
                        }
                    },
                }
            }
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    async fn subscribe_market(&self, market: &Market) -> Option<PriceStream> {
        let (up, down) = match (
            parse_token_id(&market.up_token),
            parse_token_id(&market.down_token),
        ) {
            (Some(u), Some(d)) => (u, d),
            _ => {
                warn!(slug = %market.slug, "failed to parse token IDs");
                return None;
            }
        };

        info!(
            market = %market.slug,
            up = %market.up_token,
            down = %market.down_token,
            "subscribing to price-change stream"
        );

        match self.ws.subscribe_prices(vec![up, down]) {
            Ok(s) => Some(Box::pin(s) as PriceStream),
            Err(e) => {
                warn!(error = %e, "subscribe_prices failed");
                None
            }
        }
    }

    fn unsubscribe_market(&self, market: &Market) {
        if let (Some(up), Some(down)) = (
            parse_token_id(&market.up_token),
            parse_token_id(&market.down_token),
        ) {
            let _ = self.ws.unsubscribe_prices(&[up, down]);
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_token_id(decimal_str: &str) -> Option<U256> {
    use std::str::FromStr;
    U256::from_str(decimal_str).ok()
}

fn apply_price_change(
    event: &PriceChange,
    book: &mut MarketBook,
    up_token: &str,
    down_token: &str,
) {
    for entry in &event.price_changes {
        let id_str = entry.asset_id.to_string();
        let leg = if id_str == up_token {
            &mut book.up
        } else if id_str == down_token {
            &mut book.down
        } else {
            continue;
        };
        match entry.side {
            Side::Buy => leg.best_bid = Some(entry.price),
            Side::Sell => leg.best_ask = Some(entry.price),
            _ => {}
        }
    }
}
