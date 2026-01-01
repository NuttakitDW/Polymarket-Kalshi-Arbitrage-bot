# How to Work with Portfolio Budget

## Overview

Portfolio budget management controls the maximum capital deployed per trade, ensuring risk is bounded and capital is efficiently allocated across arbitrage opportunities.

## Configuration

### Default Settings

```rust
// config.rs
pub const MAX_PORTFOLIO_CENTS: u32 = 1500;  // $15.00 per trade
```

### Environment Variables

```bash
# Override via environment
CB_MAX_TOTAL_POSITION=100000   # Total contracts across all markets
CB_MAX_POSITION_PER_MARKET=50000  # Contracts per individual market
```

## Budget Calculation

### Per-Trade Budget

The portfolio budget limits the combined cost of YES and NO legs:

```rust
// Total cost per contract = YES price + NO price
let cost_per_contract = yes_price + no_price;  // in cents

// Maximum contracts within budget
let max_affordable = MAX_PORTFOLIO_CENTS / cost_per_contract;
```

### Example Calculations

**Scenario 1: Standard Arbitrage**

```
YES price: 48 cents
NO price: 50 cents
Cost per contract: 98 cents
Budget: 1500 cents ($15.00)

Max contracts = 1500 / 98 = 15 contracts

Trade breakdown:
- YES leg: 15 × $0.48 = $7.20
- NO leg: 15 × $0.50 = $7.50
- Total: $14.70 (within $15.00 budget)
```

**Scenario 2: Wider Spread**

```
YES price: 35 cents
NO price: 55 cents
Cost per contract: 90 cents
Budget: 1500 cents ($15.00)

Max contracts = 1500 / 90 = 16 contracts

Trade breakdown:
- YES leg: 16 × $0.35 = $5.60
- NO leg: 16 × $0.55 = $8.80
- Total: $14.40 (within $15.00 budget)
```

**Scenario 3: Tight Spread**

```
YES price: 49 cents
NO price: 50 cents
Cost per contract: 99 cents
Budget: 1500 cents ($15.00)

Max contracts = 1500 / 99 = 15 contracts

Trade breakdown:
- YES leg: 15 × $0.49 = $7.35
- NO leg: 15 × $0.50 = $7.50
- Total: $14.85 (within $15.00 budget)
```

## Budget vs Liquidity

The final trade size is the minimum of budget constraint and liquidity constraint:

```rust
// Liquidity constraint
let max_from_liquidity = min(yes_size, no_size) / 100;

// Budget constraint
let max_affordable = MAX_PORTFOLIO_CENTS / (yes_price + no_price);

// Final size
let max_contracts = min(max_from_liquidity, max_affordable);
```

### When Budget is Limiting Factor

```
Available liquidity: 20 contracts
Budget allows: 15 contracts
→ Execute 15 contracts (budget-limited)

Log: "Portfolio cap: 20 → 15 contracts ($15.00 budget)"
```

### When Liquidity is Limiting Factor

```
Available liquidity: 10 contracts
Budget allows: 15 contracts
→ Execute 10 contracts (liquidity-limited)

No budget log (not the limiting factor)
```

## Position Tracking

### Cost Basis Recording

Each position tracks the cost basis for P&L calculation:

```rust
pub struct PositionLeg {
    pub contracts: f64,
    pub cost_basis: f64,   // Total cost paid
    pub avg_price: f64,    // Average price per contract
}

pub struct ArbPosition {
    pub yes_token: PositionLeg,
    pub no_token: PositionLeg,
    pub total_fees: f64,   // Always 0 on Polymarket
}
```

### Total Cost Calculation

```rust
impl ArbPosition {
    pub fn total_cost(&self) -> f64 {
        self.yes_token.cost_basis + self.no_token.cost_basis + self.total_fees
    }
}
```

## Profit Calculation

### Guaranteed Profit

For balanced positions (equal YES and NO contracts):

```rust
pub fn guaranteed_profit(&self) -> f64 {
    let matched = self.yes_token.contracts.min(self.no_token.contracts);
    matched - self.total_cost()
}

// Example:
// matched = 15 contracts
// total_cost = $14.70
// guaranteed_profit = $15.00 - $14.70 = $0.30
```

### Profit Percentage

