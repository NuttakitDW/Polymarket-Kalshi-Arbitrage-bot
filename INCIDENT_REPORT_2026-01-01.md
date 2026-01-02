# Incident Report: Polymarket Arbitrage Bot Failure

**Date**: January 1, 2026
**Duration**: 17:42 - 23:58 UTC (6+ hours)
**Status**: Critical
**Total Loss**: $49.36 (4936 cents)

---

## Executive Summary

The arbitrage bot experienced a cascading failure during live trading on January 1, 2026. The first trade executed only one side of the arbitrage (102 YES contracts filled, 0 NO contracts), depleting the account balance. The auto-close safety mechanism failed, and all subsequent 25 trade attempts failed due to insufficient balance.

---

## Timeline

| Time (UTC) | Event |
|------------|-------|
| 2025-12-31 05:20 | Bot started in **DRY RUN** mode |
| 2026-01-01 16:55 | Switched to **LIVE EXECUTION** (REAL_MONEY) |
| 2026-01-01 17:42:35 | **CRITICAL**: First trade attempt - 102 contracts |
| 2026-01-01 17:42:36 | YES filled 102 contracts @ 27¢ ($27.54) |
| 2026-01-01 17:42:36 | NO failed: "no orders found to match with FAK order" |
| 2026-01-01 17:42:38 | Auto-close failed: "not enough balance / allowance" |
| 2026-01-01 17:42+ | All subsequent trades fail: balance depleted |
| 2026-01-01 23:58:01 | Last failed trade attempt |

---

## Trade Summary

| # | Time | Market | YES Filled | NO Filled | Loss (¢) |
|---|------|--------|------------|-----------|----------|
| 1 | 17:42 | US strikes Yemen by December 31? | 102 | 0 | -2774 |
| 4 | 20:04 | Will xAI have best AI model Jan? | 104 | 0 | -626 |
| 16 | 21:21 | Luke Humphries PDC Darts? | 91 | 0 | -273 |
| 18 | 21:42 | Andrew Yang Presidential run? | 65 | 0 | -390 |
| 21 | 23:17 | BLACKPINK album by Feb 28? | 0 | 20 | -860 |

**Total one-sided fills**: 5 trades
**Total contracts with unhedged exposure**: 362 YES + 20 NO = 382 contracts
**Total recorded loss**: -$49.36

---

## Root Cause Analysis

### Primary Failure: First Trade Depleted Balance

```
17:42:35 Position: 102 contracts @ YES=27¢ + NO=71¢ = 98¢ (Total=$99.96)
17:42:36 YES token filled: 102 contracts (Cost: $27.54)
17:42:36 NO token failed: "no orders found to match with FAK order"
```

**What happened**:
1. Bot detected arb: YES 27¢ + NO 71¢ = 98¢ (2¢ profit per contract)
2. Both BUY orders sent concurrently via `tokio::join!`
3. YES order filled 102 contracts ($27.54 spent)
4. NO order arrived but **no matching orders existed at 71¢**
5. Account now holds 102 unhedged YES tokens

### Secondary Failure: Auto-Close Safety Net Failed

```
17:42:38 Failed to close Polymarket excess: "not enough balance / allowance"
```

**Why auto-close failed**:
1. After YES filled, balance was depleted
2. Auto-close waited 2 seconds for settlement
3. Attempted to SELL 102 YES tokens to exit position
4. SELL requires balance/allowance for Polymarket's escrow mechanism
5. No balance available → auto-close failed
6. Position remained open with full market exposure

### Tertiary Failure: Cascading Balance Issues

All 25 subsequent trade attempts failed with:
```
"not enough balance / allowance"
```

The bot continued detecting arb opportunities and attempting trades, but with no available balance, every order was rejected.

---

## Current Exposure

Based on the database, the bot currently holds unhedged positions:

| Token | Contracts | Entry Price | Current Status |
|-------|-----------|-------------|----------------|
| YES (Yemen) | 102 | 27¢ | EXPOSED |
| YES (xAI) | 104 | 6¢ | EXPOSED |
| YES (Darts) | 91 | 3¢ | EXPOSED |
| YES (Yang) | 65 | 6¢ | EXPOSED |
| NO (BLACKPINK) | 20 | 43¢ | EXPOSED |

**Total exposure**: 382 contracts at various prices

---

## Failure Modes Identified

### 1. No Liquidity at Target Price (FAK Order Issue)
```
"no orders found to match with FAK order. FAK orders are partially filled or killed if no match is found."
```
- FAK (Fill-And-Kill) orders only fill if there's immediate liquidity
- Orderbook can change between arb detection and order execution
- **Fix needed**: Check real-time orderbook depth before execution

### 2. Balance Race Condition
- YES and NO orders execute concurrently
- Both require available balance
- One can succeed and consume balance before the other executes
- **Fix needed**: Reserve balance for both legs before execution, or execute sequentially

### 3. Auto-Close Requires Balance
- Selling tokens on Polymarket requires balance/allowance
- If balance is depleted, cannot exit positions
- 2-second delay may not be sufficient for order settlement
- **Fix needed**: Ensure sufficient reserve balance for emergency exits

### 4. No Circuit Breaker After First Loss
- Bot continued attempting trades after balance was empty
- 25 additional failed attempts over 6+ hours
- **Fix needed**: Halt trading when balance check fails repeatedly

---

## Error Distribution

| Error Type | Count |
|------------|-------|
| "not enough balance / allowance" | 41 |
| "no orders found to match with FAK order" | 7 |
| "error decoding response body: invalid type: null" | 8 |

---

## Recommendations

### Immediate Actions
1. **Check current balance** on Polymarket wallet
2. **Assess open positions** and determine if manual close is possible
3. **Disable REAL_MONEY mode** until fixes are implemented

### Code Fixes Required

1. **Pre-execution balance check**
   - Query balance before each trade
   - Ensure sufficient funds for BOTH legs plus margin for auto-close

2. **Sequential execution option**
   - Execute NO leg first (less likely to have liquidity issues)
   - Only execute YES if NO succeeds

3. **Improved auto-close**
   - Increase timeout from 2s to 5-10s
   - Retry mechanism for failed closes
   - Market order fallback if limit fails

4. **Circuit breaker enhancement**
   - Trip immediately on "not enough balance" error
   - Require manual reset before resuming

5. **Real-time orderbook validation**
   - Check orderbook depth before execution
   - Abort if insufficient liquidity at target price

---

## Database Summary

```sql
SELECT COUNT(*) as trades, SUM(yes_filled) as yes_total,
       SUM(no_filled) as no_total, SUM(profit_cents) as loss
FROM trades;
```

| Metric | Value |
|--------|-------|
| Total trade attempts | 26 |
| Total YES filled | 362 |
| Total NO filled | 20 |
| Matched contracts | 0 |
| Successful trades | 0 |
| Total loss | -$49.36 |

---

## Conclusion

The incident was caused by a fundamental flaw in the execution strategy: concurrent order execution without balance reservation, combined with a safety net (auto-close) that fails when balance is depleted. The bot needs to either:

1. Reserve balance for both legs before any execution
2. Execute sequentially with rollback capability
3. Maintain emergency reserve balance for position exits

The 2-second auto-close timeout was not the root cause, but extending it and adding retries would improve the safety net's effectiveness.

---

*Report generated: 2026-01-02*
