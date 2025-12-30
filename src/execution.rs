//! High-performance order execution engine for Polymarket-only arbitrage opportunities.
//!
//! This module handles concurrent order execution for competing outcomes on Polymarket,
//! position reconciliation, and automatic exposure management.

use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{info, warn, error};

use crate::polymarket_clob::SharedAsyncClient;
use crate::types::{
    ArbType, MarketPair,
    FastExecutionRequest, GlobalState,
    cents_to_price,
};
use crate::circuit_breaker::CircuitBreaker;
use crate::position_tracker::{FillRecord, PositionChannel};

// =============================================================================
// EXECUTION ENGINE
// =============================================================================

/// High-precision monotonic clock for latency measurement and performance tracking
pub struct NanoClock {
    start: Instant,
}

impl NanoClock {
    pub fn new() -> Self {
        Self { start: Instant::now() }
    }

    #[inline(always)]
    pub fn now_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
}

impl Default for NanoClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Core execution engine for processing Polymarket-only arbitrage opportunities
pub struct ExecutionEngine {
    poly_async: Arc<SharedAsyncClient>,
    state: Arc<GlobalState>,
    circuit_breaker: Arc<CircuitBreaker>,
    position_channel: PositionChannel,
    in_flight: Arc<[AtomicU64; 8]>,
    clock: NanoClock,
    pub dry_run: bool,
    test_mode: bool,
}

impl ExecutionEngine {
    pub fn new(
        poly_async: Arc<SharedAsyncClient>,
        state: Arc<GlobalState>,
        circuit_breaker: Arc<CircuitBreaker>,
        position_channel: PositionChannel,
        dry_run: bool,
    ) -> Self {
        let test_mode = std::env::var("TEST_ARB")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);

