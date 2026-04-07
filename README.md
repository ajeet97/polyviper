# PolyViper: Polymarket HFT Engine

PolyViper is an institutional-grade, low-latency automated trading engine built in Rust for [Polymarket](https://polymarket.com/). It is designed to run highly parallelized, latency-sensitive arbitrage and market-making strategies on Polymarket's CLOB (Central Limit Order Book).

The bot currently implements **Latency Arbitrage** strategies, front-running Polymarket's static odds compared to centralized exchanges (Binance, Coinbase) live Spot orderbooks during periods of high volatility.

## Features

- **Blazing Fast Event-Driven Architecture:** Uses `tokio::sync::watch` to bypass traditional blocking loops, ensuring the Strategy Engine reacts instantly to the newest ticker ticks without queuing latency.
- **Batched Execution:** Centralised Order Management System (`oms`) uses proxy wallets (e.g. Gnosis Safe) and batches Polymarket CLOB requests, respecting the strict 5/req s limits.
- **Multi-Feed Ingestion:** Simultaneously tracks `wss://stream.binance.com` for tick-level crypto prices, Deribit's `DVOL` for volatility tracking, and Polymarket's live WebSockets.
- **Institutional Risk Management:** 
  - **Dynamic Position Sizing:** Fractional Kelly Criterion sizing mathematically capped at 8% of the total rolling portfolio value.
  - **Tiered Drawdowns:** Implements a strict -20% daily stop-loss and a fatal -40% kill switch to protect against tail-risk software errors or black swan events.
  - **Oracle De-Peg Safety:** Monitors price divergence across multiple centralized exchanges to lock out trading if external feeds are manipulated or delayed.
- **Crash Bootstrapper:** On startup, the engine performs a "Ghost Test" (`$0.10` test buy) and aggressively runs a `cancel_all_orders()` API cascade to clear toxic flow leftover from any unexpected crashes.

## Prerequisites

- [Rust & Cargo](https://rustup.rs/) (edition 2021+)
- A funded Polymarket Wallet (Polygon USDC)
- *(Optional but Recommended)* Dedicated low-latency infrastructure (VPS in AWS `us-east-1` or `eu-west-1`) for competitive API round-trip times to Polygon networks.

## Configuration

We use an `.env` file or environment variables to configure execution contexts safely:

```env
# Required
PRIVATE_KEY=0x...
POLYGON_RPC_URL=https://polygon-rpc.com

# Bot Toggles
SIMULATE=true # Bypasses CLOB API signatures and posts. Set false to trade live capital.
SIGNATURE_TYPE=2 # 0 = Basic EOA, 2 = Gnosis Safe (Standard for Polymarket Web Wallets)

# API (If applicable for builder endpoints)
POLYMARKET_API_KEY="..."
POLYMARKET_API_SECRET="..."
POLYMARKET_API_PASSPHRASE="..."
```

## Running the Engine

1. Clone and build the project for absolute maximum optimization:
```bash
cargo build --release
```

2. Run the executable. During your first few days, absolutely run with paper trading/simulation mode active:
```bash
SIMULATE=true ./target/release/polyviper
```

## Architecture

```text
src/
├── core/
│   ├── bus.rs        # Watch channels pushing non-blocking state to strategies
│   └── models.rs     # Canonical structs across execution loops
├── feeders/
│   ├── binance.rs    # Connects to `bookTicker` for immediate real-world pricing
│   ├── deribit.rs    # Deribit Volatility Index polling
│   └── polymarket.rs # Native Polymarket CLOB streaming
├── oms/
│   ├── bootstrapper.rs # State reset routines 
│   ├── handler.rs      # Native Signature and EIP-712 builder integration
│   └── risk.rs         # Kelly caps, stop losses, routing validation
└── strategies/
    ├── latency_arb.rs  # The 5-min & 15-min divergence latency arb routine
    └── cex_arb.rs      # The resting Market Maker template
```

## Strategy Tuning

If you are hunting Maker rebates rather than Taker execution, switch the strategy initialization inside `src/main.rs`:

```rust
// In main.rs
use crate::strategies::latency_arb::LatencyArbStrategy;
let mut strategy = LatencyArbStrategy::new("ethusdt", 0.035); // 3.5% divergence
```

## Legal Warning

Trading algorithmically involves the significant risk of capital loss. Do not trade with money you cannot afford to lose. Furthermore, Polymarket is heavily regulated and restricted entirely in the United States and several other jurisdictions. **Do not attempt to bypass these restrictions to use this software.**

Disclaimer: This project is for educational purposes only. Past performance derived from these strategies does not guarantee future results.
