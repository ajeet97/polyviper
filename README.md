# ETH 5m Arbitrage Bot — Polymarket

Watches the active ETH 5-minute up/down market on Polymarket and places
arbitrage orders whenever `YES ask + NO ask < $1.00`.

---

## Strategy

On Polymarket binary markets:
```
1x YES token + 1x NO token = $1.00 at resolution (guaranteed)
```

If the combined cost to buy both is less than $1.00, that gap is risk-free profit:
```
YES ask: $0.45
NO ask:  $0.51
Total:   $0.96  →  buy both, profit $0.04 per share at resolution
```

---

## Setup

### 1. Install Rust
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. Set environment variables
```bash
export POLYMARKET_PRIVATE_KEY=0xYOUR_PRIVATE_KEY
export POLYMARKET_FUNDER=0xYOUR_PROXY_WALLET_ADDRESS  # from polymarket.com
```

Your proxy wallet address is shown on polymarket.com when you connect your browser wallet.
It is NOT the same as your EOA/private key address.

### 3. Build (optimised release)
```bash
cargo build --release
```

### 4. Run
```bash
./target/release/eth5m-arb-bot
```

---

## Configuration

Edit constants in `src/main.rs`:

| Constant | Default | Description |
|----------|---------|-------------|
| `MIN_PROFIT` | `0.005` | Minimum 0.5% profit to trade |
| `MAX_POSITION_USDC` | `50.0` | Max USDC per arb trade |
| `STOP_TRADING_BEFORE_EXPIRY_MS` | `30000` | Stop 30s before market expires |
| `POLL_INTERVAL_MS` | `200` | Order book check frequency |

---

## Optimisations Applied

| Optimisation | Impact |
|---|---|
| Deterministic slug generation | Zero Gamma API lag for market discovery |
| HTTP connection pooling + keep-alive | No TCP handshake per request |
| `tokio::join!` for concurrent book fetches | 2x faster than sequential |
| `tokio::join!` for concurrent order placement | Both legs submitted simultaneously |
| FOK orders | No partial leg risk (all-or-nothing) |
| Pre-market loading 10s before expiry | Zero downtime between 5m windows |
| Atomic `RwLock` market state | Non-blocking reads in trading loop |
| `MissedTickBehavior::Skip` | No tick backlog under load |
| Release profile: LTO + codegen-units=1 | Maximum binary optimisation |

---

## Risks

- **Leg risk**: FOK minimises this but both orders are not atomic across each other.
  If YES fills and NO doesn't (or vice versa), the bot cancels the filled leg.
- **Resolution risk**: Bot stops trading 30s before expiry to avoid being caught
  by rapid price movement near resolution.
- **Thin markets**: ETH 5m markets can have wide spreads and low liquidity —
  arb opportunities may be rare or small.
- **Competition**: Other bots run the same strategy. Arb opportunities close fast.

---

## For Lowest Latency

Co-locate your server in **AWS us-east-1** (same region as Polymarket's servers).
This reduces round-trip from ~100ms to ~5ms — far more impactful than any code optimisation.