        Self {
            poly_async,
            state,
            circuit_breaker,
            position_channel,
            in_flight: Arc::new(std::array::from_fn(|_| AtomicU64::new(0))),
            clock: NanoClock::new(),
            dry_run,
            test_mode,
        }
    }

    /// Process an execution request
    #[inline]
    pub async fn process(&self, req: FastExecutionRequest) -> Result<ExecutionResult> {
        let market_id = req.market_id;

        // Deduplication check (512 markets via 8x u64 bitmask)
        if market_id < 512 {
            let slot = (market_id / 64) as usize;
            let bit = market_id % 64;
            let mask = 1u64 << bit;
            let prev = self.in_flight[slot].fetch_or(mask, Ordering::AcqRel);
            if prev & mask != 0 {
                return Ok(ExecutionResult {
                    market_id,
                    success: false,
                    profit_cents: 0,
                    latency_ns: self.clock.now_ns() - req.detected_ns,
                    error: Some("Already in-flight"),
                });
            }
        }

        // Get market pair 
        let market = self.state.get_by_id(market_id)
            .ok_or_else(|| anyhow!("Unknown market_id {}", market_id))?;

        let pair = market.pair.as_ref()
            .ok_or_else(|| anyhow!("No pair for market_id {}", market_id))?;

        // Calculate profit
        let profit_cents = req.profit_cents();
        if profit_cents < 1 {
            self.release_in_flight(market_id);
            return Ok(ExecutionResult {
                market_id,
                success: false,
                profit_cents: 0,
                latency_ns: self.clock.now_ns() - req.detected_ns,
                error: Some("Profit below threshold"),
            });
        }

        // Calculate max contracts from size (min of both sides)
        let mut max_contracts = (req.yes_size.min(req.no_size) / 100) as i64;

        // Safety: In test mode, cap position size at 10 contracts
        // Note: Polymarket enforces a $1 minimum order value. At 40¢ per contract,
        // a single contract ($0.40) would be rejected. Using 10 contracts ensures
        // we meet the minimum requirement at any reasonable price level.
        if self.test_mode && max_contracts > 10 {
            warn!("[EXEC] ⚠️ TEST_MODE: Position size capped from {} to 10 contracts", max_contracts);
            max_contracts = 10;
        }

        if max_contracts < 1 {
            warn!(
                "[EXEC] Liquidity fail: {:?} | yes_size={}¢ no_size={}¢",
                req.arb_type, req.yes_size, req.no_size
            );
            self.release_in_flight(market_id);
            return Ok(ExecutionResult {
                market_id,
                success: false,
                profit_cents: 0,
                latency_ns: self.clock.now_ns() - req.detected_ns,
                error: Some("Insufficient liquidity"),
            });
        }

        // Circuit breaker check
        if let Err(_reason) = self.circuit_breaker.can_execute(&pair.pair_id, max_contracts).await {
            self.release_in_flight(market_id);
            return Ok(ExecutionResult {
                market_id,
                success: false,
                profit_cents: 0,
                latency_ns: self.clock.now_ns() - req.detected_ns,
                error: Some("Circuit breaker"),
            });
        }

        let latency_to_exec = self.clock.now_ns() - req.detected_ns;
        info!(
            "[EXEC] 🎯 {} | {:?} y={}¢ n={}¢ | profit={}¢ | {}x | {}µs",
            pair.description,
            req.arb_type,
            req.yes_price,
            req.no_price,
            profit_cents,
            max_contracts,
            latency_to_exec / 1000
        );

        if self.dry_run {
            info!("[EXEC] 🏃 DRY RUN - would execute {} contracts", max_contracts);
            self.release_in_flight_delayed(market_id);
            return Ok(ExecutionResult {
                market_id,
                success: true,
                profit_cents,
                latency_ns: latency_to_exec,
                error: Some("DRY_RUN"),
            });
        }

        // Execute both legs concurrently 
        let result = self.execute_both_legs_async(&req, pair, max_contracts).await;

        // Release in-flight after delay
        self.release_in_flight_delayed(market_id);

        match result {
            // Note: For same-platform arbs (PolyOnly/KalshiOnly), these are YES/NO fills, not platform fills
            Ok((yes_filled, no_filled, yes_cost, no_cost, yes_order_id, no_order_id)) => {
                let matched = yes_filled.min(no_filled);
                let success = matched > 0;
                let actual_profit = matched as i16 * 100 - (yes_cost + no_cost) as i16;

                // === Automatic exposure management for mismatched fills ===
                // If one leg fills more than the other, automatically close the excess
                // to maintain market-neutral exposure (non-blocking background task)
                if yes_filled != no_filled && (yes_filled > 0 || no_filled > 0) {
                    let excess = (yes_filled - no_filled).abs();
                    warn!("[EXEC] ⚠️ Fill mismatch: TokenA={} TokenB={} (excess={})",
                        yes_filled, no_filled, excess);

                    // Spawn auto-close in background (don't block hot path with 2s sleep)
                    let poly_async = self.poly_async.clone();
                    let yes_price = req.yes_price;
                    let no_price = req.no_price;
                    let token_a = pair.poly_yes_token.clone();
                    let token_b = pair.poly_no_token.clone();
                    let original_cost_per_contract = if yes_filled > no_filled {
                        if yes_filled > 0 { yes_cost / yes_filled } else { 0 }
                    } else {
                        if no_filled > 0 { no_cost / no_filled } else { 0 }
                    };

                    tokio::spawn(async move {
                        Self::auto_close_background_poly(
                            poly_async, yes_filled, no_filled,
                            yes_price, no_price, token_a, token_b,
                            original_cost_per_contract
                        ).await;
                    });
                }

                if success {
                    self.circuit_breaker.record_success(&pair.pair_id, matched, matched, actual_profit as f64 / 100.0).await;
                }

                if matched > 0 {
                    // For Polymarket-only: both legs are on Polymarket, competing outcomes
                    self.position_channel.record_fill(FillRecord::new(
                        &pair.pair_id, &pair.description, "polymarket", "token_a",
                        matched as f64, yes_cost as f64 / 100.0 / yes_filled.max(1) as f64,
                        0.0, &yes_order_id,
                    ));
                    self.position_channel.record_fill(FillRecord::new(
                        &pair.pair_id, &pair.description, "polymarket", "token_b",
                        matched as f64, no_cost as f64 / 100.0 / no_filled.max(1) as f64,
                        0.0, &no_order_id,
                    ));
                }

                Ok(ExecutionResult {
                    market_id,
                    success,
                    profit_cents: actual_profit,
                    latency_ns: self.clock.now_ns() - req.detected_ns,
                    error: if success { None } else { Some("Partial/no fill") },
                })
            }
            Err(_e) => {
                self.circuit_breaker.record_error().await;
                Ok(ExecutionResult {
                    market_id,
                    success: false,
                    profit_cents: 0,
                    latency_ns: self.clock.now_ns() - req.detected_ns,
                    error: Some("Execution failed"),
                })
            }
        }
    }

    async fn execute_both_legs_async(
        &self,
        req: &FastExecutionRequest,
        pair: &MarketPair,
        contracts: i64,
    ) -> Result<(i64, i64, i64, i64, String, String)> {
        // Polymarket-only: Token A YES + Token B YES (competing outcomes)
        assert_eq!(req.arb_type, ArbType::PolyOnly, "Only PolyOnly arbitrage is supported");

        let token_a_fut = self.poly_async.buy_fak(
            &pair.poly_yes_token,
            cents_to_price(req.yes_price),
            contracts as f64,
        );
        let token_b_fut = self.poly_async.buy_fak(
            &pair.poly_no_token,
            cents_to_price(req.no_price),
            contracts as f64,
        );
        let (token_a_res, token_b_res) = tokio::join!(token_a_fut, token_b_fut);
        self.extract_poly_only_results(token_a_res, token_b_res)
    }

    /// Extract results from Poly-only execution (competing outcomes)
    fn extract_poly_only_results(
        &self,
        token_a_res: Result<crate::polymarket_clob::PolyFillAsync>,
        token_b_res: Result<crate::polymarket_clob::PolyFillAsync>,
    ) -> Result<(i64, i64, i64, i64, String, String)> {
        let (token_a_filled, token_a_cost, token_a_order_id) = match token_a_res {
            Ok(fill) => {
                ((fill.filled_size as i64), (fill.fill_cost * 100.0) as i64, fill.order_id)
            }
            Err(e) => {
                warn!("[EXEC] Token A failed: {}", e);
                (0, 0, String::new())
            }
        };

        let (token_b_filled, token_b_cost, token_b_order_id) = match token_b_res {
            Ok(fill) => {
                ((fill.filled_size as i64), (fill.fill_cost * 100.0) as i64, fill.order_id)
            }
            Err(e) => {
                warn!("[EXEC] Token B failed: {}", e);
                (0, 0, String::new())
            }
        };

        // Return Token A as "yes" and Token B as "no" to keep existing result handling logic working
        Ok((token_a_filled, token_b_filled, token_a_cost, token_b_cost, token_a_order_id, token_b_order_id))
    }

    /// Background task to automatically close excess exposure from mismatched fills (Polymarket-only)
    async fn auto_close_background_poly(
        poly_async: Arc<SharedAsyncClient>,
        token_a_filled: i64,
        token_b_filled: i64,
        token_a_price: u16,
        token_b_price: u16,
        token_a: Arc<str>,
        token_b: Arc<str>,
        original_cost_per_contract: i64,
    ) {
        let excess = (token_a_filled - token_b_filled).abs();
        if excess == 0 {
            return;
        }

        // Helper to log P&L after close
        let log_close_pnl = |token_name: &str, closed: i64, proceeds: i64| {
            if closed > 0 {
                let close_pnl = proceeds - (original_cost_per_contract * excess);
                info!("[EXEC] ✅ Closed {} {} contracts for {}¢ (P&L: {}¢)",
                    closed, token_name, proceeds, close_pnl);
            } else {
                warn!("[EXEC] ⚠️ Failed to close {} excess - 0 filled", token_name);
            }
        };

        let (token, token_name, price) = if token_a_filled > token_b_filled {
            (&token_a, "Token A", token_a_price)
        } else {
            (&token_b, "Token B", token_b_price)
        };
        let close_price = cents_to_price((price as i16).saturating_sub(10).max(1) as u16);

        info!("[EXEC] 🔄 Waiting 2s for Polymarket settlement before auto-close ({} {} contracts)", excess, token_name);
        tokio::time::sleep(Duration::from_secs(2)).await;

        match poly_async.sell_fak(token, close_price, excess as f64).await {
            Ok(fill) => log_close_pnl(token_name, fill.filled_size as i64, (fill.fill_cost * 100.0) as i64),
            Err(e) => warn!("[EXEC] ⚠️ Failed to close Polymarket excess: {}", e),
        }
    }

    #[inline(always)]
    fn release_in_flight(&self, market_id: u16) {
        if market_id < 512 {
            let slot = (market_id / 64) as usize;
            let bit = market_id % 64;
            let mask = !(1u64 << bit);
            self.in_flight[slot].fetch_and(mask, Ordering::Release);
        }
    }

    fn release_in_flight_delayed(&self, market_id: u16) {
        if market_id < 512 {
            let in_flight = self.in_flight.clone();
            let slot = (market_id / 64) as usize;
            let bit = market_id % 64;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let mask = !(1u64 << bit);
                in_flight[slot].fetch_and(mask, Ordering::Release);
            });
        }
    }
}

