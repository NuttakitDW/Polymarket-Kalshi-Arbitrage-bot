# About This Project

## Overview

**Polymarket YES/NO Token Arbitrage Bot** is a high-performance automated trading system written in Rust that detects and executes risk-free arbitrage opportunities on Polymarket prediction markets.

## Core Concept

In binary prediction markets, a YES token and NO token for the same outcome always resolve to exactly **$1.00** combined. This creates arbitrage opportunities when:

```
YES_ask_price + NO_ask_price < $1.00
```

### Example Arbitrage

| Token | Price | Action |
|-------|-------|--------|
| YES | $0.48 | Buy |
| NO | $0.50 | Buy |
| **Total Cost** | **$0.98** | |
| **Guaranteed Payout** | **$1.00** | At resolution |
| **Risk-free Profit** | **$0.02** | 2.04% return |

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    MAIN APPLICATION                          │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ↓                     ↓                     ↓
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
│   DISCOVERY      │  │   EXECUTION      │  │   MONITORING     │
│   Market pairs   │  │   Order engine   │  │   60-sec loop    │
└──────────────────┘  └──────────────────┘  └──────────────────┘
        │                     │
        ↓                     ↓
    Gamma API             Polymarket CLOB

┌─────────────────────────────────────────────────────────────┐
│              POLYMARKET WEBSOCKET HANDLER                    │
│  Price updates → AtomicOrderbook → check_arbs() → Execute   │
└─────────────────────────────────────────────────────────────┘
```

## Key Components

| Component | File | Lines | Purpose |
|-----------|------|-------|---------|
| Discovery | `discovery.rs` | 967 | Market pair identification |
| Polymarket WebSocket | `polymarket.rs` | 684 | Real-time price monitoring |
| Polymarket CLOB | `polymarket_clob.rs` | 746 | Order submission API |
| Execution Engine | `execution.rs` | 534 | Trade execution logic |
| Position Tracker | `position_tracker.rs` | 545 | P&L tracking |
| Circuit Breaker | `circuit_breaker.rs` | 409 | Risk management |
| Storage | `storage/` | ~500 | SQLite persistence |

## Technology Stack

- **Language**: Rust 1.75+
- **Async Runtime**: Tokio
- **Blockchain**: Polygon (Chain ID: 137)
- **Database**: SQLite
- **WebSocket**: tungstenite
- **HTTP Client**: reqwest
- **Ethereum Signing**: ethers

## Execution Modes

### Paper Trading (Development)
```bash
DRY_RUN=1 cargo run --release
```

### Live Trading
```bash
DRY_RUN=0 cargo run --release
```

### Synthetic Arb Testing
```bash
TEST_ARB=1 cargo run --release
```

## Key Features

1. **Lock-free Price Updates**: Sub-millisecond atomic orderbook operations
2. **Concurrent Order Execution**: Both legs submitted in parallel
3. **Auto-Close**: Handles fill mismatches automatically
4. **Circuit Breaker**: Configurable risk limits
5. **SQLite Persistence**: Non-blocking write thread with batching
6. **Zero Trading Fees**: Polymarket has no trading fees

## Performance Characteristics

- **Arbitrage Detection**: Sub-millisecond
- **Order Submission**: ~100-500ms (network dependent)
- **Memory Usage**: <100MB
- **Throughput**: 1000s of WebSocket messages/second

## Environment Variables

### Required
```bash
POLY_PRIVATE_KEY=0x...    # Ethereum private key
POLY_FUNDER=0x...         # Wallet address
```

### Optional
```bash
DRY_RUN=1                 # Paper trading mode
RUST_LOG=info             # Log level
ARB_THRESHOLD=0.995       # Profit threshold (99.5 cents)
```

## Data Storage

- **SQLite Database**: `arb.db` - Markets, arb snapshots, trades
- **Positions File**: `positions.json` - Current positions and P&L
- **Discovery Cache**: `.discovery_cache.json` - Market pair cache

## Project Structure

```
src/
├── main.rs               # Entry point
├── config.rs             # Configuration constants
├── types.rs              # Core data structures
├── discovery.rs          # Market discovery
├── polymarket.rs         # WebSocket handler
├── polymarket_clob.rs    # CLOB API client
├── execution.rs          # Trade execution
├── position_tracker.rs   # Position management
├── circuit_breaker.rs    # Risk management
└── storage/
    ├── mod.rs
    ├── writer.rs         # Async write thread
    └── schema.rs         # Database schema
```
