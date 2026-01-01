# Database Specification

## Overview

The arbitrage bot uses SQLite for persistent storage of markets, arbitrage opportunities, and executed trades. A non-blocking background writer thread handles all database operations to avoid impacting hot-path performance.

## Storage Files

| File | Type | Purpose |
|------|------|---------|
| `arb.db` | SQLite | Markets, arb snapshots, trades |
| `positions.json` | JSON | Current positions and P&L tracking |
| `.discovery_cache.json` | JSON | Market pair discovery cache |

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    APPLICATION THREADS                       │
│  (WebSocket Handler, Execution Engine, Position Tracker)     │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ StorageChannel (mpsc)
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                  STORAGE WRITER THREAD                       │
│            (Batched writes, non-blocking)                    │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
                      ┌──────────────┐
                      │   arb.db     │
                      │   (SQLite)   │
                      └──────────────┘
```

**Key Features**:
- Non-blocking writes via mpsc channel
- Batched inserts for efficiency
- Dedicated writer thread (no lock contention)
- WAL mode for concurrent reads

## SQLite Schema

### Table: `markets`

Static metadata for discovered market pairs. Created once during discovery phase.

```sql
CREATE TABLE IF NOT EXISTS markets (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    pair_id             TEXT UNIQUE NOT NULL,   -- Unique market identifier
    league              TEXT NOT NULL,          -- e.g., "epl", "nba", "politics"
    market_type         TEXT NOT NULL,          -- "moneyline", "spread", "total"
    description         TEXT NOT NULL,          -- "Chelsea vs Arsenal"
    category            TEXT,                   -- "Sports", "Politics", "Crypto"
    event_title         TEXT,                   -- Event grouping title
    yes_token_address   TEXT NOT NULL,          -- Polymarket YES token address
    no_token_address    TEXT NOT NULL,          -- Polymarket NO token address
    created_at          INTEGER NOT NULL        -- Unix timestamp
);
```

**Example Row**:
```
id: 1
pair_id: "epl-che-ars-2025-01-01"
league: "epl"
market_type: "moneyline"
description: "Chelsea vs Arsenal - Chelsea Win"
category: "Sports"
event_title: "EPL Match"
yes_token_address: "0x1234...abcd"
no_token_address: "0x5678...efgh"
created_at: 1735689600
```

### Table: `arb_snapshots`

Time-series data for detected arbitrage opportunities. Stores every arb opportunity for analysis.

```sql
CREATE TABLE IF NOT EXISTS arb_snapshots (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    market_id           INTEGER NOT NULL,       -- FK to markets.id
    timestamp           INTEGER NOT NULL,       -- Unix seconds
    timestamp_ns        INTEGER NOT NULL,       -- Nanosecond precision
    yes_ask             INTEGER NOT NULL,       -- YES price in cents (48 = $0.48)
    no_ask              INTEGER NOT NULL,       -- NO price in cents
    total_cost          INTEGER NOT NULL,       -- yes_ask + no_ask
    gap_cents           INTEGER NOT NULL,       -- total_cost - 100 (negative = profit)
    profit_per_contract INTEGER NOT NULL,       -- Cents profit per $1 contract
    max_profit_cents    INTEGER NOT NULL,       -- Max profit based on liquidity
    description         TEXT,                   -- Denormalized for queries
    event_title         TEXT,                   -- Denormalized for queries
    categories          TEXT,                   -- JSON array: ["Sports", "EPL"]
    running_type        TEXT,                   -- "DRY_RUN", "SIMULATE", "REAL_MONEY"
    FOREIGN KEY (market_id) REFERENCES markets(id)
);
```

**Example Row**:
```
id: 42
market_id: 1
timestamp: 1735689650
timestamp_ns: 123456789
yes_ask: 48
no_ask: 50
total_cost: 98
gap_cents: -2
profit_per_contract: 2
max_profit_cents: 30
description: "Chelsea vs Arsenal - Chelsea Win"
event_title: "EPL Match"
categories: ["Sports", "EPL"]
running_type: "DRY_RUN"
```

### Table: `trades`

Executed trade transactions with full audit trail.

```sql
CREATE TABLE IF NOT EXISTS trades (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    market_id           INTEGER NOT NULL,       -- FK to markets.id
    timestamp           INTEGER NOT NULL,       -- Unix seconds
    timestamp_ns        INTEGER NOT NULL,       -- Nanosecond precision
    yes_order_id        TEXT NOT NULL,          -- Polymarket order ID for YES leg
    no_order_id         TEXT NOT NULL,          -- Polymarket order ID for NO leg
    yes_filled          INTEGER NOT NULL,       -- Contracts filled on YES side
    no_filled           INTEGER NOT NULL,       -- Contracts filled on NO side
    matched_contracts   INTEGER NOT NULL,       -- min(yes_filled, no_filled)
    yes_cost_cents      INTEGER NOT NULL,       -- Total cost of YES leg in cents
    no_cost_cents       INTEGER NOT NULL,       -- Total cost of NO leg in cents
    total_cost_cents    INTEGER NOT NULL,       -- yes_cost + no_cost
    profit_cents        INTEGER NOT NULL,       -- Realized P&L in cents
    yes_price           INTEGER NOT NULL,       -- Execution price in cents
    no_price            INTEGER NOT NULL,       -- Execution price in cents
    latency_us          INTEGER NOT NULL,       -- Detection-to-execution microseconds
    success             INTEGER NOT NULL,       -- 1 = success, 0 = failure
    error               TEXT,                   -- Error message if failed
    running_type        TEXT NOT NULL,          -- "DRY_RUN", "SIMULATE", "REAL_MONEY"
    FOREIGN KEY (market_id) REFERENCES markets(id)
);
```

**Example Row**:
```
id: 1
market_id: 1
timestamp: 1735689655
timestamp_ns: 987654321
yes_order_id: "order-abc-123"
no_order_id: "order-def-456"
yes_filled: 15
no_filled: 15
matched_contracts: 15
yes_cost_cents: 720
no_cost_cents: 750
total_cost_cents: 1470
profit_cents: 30
yes_price: 48
no_price: 50
latency_us: 1234
success: 1
error: NULL
running_type: "REAL_MONEY"
```

## Indexes

### arb_snapshots Indexes

```sql
-- Recent opportunities (dashboard queries)
CREATE INDEX idx_arb_time ON arb_snapshots(timestamp DESC);

