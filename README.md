# polyviper

A high-performance Polymarket market watcher written in Rust. Watches real-time price feeds on Polymarket's up/down binary markets via WebSocket, detects pricing anomalies, and simulates execution opportunities as they arise.

## How it works

Polymarket's up/down markets are time-bounded binary contracts — e.g. `btc-updown-5m-<end_ts>` expires in 5 minutes, and a fresh one opens immediately after. UP + DOWN outcomes always settle to 1.0 combined, so if the best-ask sum of both legs falls below 1.0, there's a risk-free edge.

polyviper watches these markets continuously across rotations with **zero gap** — the next market's WebSocket subscription is opened concurrently while the current one is still live.

## Architecture

Each market config (e.g. BTC-5m, ETH-15m) runs on its **own OS thread** with a dedicated single-threaded tokio runtime. Everything — API fetches, WebSocket streams, book processing, strategy callbacks — is handled concurrently on that one thread via async I/O.

```
main
 ├── OS Thread: btc-5m          ├── OS Thread: eth-5m
 │   current_thread runtime     │   current_thread runtime
 │                              │  
 │   [boot] fetch metadata      │   [boot] fetch metadata
 │   [boot] cold WS subscribe   │   [boot] cold WS subscribe
 │                              │
 │   loop {                     │   loop {
 │     select! {                │     select! {
 │       Arm 1: WS events  ◀── │       Arm 1: WS events
 │       Arm 2: expiry ⏰      │       Arm 2: expiry ⏰
 │       Arm 3: standby  🔍   │       Arm 3: standby 🔍
 │     }                        │     }
 │   }                          │   }
 └──────────────────────────    └──────────────────────────
```

**Arm 3** runs a `spawn_local` task that fetches the next market's metadata from the Gamma API while Arm 1 processes live events. When the metadata arrives, polyviper immediately subscribes to the standby WS stream. On rotation, the pre-armed stream is promoted → active in **~0ms** with **< 15ms** to first event.

## Market lifecycle

```
t=0s   boot: fetch btc-updown-5m-N, cold WS subscribe
t=0.4s first event arrives
t=0.5s standby fetch started (btc-updown-5m-N+300)
t=0.7s standby metadata ready, WS subscribed (standby armed ✓)

        ... 5 minutes of live events on market N ...

t=300s ⏰ market N expired
       🔀 promote standby → active            (swap, ~0ms)
       ⏱  first event via standby: ~10ms
       🔍 start new standby fetch for N+600
```

## Running

```bash
cargo run
```

Configure which markets to watch in `src/main.rs`:

```rust
let configs = vec![
    MarketConfig::btc_5m(),          // BTC up/down 5-minute
    MarketConfig::eth_5m(),          // ETH up/down 5-minute
    MarketConfig::btc_15m(),         // BTC up/down 15-minute
    MarketConfig::new("sol", 5),     // SOL up/down 5-minute (any asset)
];
```

Each entry spins up an independent watcher thread. Adding a new market is one line.

Log verbosity is controlled via `RUST_LOG`:

```bash
RUST_LOG=polyviper=debug cargo run    # verbose
RUST_LOG=polyviper=info  cargo run    # default
```

## Key log events

| Symbol | Meaning |
|--------|---------|
| 🚀 | First market on boot |
| 🔍 | Standby metadata fetch started (background) |
| 🏹 | Standby metadata ready, opening WS subscription |
| 🔀 | Market rotation: standby promoted to active |
| ⏰ | Active market expired |
| ⚡ | Pricing anomaly detected (SIMULATE EXECUTE) |
| ⏱ | Time-to-first-event after rotation (latency audit) |

## Implementing a strategy

Implement the `Strategy` trait in `src/main.rs`:

```rust
pub trait Strategy: Send + 'static {
    fn on_market_change(&mut self, previous: Option<&Market>, current: &Market);
    fn on_book_update(&mut self, market: &Market, book: &MarketBook);
}
```

`on_book_update` is called on every WebSocket price-change event. `MarketBook` exposes `best_bid` and `best_ask` for both the UP and DOWN legs.

## Project structure

```
src/
├── main.rs             # Entry point, strategy implementation, market config
├── market_watcher.rs   # Core event loop, WS management, standby pre-arming
├── market_config.rs    # MarketConfig + Market types, slug arithmetic
└── polymarket_api.rs   # Gamma API client (fetch market by slug)
```

