//! MarketWatcher — zero-gap market rotation with pre-armed WS subscriptions.
//!
//! # Design
//!
//! The key latency problem: when market N expires we must start consuming market
//! N+1 *immediately*, with no round-trip to subscribe, no buffer miss, no gap.
//!
//! Solution: while market N is live we **pre-arm** the subscription for market
//! N+1 (open the WS stream, populate its initial book) so that at the exact
//! moment of expiry we just swap the pinned stream pointer — zero extra I/O.
//!
//! ```text
//!  timeline ──────────────────────────────────────────────────────────────────▶
//!
//!  market N   [==========================================]
//!                     ↑ standby pre-arm starts here
//!                     market N+1 [============================
//!
//!  expiry     ──────────────────────────────────────────▶ swap streams (≈0ms)
//! ```
//!
//! ## Why `subscribe_prices` instead of `subscribe_orderbook`
//!
//! | | `subscribe_orderbook` (book events) | `subscribe_prices` (price_change events) |
//! |---|---|---|
//! | Fires when | Subscribe + every trade | Best bid/ask changes |
//! | Carries | Full depth snapshot | `best_bid`/`best_ask` + changed price |
//! | For best bid/ask | Must read `bids[0]`/`asks[0]` | Explicit fields |
//!
//! `price_change` fires specifically when the best bid or ask *changes* and
//! explicitly carries the new best values — ideal for top-of-book tracking.
//!
//! To switch to full orderbook depth later:
//! 1. Change `PriceStream` to `BookStream` (see `BookStream` type alias below)
//! 2. Change `subscribe_market` to call `ws.subscribe_prices` → `ws.subscribe_orderbook`
//! 3. Change `apply_event` to call `apply_book_update` instead of `apply_price_change`

use std::pin::Pin;
use std::time::{Duration, Instant};

use futures::{Stream, StreamExt as _};
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::clob::ws::types::response::{BookUpdate, PriceChange};
use polymarket_client_sdk::clob::ws::Client as WsClient;
use polymarket_client_sdk::types::{Decimal, U256};
use tokio::time::sleep_until;
use tracing::{debug, info, warn};

use crate::market_finder::{Market, MarketBuffer};

// ─── Stream type ─────────────────────────────────────────────────────────────
//
// Using `price_change` events: lightweight, fires on best bid/ask changes,
// and explicitly carries updated best_bid / best_ask per token.
//
// `BookStream` is kept as a type alias so the switcher comment above is concrete.
#[allow(dead_code)]
type BookStream = Pin<Box<dyn Stream<Item = polymarket_client_sdk::Result<BookUpdate>> + Send>>;
type PriceStream = Pin<Box<dyn Stream<Item = polymarket_client_sdk::Result<PriceChange>> + Send>>;

// ─── Price level ──────────────────────────────────────────────────────────────

/// Best bid & ask for one binary-market token leg.
#[derive(Debug, Clone, Default)]
pub struct TokenBook {
    /// Highest buy order price. `None` until the first WS event arrives.
    pub best_bid: Option<Decimal>,
    /// Lowest sell order price. `None` until the first WS event arrives.
    pub best_ask: Option<Decimal>,
}

impl TokenBook {
    /// Mid-price = (bid + ask) / 2, or `None` if either side is missing.
    #[inline]
    pub fn mid(&self) -> Option<Decimal> {
        match (self.best_bid, self.best_ask) {
            (Some(b), Some(a)) => Some((b + a) / Decimal::TWO),
            _ => None,
        }
    }

    /// Spread = ask − bid, or `None` if either side is missing.
    #[inline]
    pub fn spread(&self) -> Option<Decimal> {
        match (self.best_bid, self.best_ask) {
            (Some(b), Some(a)) if a > b => Some(a - b),
            _ => None,
        }
    }
}

// ─── Combined live view ───────────────────────────────────────────────────────

