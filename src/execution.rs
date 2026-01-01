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

use crate::config::{MAX_PORTFOLIO_CENTS, debug_sizes_enabled};
use crate::polymarket_clob::SharedAsyncClient;
use crate::types::{
    ArbType, MarketPair,
    FastExecutionRequest, GlobalState,
    cents_to_price,
};
use crate::circuit_breaker::CircuitBreaker;
use crate::position_tracker::{FillRecord, PositionChannel};
use crate::storage::{StorageChannel, TradeRecord};

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
    storage: StorageChannel,
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
        storage: StorageChannel,
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
            storage,
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

        // Debug: log sizes received in execution request
        if debug_sizes_enabled() {
            info!("[SIZE_DEBUG] Exec Received | market_id={} | yes_size={} cents | no_size={} cents | MATCH={}",
                  market_id, req.yes_size, req.no_size,
                  if req.yes_size == req.no_size { "⚠️ SAME" } else { "✓ DIFF" });
        }

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

        // Polymarket enforces $1 minimum order value per leg
        const MIN_ORDER_VALUE_CENTS: u32 = 100; // $1.00

        let cost_per_contract = req.yes_price as u32 + req.no_price as u32;

        // Step 1: Calculate minimum contracts needed for $1 per leg
        // min_contracts = max(ceil(100/yes_price), ceil(100/no_price))
        let min_for_yes = (MIN_ORDER_VALUE_CENTS + req.yes_price as u32 - 1) / req.yes_price as u32;
        let min_for_no = (MIN_ORDER_VALUE_CENTS + req.no_price as u32 - 1) / req.no_price as u32;
        let min_contracts = min_for_yes.max(min_for_no) as i64;

        // Step 2: Calculate max contracts from liquidity and budget
        let max_from_liquidity = (req.yes_size.min(req.no_size) / 100) as i64;
        let max_affordable = (MAX_PORTFOLIO_CENTS / cost_per_contract) as i64;

        // Step 3: Final position size = min(liquidity, affordable) - trade what we can afford
        let mut max_contracts = max_from_liquidity.min(max_affordable);

        // Log if we're capping due to portfolio limit
        if max_from_liquidity > max_affordable {
            info!("[EXEC] 💰 Portfolio cap: {} → {} contracts (${:.2} budget)",
                  max_from_liquidity, max_affordable, MAX_PORTFOLIO_CENTS as f64 / 100.0);
        }

        // Safety: In test mode, cap position size at 10 contracts
        if self.test_mode && max_contracts > 10 {
            warn!("[EXEC] ⚠️ TEST_MODE: Position size capped from {} to 10 contracts", max_contracts);
            max_contracts = 10;
        }

        // Step 4: Validate we can meet $1 minimum per leg (Polymarket requirement)
        if max_contracts < min_contracts {
            warn!(
                "[EXEC] Can't meet $1 minimum per leg: {:?} | \
                 need {} contracts but can only do {} (liquidity={}, budget=${})",
                req.arb_type, min_contracts, max_contracts,
                max_from_liquidity, MAX_PORTFOLIO_CENTS as f64 / 100.0
            );
            self.release_in_flight(market_id);
            return Ok(ExecutionResult {
                market_id,
                success: false,
                profit_cents: 0,
                latency_ns: self.clock.now_ns() - req.detected_ns,
                error: Some("Can't meet $1 minimum per leg"),
            });
        }

        // Log final position sizing
        let yes_order_value = max_contracts as u32 * req.yes_price as u32;
        let no_order_value = max_contracts as u32 * req.no_price as u32;
        let total_cost = max_contracts as u32 * cost_per_contract;
        info!(
            "[EXEC] Position: {} contracts | YES=${:.2} ({} × {}¢) NO=${:.2} ({} × {}¢) | Total=${:.2}",
            max_contracts,
            yes_order_value as f64 / 100.0, max_contracts, req.yes_price,
            no_order_value as f64 / 100.0, max_contracts, req.no_price,
            total_cost as f64 / 100.0
        );

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
                    let token_a = pair.yes_token.clone();
                    let token_b = pair.no_token.clone();
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
                    self.circuit_breaker.record_success(&pair.pair_id, matched).await;
                }

                if matched > 0 {
                    // For Polymarket-only: both legs are YES and NO tokens
                    self.position_channel.record_fill(FillRecord::new(
                        &pair.pair_id, &pair.description, "yes",
                        matched as f64, yes_cost as f64 / 100.0 / yes_filled.max(1) as f64,
                        0.0, &yes_order_id,
                    ));
                    self.position_channel.record_fill(FillRecord::new(
                        &pair.pair_id, &pair.description, "no",
                        matched as f64, no_cost as f64 / 100.0 / no_filled.max(1) as f64,
                        0.0, &no_order_id,
                    ));
                }

                let latency_ns = self.clock.now_ns() - req.detected_ns;

                // Record trade to SQLite storage
                let now = chrono::Utc::now();
                self.storage.record_trade(TradeRecord {
                    pair_id: pair.pair_id.to_string(),
                    timestamp_secs: now.timestamp(),
                    timestamp_ns: (latency_ns % 1_000_000_000) as u32,
                    yes_order_id: yes_order_id.clone(),
                    no_order_id: no_order_id.clone(),
                    yes_filled,
                    no_filled,
                    matched_contracts: matched,
                    yes_cost_cents: yes_cost,
                    no_cost_cents: no_cost,
                    total_cost_cents: yes_cost + no_cost,
                    profit_cents: actual_profit as i64,
                    yes_price: req.yes_price,
                    no_price: req.no_price,
                    latency_us: latency_ns / 1000,
                    success,
                    error: if success { None } else { Some("Partial/no fill".to_string()) },
                    running_type: self.storage.running_type.clone(),
                });

                Ok(ExecutionResult {
                    market_id,
                    success,
                    profit_cents: actual_profit,
                    latency_ns,
                    error: if success { None } else { Some("Partial/no fill") },
                })
            }
            Err(e) => {
                self.circuit_breaker.record_error().await;

                let latency_ns = self.clock.now_ns() - req.detected_ns;

                // Record failed trade to SQLite storage
                let now = chrono::Utc::now();
                self.storage.record_trade(TradeRecord {
                    pair_id: pair.pair_id.to_string(),
                    timestamp_secs: now.timestamp(),
                    timestamp_ns: (latency_ns % 1_000_000_000) as u32,
                    yes_order_id: String::new(),
                    no_order_id: String::new(),
                    yes_filled: 0,
                    no_filled: 0,
                    matched_contracts: 0,
                    yes_cost_cents: 0,
                    no_cost_cents: 0,
                    total_cost_cents: 0,
                    profit_cents: 0,
                    yes_price: req.yes_price,
                    no_price: req.no_price,
                    latency_us: latency_ns / 1000,
                    success: false,
                    error: Some(e.to_string()),
                    running_type: self.storage.running_type.clone(),
                });

                Ok(ExecutionResult {
                    market_id,
                    success: false,
                    profit_cents: 0,
                    latency_ns,
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
        // Polymarket-only: YES token + NO token
        if req.arb_type != ArbType::PolyOnly {
            return Err(anyhow!("Only PolyOnly arbitrage is supported, got {:?}", req.arb_type));
        }

        let yes_fut = self.poly_async.buy_fak(
            &pair.yes_token,
            cents_to_price(req.yes_price),
            contracts as f64,
        );
        let no_fut = self.poly_async.buy_fak(
            &pair.no_token,
            cents_to_price(req.no_price),
            contracts as f64,
        );
        let (yes_res, no_res) = tokio::join!(yes_fut, no_fut);
        self.extract_poly_only_results(yes_res, no_res)
    }

    /// Extract results from Poly-only execution (YES + NO tokens)
    fn extract_poly_only_results(
        &self,
        yes_res: Result<crate::polymarket_clob::PolyFillAsync>,
        no_res: Result<crate::polymarket_clob::PolyFillAsync>,
    ) -> Result<(i64, i64, i64, i64, String, String)> {
        let (yes_filled, yes_cost, yes_order_id) = match yes_res {
            Ok(fill) => {
                ((fill.filled_size as i64), (fill.fill_cost * 100.0) as i64, fill.order_id)
            }
            Err(e) => {
                warn!("[EXEC] YES token failed: {}", e);
                (0, 0, String::new())
            }
        };

        let (no_filled, no_cost, no_order_id) = match no_res {
            Ok(fill) => {
                ((fill.filled_size as i64), (fill.fill_cost * 100.0) as i64, fill.order_id)
            }
            Err(e) => {
                warn!("[EXEC] NO token failed: {}", e);
                (0, 0, String::new())
            }
        };

        Ok((yes_filled, no_filled, yes_cost, no_cost, yes_order_id, no_order_id))
    }

    /// Background task to automatically close excess exposure from mismatched fills (Polymarket-only)
    async fn auto_close_background_poly(
        poly_async: Arc<SharedAsyncClient>,
        yes_filled: i64,
        no_filled: i64,
        yes_price: u16,
        no_price: u16,
        yes_token: Arc<str>,
        no_token: Arc<str>,
        original_cost_per_contract: i64,
    ) {
        let excess = (yes_filled - no_filled).abs();
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

        let (token, token_name, price) = if yes_filled > no_filled {
            (&yes_token, "YES", yes_price)
        } else {
            (&no_token, "NO", no_price)
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