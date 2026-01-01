# How to Fetch Order Size

## Overview

Order size determination involves fetching available liquidity from the orderbook and calculating the maximum executable trade size based on multiple constraints.

## Liquidity Sources

### 1. WebSocket Book Snapshots

Full orderbook state received when subscribing to a market:

```json
{
  "asset_id": "0x1234...abcd",
  "asks": [
    {"price": "0.48", "size": "2000"},
    {"price": "0.49", "size": "1500"},
    {"price": "0.50", "size": "3000"}
  ],
  "bids": [
    {"price": "0.47", "size": "1800"},
    {"price": "0.46", "size": "2200"}
  ]
}
```

The `size` field represents available contracts at each price level (in cents).

### 2. WebSocket Price Change Events

Real-time updates for best ask/bid:

```json
{
  "event_type": "price_change",
  "price_changes": [
    {
      "asset_id": "0x1234...abcd",
      "best_ask": "0.48",
      "size": "100"
    }
  ]
}
```

**Note**: The `size` field here represents the size of the order that triggered the update, NOT total available liquidity.

### 3. Atomic Orderbook Cache

The bot maintains a lock-free atomic orderbook for fast access:

```rust
pub struct AtomicOrderbook {
    // Packed: [price_cents:16][unused:16]
    prices: AtomicU32,
    // Packed: [size_cents:32][unused:32]
    sizes: AtomicU64,
}
```

**Access methods**:

```rust
// Load current state (sub-microsecond)
let (best_ask, best_bid, ask_size, bid_size) = orderbook.load();

// Update YES side
orderbook.update_yes(ask_price_cents, ask_size_cents);

// Update NO side
orderbook.update_no(ask_price_cents, ask_size_cents);
```

## Size Calculation Algorithm

### Step 1: Fetch Available Liquidity

```rust
// From atomic orderbook
let (yes_price, _, yes_size, _) = yes_orderbook.load();
let (no_price, _, no_size, _) = no_orderbook.load();

// Available liquidity is the minimum of both sides
let available_yes_size = yes_size;  // in cents
let available_no_size = no_size;    // in cents
```

### Step 2: Convert to Contracts

```rust
// Liquidity is stored in cents, convert to contracts
// 100 cents = 1 contract (each contract worth $1 at resolution)
let max_from_liquidity = (min(available_yes_size, available_no_size) / 100) as i64;
```

### Step 3: Apply Portfolio Budget

```rust
const MAX_PORTFOLIO_CENTS: u32 = 1500;  // $15.00

// Cost per contract = YES price + NO price (in cents)
let cost_per_contract = yes_price + no_price;

// Maximum affordable contracts
let max_affordable = (MAX_PORTFOLIO_CENTS / cost_per_contract) as i64;
```

### Step 4: Apply Minimum Order Value

Polymarket requires minimum $1.00 per leg:

```rust
// Minimum contracts for YES leg (100 cents / yes_price)
let min_for_yes = (100 + yes_price - 1) / yes_price;  // ceil division

// Minimum contracts for NO leg (100 cents / no_price)
let min_for_no = (100 + no_price - 1) / no_price;     // ceil division

// Overall minimum is the larger of the two
let min_contracts = max(min_for_yes, min_for_no) as i64;
```

### Step 5: Determine Final Size

```rust
// Maximum possible contracts
let max_contracts = min(max_from_liquidity, max_affordable);

// Validate we can meet minimum
if max_contracts < min_contracts {
    return Err("Insufficient size for minimum order value");
}

// In test mode, apply safety cap
let final_contracts = if test_mode {
    min(max_contracts, 10)  // Cap at 10 for safety
} else {
    max_contracts
};
```

## Complete Example

```
Market: "Will Team A win?"

Orderbook State:
- YES best ask: 48 cents, size: 2000 cents
- NO best ask: 50 cents, size: 2500 cents

Step 1: Available Liquidity
- available_yes_size = 2000 cents
- available_no_size = 2500 cents

Step 2: Convert to Contracts
- max_from_liquidity = min(2000, 2500) / 100 = 20 contracts

Step 3: Portfolio Budget
- cost_per_contract = 48 + 50 = 98 cents
- max_affordable = 1500 / 98 = 15 contracts

Step 4: Minimum Order Value
- min_for_yes = ceil(100 / 48) = 3 contracts
- min_for_no = ceil(100 / 50) = 2 contracts
- min_contracts = max(3, 2) = 3 contracts

Step 5: Final Size
- max_contracts = min(20, 15) = 15 contracts ✓
- 15 >= 3 (minimum check passes)
- final_contracts = 15

Trade Execution:
- YES leg: 15 × $0.48 = $7.20
- NO leg: 15 × $0.50 = $7.50
- Total cost: $14.70
- Guaranteed profit: $15.00 - $14.70 = $0.30
```

## Code Reference

### Execution Engine (execution.rs:130-150)

```rust
pub fn calculate_trade_size(
    yes_price: PriceCents,
    no_price: PriceCents,
    yes_size: SizeCents,
    no_size: SizeCents,
    max_portfolio_cents: u32,
) -> Result<i64, &'static str> {
    // Liquidity constraint
    let max_from_liquidity = (min(yes_size, no_size) / 100) as i64;

    // Budget constraint
    let cost_per_contract = (yes_price + no_price) as u32;
    let max_affordable = (max_portfolio_cents / cost_per_contract) as i64;

    // Minimum order constraints
    let min_for_yes = ((100 + yes_price - 1) / yes_price) as i64;
    let min_for_no = ((100 + no_price - 1) / no_price) as i64;
    let min_contracts = max(min_for_yes, min_for_no);

    // Final calculation
    let max_contracts = min(max_from_liquidity, max_affordable);

    if max_contracts < min_contracts {
        return Err("Trade size below minimum");
    }

    Ok(max_contracts)
}
```

## Logging

When liquidity constrains the trade:

```
Portfolio cap: 20 → 15 contracts ($15.00 budget)
```

When liquidity is insufficient:

```
Insufficient liquidity: YES=500 NO=200 (min: 300 needed)
```

## Size Update Flow

```
WebSocket Message
       │
       ▼
┌─────────────────────┐
│ Parse JSON Message  │
└─────────────────────┘
       │
       ▼
┌─────────────────────┐
│ Hash Token Address  │ ← O(1) FxHash lookup
└─────────────────────┘
       │
       ▼
┌─────────────────────┐
│ Find Market State   │
└─────────────────────┘
       │
       ▼
┌─────────────────────┐
│ Update Atomic Book  │ ← Lock-free atomic update
└─────────────────────┘
       │
       ▼
┌─────────────────────┐
│ Check for Arbitrage │
└─────────────────────┘
       │
       ▼
┌─────────────────────┐
│ Send to Execution   │ ← Includes current size
└─────────────────────┘
```

## Considerations

### Size Staleness

Orderbook sizes can become stale between detection and execution. The bot uses FAK (Fill-or-Kill) orders to handle this:

- If size decreased: Order may partially fill or cancel
- Auto-close mechanism handles mismatched fills

### Size in Storage

Arbitrage snapshots record available sizes for analysis:

```rust
ArbSnapshotRecord {
    yes_ask: 48,           // cents
    no_ask: 50,            // cents
    yes_size: 2000,        // cents available
    no_size: 2500,         // cents available
    max_profit_cents: 30,  // based on available liquidity
}
```