/// Result of an execution attempt
#[derive(Debug, Clone, Copy)]
pub struct ExecutionResult {
    /// Market identifier
    pub market_id: u16,
    /// Whether execution was successful
    pub success: bool,
    /// Realized profit in cents
    pub profit_cents: i16,
    /// Total latency from detection to completion in nanoseconds
    pub latency_ns: u64,
    /// Error message if execution failed
    pub error: Option<&'static str>,
}

/// Create a new execution request channel with bounded capacity
pub fn create_execution_channel() -> (mpsc::Sender<FastExecutionRequest>, mpsc::Receiver<FastExecutionRequest>) {
    mpsc::channel(256)
}

/// Main execution event loop - processes arbitrage opportunities as they arrive
pub async fn run_execution_loop(
    mut rx: mpsc::Receiver<FastExecutionRequest>,
    engine: Arc<ExecutionEngine>,
) {
    info!("[EXEC] Execution engine started (dry_run={})", engine.dry_run);

    while let Some(req) = rx.recv().await {
        let engine = engine.clone();

        // Process immediately in spawned task
        tokio::spawn(async move {
            match engine.process(req).await {
                Ok(result) if result.success => {
                    info!(
                        "[EXEC] ✅ market_id={} profit={}¢ latency={}µs",
                        result.market_id, result.profit_cents, result.latency_ns / 1000
                    );
                }
                Ok(result) => {
                    if result.error != Some("Already in-flight") {
                        warn!(
                            "[EXEC] ⚠️ market_id={}: {:?}",
                            result.market_id, result.error
                        );
                    }
                }
                Err(e) => {
                    error!("[EXEC] ❌ Error: {}", e);
                }
            }
        });
    }

    info!("[EXEC] Execution engine stopped");
}