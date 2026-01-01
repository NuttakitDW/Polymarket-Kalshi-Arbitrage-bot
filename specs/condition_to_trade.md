# Conditions to Trade

## Arbitrage Detection Criteria

### Core Arbitrage Condition

The bot executes trades when the following condition is met:

```
YES_ask_price + NO_ask_price < ARB_THRESHOLD
```

**Default Threshold**: `ARB_THRESHOLD = 0.995` (99.5 cents)

This means the combined cost of YES and NO tokens must be less than 99.5 cents to trigger a trade, guaranteeing at least a 0.5% profit at resolution.

### Example Detection

```
Market: "Will Team A win?"

YES ask: 48 cents ($0.48)
NO ask:  50 cents ($0.50)
Total:   98 cents ($0.98)

98 < 99.5 → ARBITRAGE DETECTED ✓

Expected profit: $1.00 - $0.98 = $0.02 per contract (2.04% return)
```

## Pre-Trade Validations

### 1. Price Availability

Both YES and NO prices must be available:

```rust
if yes_price == 0 || no_price == 0 {
    return 0;  // No arbitrage - prices missing
}
```

### 2. Minimum Order Value ($1.00 per leg)

Polymarket enforces a minimum order value of $1.00 per leg:

```rust
// Calculate minimum contracts needed for $1.00 order value
min_for_yes = ceil(100 / yes_price)  // e.g., ceil(100/48) = 3 contracts
min_for_no = ceil(100 / no_price)    // e.g., ceil(100/50) = 2 contracts
min_contracts = max(min_for_yes, min_for_no)  // = 3 contracts minimum
```

**Example**:
- YES price: 48 cents → minimum 3 contracts ($1.44)
- NO price: 50 cents → minimum 2 contracts ($1.00)
- Trade requires at least 3 contracts to execute

### 3. Liquidity Constraints

Available liquidity must support the trade:

```rust
max_from_liquidity = min(yes_available_size, no_available_size) / 100
```

The trade size is limited by the smaller of the two orderbook depths.

### 4. Portfolio Budget Limit

Maximum spend per trade is capped:

```rust
MAX_PORTFOLIO_CENTS = 1500  // $15.00 default

max_affordable = MAX_PORTFOLIO_CENTS / (yes_price + no_price)
```

### 5. Final Size Calculation

```rust
max_contracts = min(max_from_liquidity, max_affordable)

// Validate minimum is achievable
if max_contracts < min_contracts {
    return Err("Trade size too small")
}
```

## Circuit Breaker Checks

Before any trade executes, the circuit breaker validates:

### Per-Market Position Limit

```rust
CB_MAX_POSITION_PER_MARKET = 50000  // contracts
```

Prevents overconcentration in a single market.

### Total Portfolio Limit

```rust
CB_MAX_TOTAL_POSITION = 100000  // contracts
```

Hard cap on aggregate exposure across all markets.

### Daily Loss Threshold

```rust
CB_MAX_DAILY_LOSS = 500  // dollars
```

Stops trading if cumulative daily losses exceed this limit.

### Consecutive Error Limit

```rust
CB_MAX_CONSECUTIVE_ERRORS = 5
```

Halts trading after too many consecutive failures.

## Trade Execution Requirements

### 1. Deduplication

The execution engine maintains a bitmask to prevent duplicate trades on the same market within a short window:

```rust
// market_id used to check dedup bitmask
if already_processing(market_id) {
    return;  // Skip duplicate
}
```

### 2. Market Existence

The market pair must exist in the discovery cache:

```rust
let pair = market_pairs.get(&market_id)?;
```

### 3. Valid Token Addresses

Both YES and NO token addresses must be available:

```rust
pair.yes_token  // Must be valid Polymarket token address
pair.no_token   // Must be valid Polymarket token address
```

## Summary of Trade Conditions

| Condition | Requirement | Default Value |
|-----------|-------------|---------------|
| Arbitrage exists | `yes_ask + no_ask < threshold` | < 99.5¢ |
| Prices available | Both YES and NO prices set | Non-zero |
| Minimum order | At least $1.00 per leg | $1.00 |
| Liquidity | Sufficient orderbook depth | Variable |
| Budget | Within portfolio limit | $15.00 |
| Circuit breaker | All checks pass | See config |
| Not duplicate | Not already processing | Bitmask check |
| Market exists | Valid market pair | Discovery cache |

## Configuration

### Environment Variables

```bash
# Arbitrage threshold (default: 0.995)
ARB_THRESHOLD=0.995

# Circuit breaker settings
CB_ENABLED=true
CB_MAX_POSITION_PER_MARKET=50000
CB_MAX_TOTAL_POSITION=100000
CB_MAX_DAILY_LOSS=500
CB_MAX_CONSECUTIVE_ERRORS=5
CB_COOLDOWN_SECS=300
```

### Code Constants (config.rs)

```rust
pub const ARB_THRESHOLD: f64 = 0.995;
pub const MAX_PORTFOLIO_CENTS: u32 = 1500;
pub const POLY_PING_INTERVAL_SECS: u64 = 30;
```