```rust
pub fn profit_percentage(&self) -> f64 {
    let profit = self.guaranteed_profit();
    let cost = self.total_cost();
    (profit / cost) * 100.0
}

// Example:
// profit = $0.30
// cost = $14.70
// percentage = 2.04%
```

## Budget Scaling

### Adjusting Budget

To increase/decrease trade sizes, modify `MAX_PORTFOLIO_CENTS`:

```rust
// Conservative ($5 max per trade)
pub const MAX_PORTFOLIO_CENTS: u32 = 500;

// Standard ($15 max per trade)
pub const MAX_PORTFOLIO_CENTS: u32 = 1500;

// Aggressive ($50 max per trade)
pub const MAX_PORTFOLIO_CENTS: u32 = 5000;
```

### Impact Analysis

| Budget | Example Trade Size | Profit per Trade | Trades for $100 Profit |
|--------|-------------------|------------------|------------------------|
| $5 | 5 contracts | ~$0.10 | 1000 |
| $15 | 15 contracts | ~$0.30 | 333 |
| $50 | 50 contracts | ~$1.00 | 100 |

## Circuit Breaker Integration

### Total Position Limit

Beyond per-trade budget, circuit breaker tracks total exposure:

```rust
pub struct CircuitBreakerConfig {
    pub max_total_position: i64,      // Total contracts across markets
    pub max_position_per_market: i64, // Per-market contract limit
    pub max_daily_loss: f64,          // Daily loss threshold
}
```

### Position Check

Before each trade:

```rust
fn can_execute(&self, new_contracts: i64, market_id: &str) -> bool {
    let current_total = self.total_position.load(Ordering::Relaxed);
    let market_position = self.get_market_position(market_id);

    // Check total position limit
    if current_total + new_contracts > self.config.max_total_position {
        return false;
    }

    // Check per-market limit
    if market_position + new_contracts > self.config.max_position_per_market {
        return false;
    }

    true
}
```

## Persistence

### Positions File (positions.json)

```json
{
  "positions": {
    "arb-123": {
      "market_id": "arb-123",
      "description": "Chelsea vs Arsenal",
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
      "opened_at": "2025-12-31T10:30:00Z",
      "status": "open"
    }
  },
  "daily_realized_pnl": 0.30,
  "trading_date": "2025-12-31",
  "all_time_pnl": 12.45
}
```

### Database Recording (trades table)

```sql
INSERT INTO trades (
    pair_id,
    yes_cost_cents,
    no_cost_cents,
    total_cost_cents,
    profit_cents,
    matched_contracts
) VALUES (
    'arb-123',
    720,     -- $7.20
    750,     -- $7.50
    1470,    -- $14.70
    30,      -- $0.30
    15
);
```

## Best Practices

### 1. Start Conservative

Begin with smaller budgets to validate the system:

```bash
# Testing phase
MAX_PORTFOLIO_CENTS=500  # $5 per trade
```

### 2. Monitor Fill Rates

Track how often liquidity limits trades vs budget:

```sql
SELECT
    COUNT(*) as total_trades,
    SUM(CASE WHEN total_cost_cents < 1500 THEN 1 ELSE 0 END) as budget_limited,
    SUM(CASE WHEN total_cost_cents >= 1500 THEN 1 ELSE 0 END) as liquidity_limited
FROM trades;
```

### 3. Scale Gradually

Increase budget as you gain confidence:

```bash
# Week 1: $5 per trade
# Week 2: $15 per trade
# Week 3+: $25+ per trade (based on results)
```

### 4. Track Exposure

Monitor total capital deployed:

```rust
// Total current exposure
let total_exposure: f64 = positions.values()
    .map(|p| p.total_cost())
    .sum();
```

## Summary

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `MAX_PORTFOLIO_CENTS` | 1500 | Per-trade spending limit |
| `CB_MAX_TOTAL_POSITION` | 100000 | Total contracts limit |
| `CB_MAX_POSITION_PER_MARKET` | 50000 | Per-market contracts |
| `CB_MAX_DAILY_LOSS` | 500 | Daily loss circuit breaker |

The portfolio budget system ensures:
1. Individual trades are size-limited
2. Total exposure is bounded
3. Daily losses are capped
4. Capital efficiency is optimized
