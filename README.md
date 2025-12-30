# Polymarket YES/NO Token Arbitrage Bot

A high-performance Polymarket arbitrage trading system that detects and executes risk-free arbitrage opportunities when YES + NO token prices sum to less than $1.00.

> **Search Keywords**: polymarket arbitrage bot, polymarket trading bot, prediction market arbitrage, yes no token arbitrage

## Overview

This **Polymarket arbitrage bot** identifies and executes arbitrage opportunities when the combined price of YES and NO tokens on Polymarket is less than $1.00, guaranteeing a risk-free profit at market expiry.

### How It Works

**Example Opportunity:**
- YES token = $0.40, NO token = $0.58
- Total cost = $0.98
- At expiry: Either YES = $1.00 and NO = $0.00, or NO = $1.00 and YES = $0.00
- **Result: 2.04% risk-free return**

### Market Insights

When observing large traders finding significant size in these opportunities, the initial assumption was that opportunities would be extremely fleeting with intense competition. However, the reality is quite different:

- **Opportunities are persistent**: While concurrent dislocations aren't frequent, when they do occur, they persist long enough to execute
- **Large traders use limit orders**: Whales typically fill positions via limit orders over extended periods
- **Manual execution is viable**: Opportunities remain available long enough for manual intervention if needed

### System Workflow

1. **Market Discovery**: Discovers Polymarket markets via Gamma API
2. **Real-time Monitoring**: Subscribes to orderbook delta WebSockets to detect instances where YES + NO can be purchased for less than $1.00
3. **Order Execution**: Executes trades concurrently for both YES and NO tokens
4. **Risk Management**: Includes position management and circuit breakers

### Useful Components

Beyond the complete arbitrage system, you may find these components particularly useful:

- **Rust CLOB client**: A Rust rewrite of Polymarket's Python `py-clob-client` (focused on order submission only)
- **Lock-free atomic orderbook**: High-performance orderbook cache using atomic operations

## Quick Start

### 1. Install Dependencies

```bash
# Rust 1.75+
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build
cargo build --release
```

### 2. Set Up Credentials

Create a `.env` file:

```bash
# === POLYMARKET CREDENTIALS ===
POLY_PRIVATE_KEY=0xYOUR_WALLET_PRIVATE_KEY
POLY_FUNDER=0xYOUR_WALLET_ADDRESS

# === SYSTEM CONFIGURATION ===
DRY_RUN=1
RUST_LOG=info
```

### 3. Run

```bash
# Dry run (paper trading)
dotenvx run -- cargo run --release

# Live execution
DRY_RUN=0 dotenvx run -- cargo run --release
```

---

## Environment Variables

### Required

| Variable           | Description                                                 |
| ------------------ | ----------------------------------------------------------- |
| `POLY_PRIVATE_KEY` | Ethereum private key (with 0x prefix) for Polymarket wallet |
| `POLY_FUNDER`      | Your Polymarket wallet address (with 0x prefix)             |

### System Configuration

| Variable          | Default | Description                                           |
| ----------------- | ------- | ----------------------------------------------------- |
| `DRY_RUN`         | `1`     | `1` = paper trading (no orders), `0` = live execution |
| `RUST_LOG`        | `info`  | Log level: `error`, `warn`, `info`, `debug`, `trace`  |
| `FORCE_DISCOVERY` | `0`     | `1` = re-fetch market mappings (ignore cache)         |
| `PRICE_LOGGING`   | `0`     | `1` = verbose price update logging                    |

### Test Mode

| Variable   | Default | Description                                        |
| ---------- | ------- | -------------------------------------------------- |
| `TEST_ARB` | `0`     | `1` = inject synthetic arb opportunity for testing |

### Circuit Breaker

| Variable                     | Default | Description                                 |
| ---------------------------- | ------- | ------------------------------------------- |
| `CB_ENABLED`                 | `true`  | Enable/disable circuit breaker              |
| `CB_MAX_POSITION_PER_MARKET` | `100`   | Max contracts per market                    |
| `CB_MAX_TOTAL_POSITION`      | `500`   | Max total contracts across all markets      |
| `CB_MAX_DAILY_LOSS`          | `5000`  | Max daily loss in cents before halt         |
| `CB_MAX_CONSECUTIVE_ERRORS`  | `5`     | Consecutive errors before halt              |
| `CB_COOLDOWN_SECS`           | `60`    | Cooldown period after circuit breaker trips |