-- Per-market history
CREATE INDEX idx_arb_market_time ON arb_snapshots(market_id, timestamp DESC);

-- Filter by profitability
CREATE INDEX idx_arb_gap ON arb_snapshots(gap_cents);

-- Category-based filtering
CREATE INDEX idx_arb_categories ON arb_snapshots(categories);

-- DRY_RUN vs REAL_MONEY filtering
CREATE INDEX idx_arb_running_type ON arb_snapshots(running_type);
```

### trades Indexes

```sql
-- Recent trades
CREATE INDEX idx_trades_time ON trades(timestamp DESC);

-- Per-market trade history
CREATE INDEX idx_trades_market_time ON trades(market_id, timestamp DESC);

-- Filter success/failure
CREATE INDEX idx_trades_success ON trades(success);

-- Running type filtering
CREATE INDEX idx_trades_running_type ON trades(running_type);
```

### markets Indexes

```sql
-- Lookup by pair_id
CREATE INDEX idx_markets_pair_id ON markets(pair_id);

-- Category filtering
CREATE INDEX idx_markets_category ON markets(category);
```

## Rust Types

### MarketMetadataRecord

```rust
pub struct MarketMetadataRecord {
    pub pair_id: String,
    pub league: String,
    pub market_type: String,
    pub description: String,
    pub category: Option<String>,
    pub event_title: Option<String>,
    pub yes_token_address: String,
    pub no_token_address: String,
}
```

### ArbSnapshotRecord

```rust
pub struct ArbSnapshotRecord {
    pub pair_id: String,
    pub timestamp_secs: i64,
    pub timestamp_ns: u32,
    pub yes_ask: u16,                    // Price in cents
    pub no_ask: u16,                     // Price in cents
    pub total_cost: u16,                 // yes_ask + no_ask
    pub gap_cents: i16,                  // total_cost - 100
    pub profit_per_contract: u16,        // Cents per contract
    pub max_profit_cents: u32,           // Based on liquidity
    pub description: Option<String>,
    pub event_title: Option<String>,
    pub categories: Option<String>,      // JSON array
    pub running_type: String,
}
```

### TradeRecord

```rust
pub struct TradeRecord {
    pub pair_id: String,
    pub timestamp_secs: i64,
    pub timestamp_ns: u32,
    pub yes_order_id: String,
    pub no_order_id: String,
    pub yes_filled: i64,
    pub no_filled: i64,
    pub matched_contracts: i64,
    pub yes_cost_cents: i64,
    pub no_cost_cents: i64,
    pub total_cost_cents: i64,
    pub profit_cents: i64,
    pub yes_price: u16,
    pub no_price: u16,
    pub latency_us: u64,
    pub success: bool,
    pub error: Option<String>,
    pub running_type: String,
}
```

## positions.json Structure

Current open positions and P&L tracking (in-memory + file persistence).

```json
{
  "positions": {
    "epl-che-ars-2025-01-01": {
      "market_id": "epl-che-ars-2025-01-01",
      "description": "Chelsea vs Arsenal - Chelsea Win",
      "yes_token": {
        "contracts": 15.0,
        "cost_basis": 7.20,
        "avg_price": 0.48
      },
      "no_token": {
        "contracts": 15.0,
        "cost_basis": 7.50,
        "avg_price": 0.50
      },
      "total_fees": 0.0,
      "opened_at": "2025-01-01T10:30:00Z",
      "status": "open"
    }
  },
  "daily_realized_pnl": 0.30,
  "trading_date": "2025-01-01",
  "all_time_pnl": 12.45
}
```

## Common Queries

### Recent Arbitrage Opportunities

```sql
SELECT
    s.timestamp,
    m.description,
    s.yes_ask,
    s.no_ask,
    s.gap_cents,
    s.profit_per_contract,
    s.max_profit_cents