/// Live best-bid/ask snapshot for both legs of a 5-minute binary market.
#[derive(Debug, Clone, Default)]
pub struct MarketBook {
    /// UP token (pays out if price ends above threshold).
    pub up: TokenBook,
    /// DOWN token (pays out if price ends at or below threshold).
    pub down: TokenBook,
}

impl MarketBook {
    /// `true` once at least one price event has arrived for *each* leg.
    #[inline]
    pub fn is_ready(&self) -> bool {
        self.up.best_bid.is_some() && self.down.best_bid.is_some()
    }
}

// ─── Strategy ─────────────────────────────────────────────────────────────────

/// Implement this to plug in any trading or monitoring strategy.
pub trait Strategy: Send + 'static {
    /// Called exactly once each time the active market rotates.
    ///
    /// `previous` is `None` on the very first market.
    fn on_market_change(&mut self, previous: Option<&Market>, current: &Market) {
        let _ = (previous, current);
    }

    /// Called on every CLOB price-change event for the **active** market.
    ///
    /// Hot path — keep it fast. `book` always reflects the latest best bid/ask.
    /// Use [`MarketBook::is_ready`] to skip until both legs have initialised.
    fn on_book_update(&mut self, market: &Market, book: &MarketBook);
}

// ─── MarketWatcher ────────────────────────────────────────────────────────────

pub struct MarketWatcher<S: Strategy> {
    buf: MarketBuffer,
    ws: WsClient,
    strategy: S,
}

impl<S: Strategy> MarketWatcher<S> {
    /// Create a new watcher backed by `buf`.
    ///
    /// Start the `MarketFinder` background task (`.spawn()`) before calling
    /// `run()`, otherwise the watcher spins until the buffer has data.
    pub fn new(buf: MarketBuffer, strategy: S) -> Self {
        Self {
            buf,
            ws: WsClient::default(),
            strategy,
        }
    }