---

## Obtaining Credentials

### Polymarket

1. Create or import an Ethereum wallet (MetaMask, etc.)
2. Export the private key (include `0x` prefix)
3. Fund your wallet on Polygon network with USDC
4. The wallet address is your `POLY_FUNDER`

---

## Usage Examples

### Paper Trading (Development)

```bash
# Full logging, dry run
RUST_LOG=debug DRY_RUN=1 dotenvx run -- cargo run --release
```

### Test Arbitrage Execution

```bash
# Inject synthetic arb to test execution path
TEST_ARB=1 DRY_RUN=0 dotenvx run -- cargo run --release
```

### Production

```bash
# Live trading with circuit breaker
DRY_RUN=0 CB_MAX_DAILY_LOSS=10000 dotenvx run -- cargo run --release
```

### Force Market Re-Discovery

```bash
# Clear cache and re-fetch all market mappings
FORCE_DISCOVERY=1 dotenvx run -- cargo run --release
```

---

## How It Works

### Arbitrage Mechanics

In prediction markets, the fundamental property holds: **YES + NO = $1.00** (guaranteed).

This **Polymarket arbitrage bot** exploits this property by detecting when:

```
Best YES ask + Best NO ask < $1.00
```

**Example Scenario:**

```
YES token ask:  42 cents
NO token ask:   56 cents
Total cost:     98 cents
Guaranteed payout: 100 cents
Net profit:     2 cents per contract (2.04% return)
```

The bot automatically executes both legs simultaneously, locking in the risk-free profit.

### Fee Structure

- **Polymarket**: Zero trading fees on all orders

---

## Architecture

This **Polymarket arbitrage bot** is built with a modular, high-performance architecture optimized for low-latency execution:

```
src/
├── main.rs              # Application entry point and WebSocket orchestration
├── types.rs             # Core type definitions and market state management
├── execution.rs         # Concurrent order execution engine with position reconciliation
├── position_tracker.rs  # Channel-based position tracking and P&L calculation
├── circuit_breaker.rs   # Risk management with configurable limits and auto-halt
├── discovery.rs         # Market discovery via Gamma API
├── polymarket.rs        # Polymarket WebSocket client and market data
├── polymarket_clob.rs   # Polymarket CLOB order execution client
└── config.rs            # League configurations and system thresholds
```

### Key Features

- **Lock-free orderbook cache** using atomic operations for zero-copy updates
- **SIMD-accelerated arbitrage detection** for sub-millisecond latency
- **Concurrent order execution** with automatic position reconciliation
- **Circuit breaker protection** with configurable risk limits
- **Intelligent market discovery** with caching and incremental updates

---

## Development

### Run Tests

```bash
cargo test
```

### Enable Profiling

```bash
cargo build --release --features profiling
```

### Benchmarks

```bash
cargo bench
```

---

## Project Status

### Completed Features

- [x] Polymarket REST API and WebSocket client
- [x] Lock-free atomic orderbook cache
- [x] SIMD-accelerated arbitrage detection
- [x] Concurrent multi-leg order execution
- [x] Real-time position and P&L tracking
- [x] Circuit breaker with configurable risk limits
- [x] Intelligent market discovery with caching
- [x] Automatic exposure management for mismatched fills

### Future Enhancements

- [ ] Web-based risk limit configuration UI
- [ ] Multi-account support for portfolio management
- [ ] Advanced order routing strategies
- [ ] Historical performance analytics dashboard

---

## Topics & Keywords

This **Polymarket arbitrage bot** repository covers:

- **Polymarket arbitrage** - Automated trading on Polymarket prediction markets
- **YES/NO token arbitrage** - Exploiting price discrepancies between YES and NO tokens
- **Prediction market trading** - Automated trading bot for prediction markets
- **Arbitrage trading bot** - High-frequency arbitrage detection and execution
- **Risk-free trading** - Guaranteed profit via token price discrepancies
- **Rust trading bot** - High-performance trading system written in Rust

### Related Technologies

- Rust async/await for high-performance concurrent execution
- WebSocket real-time price feeds
- REST API integration (Polymarket CLOB)
- Atomic lock-free data structures for orderbook management
- SIMD-accelerated arbitrage detection algorithms

---

## Contributing

Contributions are welcome! This **Polymarket arbitrage bot** is open source and designed to help the prediction market trading community.

## License

This project is licensed under either of

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