FROM arb_snapshots s
JOIN markets m ON s.market_id = m.id
WHERE s.timestamp > strftime('%s', 'now') - 3600  -- Last hour
ORDER BY s.timestamp DESC
LIMIT 100;
```

### Daily P&L Summary

```sql
SELECT
    date(timestamp, 'unixepoch') as trade_date,
    COUNT(*) as trade_count,
    SUM(matched_contracts) as total_contracts,
    SUM(profit_cents) as total_profit_cents,
    AVG(latency_us) as avg_latency_us
FROM trades
WHERE success = 1
GROUP BY trade_date
ORDER BY trade_date DESC;
```

### Best Opportunities by Category

```sql
SELECT
    categories,
    COUNT(*) as opportunity_count,
    AVG(profit_per_contract) as avg_profit,
    MAX(max_profit_cents) as best_opportunity
FROM arb_snapshots
WHERE gap_cents < 0  -- Profitable opportunities only
GROUP BY categories
ORDER BY avg_profit DESC;
```

### Trade Success Rate

```sql
SELECT
    running_type,
    COUNT(*) as total_trades,
    SUM(success) as successful,
    ROUND(100.0 * SUM(success) / COUNT(*), 2) as success_rate
FROM trades
GROUP BY running_type;
```

### Liquidity-Limited vs Budget-Limited Trades

```sql
SELECT
    CASE
        WHEN total_cost_cents >= 1500 THEN 'budget_limited'
        ELSE 'liquidity_limited'
    END as limit_type,
    COUNT(*) as count
FROM trades
WHERE success = 1
GROUP BY limit_type;
```

## Storage Channel API

### Recording Arb Snapshots

```rust
storage.record_arb(ArbSnapshotRecord {
    pair_id: "epl-che-ars-2025-01-01".to_string(),
    timestamp_secs: now.timestamp(),
    timestamp_ns: 123456789,
    yes_ask: 48,
    no_ask: 50,
    total_cost: 98,
    gap_cents: -2,
    profit_per_contract: 2,
    max_profit_cents: 30,
    description: Some("Chelsea vs Arsenal".to_string()),
    event_title: Some("EPL Match".to_string()),
    categories: Some("[\"Sports\",\"EPL\"]".to_string()),
    running_type: "DRY_RUN".to_string(),
});
```

### Recording Trades

```rust
storage.record_trade(TradeRecord {
    pair_id: "epl-che-ars-2025-01-01".to_string(),
    timestamp_secs: now.timestamp(),
    timestamp_ns: 987654321,
    yes_order_id: "order-abc-123".to_string(),
    no_order_id: "order-def-456".to_string(),
    yes_filled: 15,
    no_filled: 15,
    matched_contracts: 15,
    yes_cost_cents: 720,
    no_cost_cents: 750,
    total_cost_cents: 1470,
    profit_cents: 30,
    yes_price: 48,
    no_price: 50,
    latency_us: 1234,
    success: true,
    error: None,
    running_type: "REAL_MONEY".to_string(),
});
```

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `STORE_ALL_ARB` | `false` | Store all arb snapshots (not just profitable) |
| `DB_PATH` | `arb.db` | SQLite database file path |

## Code References

| File | Purpose |
|------|---------|
| [src/storage/mod.rs](../src/storage/mod.rs) | Storage module entry point |
| [src/storage/schema.rs](../src/storage/schema.rs) | Table creation and migrations |
| [src/storage/types.rs](../src/storage/types.rs) | Record type definitions |
| [src/storage/writer.rs](../src/storage/writer.rs) | Background writer thread |