    /// Start the event loop — runs forever.
    pub async fn run(&mut self) {
        info!("watcher starting");

        // ── Phase 0: wait for first market ───────────────────────────────────
        let t_boot = Instant::now();
        loop {
            if self.buf.current().is_some() {
                break;
            }
            debug!("buffer empty, waiting...");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        info!(
            boot_ms = format_args!("{:.1}", t_boot.elapsed().as_secs_f64() * 1000.0),
            "first market available"
        );

        let mut active_market: Option<Market> = None;
        let mut active_stream: Option<PriceStream> = None;
        let mut active_book = MarketBook::default();

        // Standby = pre-armed subscription for market N+1.
        // Populated synchronously (WS subscription is fast once the connection
        // is up — it's just a protocol message, no new TCP handshake).
        let mut standby_market: Option<Market> = None;
        let mut standby_stream: Option<PriceStream> = None;
        let mut standby_book = MarketBook::default();

        // Tracks when the last rotation completed so we can measure
        // time-to-first-event: the real latency saved by standby pre-arming.
        let mut t_rotation_end: Option<Instant> = None;
        let mut rotation_was_promotion = false;

        loop {
            // ── Step 1: rotate to next market if active has expired ───────────
            let should_rotate = active_market
                .as_ref()
                .map(|m| m.is_expired())
                .unwrap_or(true);

            if should_rotate {
                let t_rotate = Instant::now();

                let prev_market = active_market.take(); // grab before overwrite

                // Spin until buffer has a non-expired market
                let next = loop {
                    match self.buf.current() {
                        Some(m) => break m,
                        None => {
                            warn!("buffer empty on rotation — waiting 50ms");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                };

                // Unsubscribe previous legs (best-effort)
                if let Some(ref prev) = prev_market {
                    self.unsubscribe_market(prev);
                }

                // Promote standby if it matches the next market (zero-latency)
                let can_promote = standby_market
                    .as_ref()
                    .map(|sm| sm.slug == next.slug)
                    .unwrap_or(false);

                if can_promote {
                    info!(market = %next.slug, "🔀 promoting standby → active");
                    active_market = standby_market.take();
                    active_stream = standby_stream.take();
                    // Standby book is already warmed — carry it over as-is
                    active_book = std::mem::take(&mut standby_book);
                    rotation_was_promotion = true;
                } else {
                    // Cold subscribe — first market only, or if standby wasn't ready
                    warn!(market = %next.slug, "no standby ready — cold subscribe (~1 RTT)");
                    let t_sub = Instant::now();
                    active_stream = self.subscribe_market(&next).await;
                    info!(
                        subscribe_ms =
                            format_args!("{:.1}", t_sub.elapsed().as_secs_f64() * 1000.0),
                        "cold subscribe latency"
                    );
                    active_market = Some(next);
                    active_book = MarketBook::default();
                    rotation_was_promotion = false;
                }

                // Notify strategy
                if let Some(ref m) = active_market {
                    self.strategy.on_market_change(prev_market.as_ref(), m);
                }

                // Reset standby — will be re-armed inside the event loop below
                standby_market = None;
                standby_stream = None;
                standby_book = MarketBook::default();

                info!(
                    rotation_ms = format_args!("{:.1}", t_rotate.elapsed().as_secs_f64() * 1000.0),
                    "rotation complete"
                );
                // Start the clock — we'll log how long until the first event arrives.
                t_rotation_end = Some(Instant::now());
            }

            // ── Step 2: event loop until expiry or stream error ───────────────
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
                // ── Pre-arm standby if not yet done ───────────────────────────
                // Checked on every iteration so we retry until the buffer has
                // the next market available (it may not be there immediately
                // after rotation). peek_next() is O(1) — just a Mutex + deque
                // index — so this is essentially free on the hot path.
                if standby_stream.is_none() {
                    if let Some(next_market) = self.buf.peek_next() {
                        info!(market = %next_market.slug, "🏹 pre-arming standby");
                        let t_prearm = Instant::now();
                        standby_stream = self.subscribe_market(&next_market).await;
                        info!(
                            subscribe_ms =
                                format_args!("{:.1}", t_prearm.elapsed().as_secs_f64() * 1000.0),
                            "standby subscribe latency"
                        );
                        standby_market = Some(next_market);
                    }
                    // else: buffer doesn't have next market yet — will retry next iteration
                }

                // Non-blockingly drain standby stream to warm its book.
                // Uses the standby market's token IDs for correct leg assignment.
                if let (Some(ref mut sb_stream), Some(ref sb_market)) =
                    (&mut standby_stream, &standby_market)
                {
                    while let Ok(Some(result)) = tokio::time::timeout(
                        Duration::from_micros(1), // non-blocking peek
                        sb_stream.next(),
                    )
                    .await
                    {
                        if let Ok(event) = result {
                            apply_price_change(
                                &event,
                                &mut standby_book,
                                &sb_market.up_token,
                                &sb_market.down_token,
                            );
                        }
                    }
                }

                tokio::select! {
                    biased; // market events take priority over the expiry timer

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
                            // Log time-to-first-event once per rotation — this
                            // is the real measure of what standby pre-arming saves.
                            if let Some(t_rot) = t_rotation_end.take() {
                                let first_event_ms = t_rot.elapsed().as_secs_f64() * 1000.0;
                                info!(
                                    first_event_ms = format_args!("{:.1}", first_event_ms),
                                    via = if rotation_was_promotion { "standby" } else { "cold" },
                                    "⏱ first event after rotation"
                                );
                            }
                            if let Some(ref m) = active_market {
                                apply_price_change(
                                    &event,
                                    &mut active_book,
                                    &m.up_token,
                                    &m.down_token,
                                );
                                let t_strat = Instant::now();
                                self.strategy.on_book_update(m, &active_book);
                                debug!(strategy_ms = format_args!("{:.3}", t_strat.elapsed().as_secs_f64() * 1000.0), "strategy.on_book_update");
                            }
                            debug!(total_ms = format_args!("{:.3}", t_event.elapsed().as_secs_f64() * 1000.0), "event → strategy total");
                        }
                    },

                    _ = sleep_until(deadline) => {
                        info!(
                            market = active_market.as_ref().map(|m| m.slug.as_str()).unwrap_or("?"),
                            "⏰ market expired"
                        );
                        // Outer loop will detect is_expired() and rotate
                        break 'events;
                    }
                }
            }
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Subscribe to price-change events for both legs of a market.
    async fn subscribe_market(&self, market: &Market) -> Option<PriceStream> {
        let (up, down) = match (
            parse_token_id(&market.up_token),
            parse_token_id(&market.down_token),
        ) {
            (Some(u), Some(d)) => (u, d),
            _ => {
                warn!(market = %market.slug, "failed to parse token IDs");
                return None;
            }
        };

        info!(
            market = %market.slug,
            up = &market.up_token[..8.min(market.up_token.len())],
            down = &market.down_token[..8.min(market.down_token.len())],
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

    /// Unsubscribe from price-change events for a market's legs (best-effort).
    fn unsubscribe_market(&self, market: &Market) {
        let (u, d) = match (
            parse_token_id(&market.up_token),
            parse_token_id(&market.down_token),
        ) {
            (Some(u), Some(d)) => (u, d),
            _ => return,
        };
        if let Err(e) = self.ws.unsubscribe_prices(&[u, d]) {
            warn!(error = %e, "unsubscribe_prices failed (non-fatal)");
        }
    }
}

// ─── Apply a price_change event to a MarketBook ───────────────────────────────

/// Update `book` from a CLOB `price_change` event.
///
/// # Leg identification
/// Uses string comparison of decimal token IDs (`entry.asset_id.to_string()`)
/// against the market's stored `up_token` / `down_token` strings, avoiding any
/// `U256::from_str` ↔ serde deserialization representation mismatches.
///
/// # Update priority
/// 1. `best_bid` / `best_ask` on the entry — provided by the server when
///    `custom_feature_enabled` is set; most accurate (post-change snapshot).
/// 2. `price` + `side` fallback — `price_change` fires when the best bid/ask
///    changes; `price` is the new best value for that `side`.
fn apply_price_change(
    event: &PriceChange,
    book: &mut MarketBook,
    up_token: &str,
    down_token: &str,
) {
    for entry in &event.price_changes {
        // String comparison avoids U256 representation issues
        let asset_str = entry.asset_id.to_string();

        let leg = if asset_str == up_token {
            &mut book.up
        } else if asset_str == down_token {
            &mut book.down
        } else {
            debug!(
                asset = &asset_str[..8.min(asset_str.len())],
                "price_change for unknown asset — skipping"
            );
            continue;
        };

        // Priority 1: explicit best_bid / best_ask from the server
        let mut updated = false;
        if let Some(bb) = entry.best_bid {
            leg.best_bid = Some(bb);
            updated = true;
        }
        if let Some(ba) = entry.best_ask {
            leg.best_ask = Some(ba);
            updated = true;
        }

        // Priority 2: infer from price + side
        // price_change fires when best bid/ask changes; `price` IS the new best
        if !updated {
            match entry.side {
                Side::Buy => leg.best_bid = Some(entry.price),
                Side::Sell => leg.best_ask = Some(entry.price),
                _ => {} // unknown side variant — ignore
            }
        }
    }
}

// ─── Helper ───────────────────────────────────────────────────────────────────

/// Parse a decimal token ID string to `U256` (needed for WS subscribe/unsubscribe).
/// Data matching uses string comparison — this is only for API call arguments.
fn parse_token_id(id: &str) -> Option<U256> {
    match id.parse::<U256>() {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(id, error = %e, "cannot parse token id");
            None
        }
    }
}
