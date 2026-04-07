# PolyViper Architecture & Design Decisions

This document outlines the architectural plan for the PolyViper HFT Bot and the rationale behind critical design changes made during its transition to a high-frequency, event-driven trading engine.

## 1. Event-Driven Architecture (EDA) & Concurrency
**Decision:** Replace the single-threaded, blocking `MarketWatcher` with an asynchronous pipeline powered by `tokio::sync::watch`.
- **Reasoning:** In the original codebase, the main thread was responsible for parsing WebSockets, evaluating math, and sending HTTP requests synchronously. This created a massive chokepoint. In HFT, latency is everything.
- **Implementation:** The `WatchBus` acts as the central nervous system. Independent "Feeder" tasks ingest WebSockets (Binance, Polymarket, Deribit) and push updates into the `watch` channels. The central `Strategy` loop reads from this bus instantly. `tokio::sync::watch` ensures the strategy only ever sees the absolute newest price, without getting bogged down by a queue of stale ticks.

## 2. Decoupled Ingestion Feeders
**Decision:** Break data ingestion into separate `BinanceFeeder`, `PolymarketFeeder`, and `DeribitFeeder` modules.
- **Reasoning:** Polymarket's 5-minute and 15-minute crypto contracts trade highly correlated to centralized exchanges. The fundamental "Latency Arbitrage" edge relies on Binance moving *first*, and Polymarket lagging behind by ~2.7 seconds.
- **Implementation:** By isolating Binance WS logic into `feeders/binance.rs`, the bot maintains a persistent, microsecond-latency connection to Binance's `"bookTicker"`, providing the "True Probability" baseline *before* the Polymarket API registers the move.

## 3. Order Management System (OMS) & Proxy Wallets
**Decision:** Centralize all order execution into an `OmsHandler` via an internal `MPSC` Order Intent queue.
- **Reasoning:** Polymarket relies heavily on Gnosis Safe smart contract wallets ("proxy wallets"), where the funding address holds capital but an Externally Owned Account (EOA) signs the transactions. Furthermore, API rate limits (5 req/sec sustained) require bottlenecking outbound requests safely.
- **Implementation:** The `OmsHandler` accepts abstract `OrderIntent` objects. It is initialized explicitly with `SignatureType::GnosisSafe` and a `funder_address`. The handler signs EIP-712 payloads locally utilizing an injected `LocalSigner`, mitigating the need for on-chain gas operations for order placement.

## 4. Bootstrapper ("Clean Slate" & Ghost Testing)
**Decision:** Execute a rigorous pre-flight checklist (`src/oms/bootstrapper.rs`) on every startup.
- **Reasoning:** When a bot crashes or loses network connectivity, it leaves "resting" maker orders sitting on the orderbook ("toxic flow"). These stale orders will be mercilessly exploited by competitors.
- **Implementation:** Before the strategy loop is armed, the `Bootstrapper` performs two actions:
  1. **Ghost Test:** Posts a $0.10 dummy limit order and instantly cancels it to confirm the L2 network and API credentials are fully responsive.
  2. **Clean Slate:** Triggers an unconditional `cancel_all_orders()` API call against the Polymarket CLOB to wipe any residual risk from previous sessions.

## 5. Strategic Focus: Latency Arbitrage
**Decision:** Pivot from general mathematical arbitrage to a directional "Latency Arbitrage" strategy (`src/strategies/latency_arb.rs`).
- **Reasoning:** Based on the mechanics reverse-engineered from the highly successful `0x8dxd` bot (which turned $313 into $2.3M), the most consistently profitable edge is front-running Polymarket's static odds compared to Binance's Live Spot price.
- **Implementation:** The `LatencyArbStrategy` calculates the divergence between Binance BBO and the Polymarket Orderbook. If the difference hits our dynamic threshold (e.g., `3.5%`), the bot immediately buys the under-priced Polymarket contract as a *Taker* before the rest of the market reprices it.

## 6. Institutional Risk Management
**Decision:** Implement strict, tiered drawdown protections mirroring institutional desks rather than standard retail static limits.
- **Reasoning:** A strategy with a 98% win rate can still be liquidated by black swan events or API bugs if position sizing is naive. Emotional sizing or "doubling down" must be mathematically prevented.
- **Implementation:** The `RiskManager` intercepts every outbound order from the Strategy and applies three laws:
  1. **Kelly Position Cap:** No single position may exceed **8%** of the total defined portfolio value.
  2. **Daily Stop Loss:** If the total PnL from the start of the 24-hour cycle hits **-20%**, trading is halted until human intervention.
  3. **Universal Killswitch:** If the portfolio breaches **-40%** from initial capital, the bot fatally exits to prevent total devastation. 
  4. **Oracle De-Peg Monitor:** Prevents entering trades if a sanity check (e.g., Binance vs. Coinbase prices) shows extreme deviation, indicating a faulty data provider rather than a valid market opportunity.
