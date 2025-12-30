//! Polymarket-Only Arbitrage Trading System
//!
//! A high-performance, production-ready arbitrage trading system for Polymarket
//! prediction markets. This system monitors price discrepancies between competing
//! outcomes within Polymarket, executing risk-free arbitrage opportunities in real-time.
//!
//! ## Strategy
//!
//! The core arbitrage strategy exploits multi-outcome markets where competing outcomes
//! can be bought for less than $1.00 total. Since exactly ONE outcome resolves to $1.00:
//!
//! ```
//! Token A YES + Token B YES < $1.00  (where A and B are competing outcomes)
//! ```
//!
//! Example: Chelsea (40¢) + Arsenal (58¢) = 98¢ → 2¢ profit (NO FEES!)
//!
//! ## Architecture
//!
//! - **Real-time price monitoring** via WebSocket connections to Polymarket
//! - **Lock-free orderbook cache** using atomic operations for zero-copy updates
//! - **SIMD-accelerated arbitrage detection** for sub-millisecond latency
//! - **Concurrent order execution** with automatic position reconciliation
//! - **Circuit breaker protection** with configurable risk limits
//! - **Market discovery system** for finding competing outcome pairs

mod circuit_breaker;
mod config;
mod discovery;
mod execution;
mod polymarket;
mod polymarket_clob;
mod position_tracker;
mod types;

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};
use config::{ARB_THRESHOLD, ENABLED_LEAGUES, WS_RECONNECT_DELAY_SECS};
use discovery::DiscoveryClient;
use execution::{ExecutionEngine, create_execution_channel, run_execution_loop};
use polymarket_clob::{PolymarketAsyncClient, PreparedCreds, SharedAsyncClient};
use position_tracker::{PositionTracker, create_position_channel, position_writer_loop};
use types::{GlobalState, PriceCents};

/// Polymarket CLOB API host
const POLY_CLOB_HOST: &str = "https://clob.polymarket.com";
/// Polygon chain ID
const POLYGON_CHAIN_ID: u64 = 137;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("arb_bot=info".parse().unwrap()),
        )
        .init();

    info!("🚀 Polymarket-Only Arbitrage System v2.0");
    info!("   Strategy: Competing outcomes arbitrage (NO FEES!)");
    info!("   Profit threshold: <{:.1}¢ ({:.1}% minimum profit)",
          ARB_THRESHOLD * 100.0, (1.0 - ARB_THRESHOLD) * 100.0);
    info!("   Monitored leagues: {:?}", ENABLED_LEAGUES);

    // Check for dry run mode
    let dry_run = std::env::var("DRY_RUN").map(|v| v == "1" || v == "true").unwrap_or(true);
    if dry_run {
        info!("   Mode: DRY RUN (set DRY_RUN=0 to execute)");
    } else {
        warn!("   Mode: LIVE EXECUTION");
    }

    // Load Polymarket credentials
    dotenvy::dotenv().ok();
    let poly_private_key = std::env::var("POLY_PRIVATE_KEY")
        .context("POLY_PRIVATE_KEY not set")?;
    let poly_funder = std::env::var("POLY_FUNDER")
        .context("POLY_FUNDER not set (your wallet address)")?;

    // Create async Polymarket client and derive API credentials
    info!("[POLYMARKET] Creating async client and deriving API credentials...");
    let poly_async_client = PolymarketAsyncClient::new(
        POLY_CLOB_HOST,
        POLYGON_CHAIN_ID,
        &poly_private_key,
        &poly_funder,
    )?;
    let api_creds = poly_async_client.derive_api_key(0).await?;
    let prepared_creds = PreparedCreds::from_api_creds(&api_creds)?;
    let poly_async = Arc::new(SharedAsyncClient::new(poly_async_client, prepared_creds, POLYGON_CHAIN_ID));

    info!("[POLYMARKET] Client ready for {}", &poly_funder[..10]);

    // Run discovery (with caching support)
    let force_discovery = std::env::var("FORCE_DISCOVERY")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);

    info!("🔍 Polymarket-only market discovery{}...",
          if force_discovery { " (forced refresh)" } else { "" });

    let discovery = DiscoveryClient::new_polymarket_only();

    let result = if force_discovery {
        discovery.discover_polymarket_only_force(ENABLED_LEAGUES).await
    } else {
        discovery.discover_polymarket_only(ENABLED_LEAGUES).await
    };

    info!("📊 Market discovery complete:");
    info!("   - Matched market pairs: {}", result.pairs.len());

    if !result.errors.is_empty() {
        for err in &result.errors {
            warn!("   ⚠️ {}", err);
        }
    }

    if result.pairs.is_empty() {
        error!("No market pairs found!");
        return Ok(());
    }

    // Display discovered market pairs (first 5 with full token IDs for debugging)
    info!("📋 Discovered competing outcome pairs:");
    for (i, pair) in result.pairs.iter().enumerate() {
        if i < 5 {
            info!("   ✅ {} | {}",
                  pair.description,
                  pair.market_type);
            info!("      Token A: {} (len={})",
                  &pair.poly_yes_token[..pair.poly_yes_token.len().min(50)],
                  pair.poly_yes_token.len());
            info!("      Token B: {} (len={})",
                  &pair.poly_no_token[..pair.poly_no_token.len().min(50)],
                  pair.poly_no_token.len());
        } else if i == 5 {
            info!("   ... and {} more pairs", result.pairs.len() - 5);
        }
    }

    // Build global state
    let state = Arc::new({
        let mut s = GlobalState::new();
        for pair in result.pairs {
            s.add_pair(pair);
        }
        info!("📡 Global state initialized: tracking {} markets", s.market_count());
        s
    });

    // Initialize execution infrastructure
    let (exec_tx, exec_rx) = create_execution_channel();
    let circuit_breaker = Arc::new(CircuitBreaker::new(CircuitBreakerConfig::from_env()));

    let position_tracker = Arc::new(RwLock::new(PositionTracker::new()));
    let (position_channel, position_rx) = create_position_channel();

    tokio::spawn(position_writer_loop(position_rx, position_tracker));

    let threshold_cents: PriceCents = ((ARB_THRESHOLD * 100.0).round() as u16).max(1);
    info!("   Execution threshold: {} cents", threshold_cents);

    let engine = Arc::new(ExecutionEngine::new(
        poly_async,
        state.clone(),
        circuit_breaker.clone(),
        position_channel,
        dry_run,
    ));

    let exec_handle = tokio::spawn(run_execution_loop(exec_rx, engine));

    // === TEST MODE: Synthetic arbitrage injection ===
    // TEST_ARB=1 to enable (Polymarket-only arbitrage)
    let test_arb = std::env::var("TEST_ARB").map(|v| v == "1" || v == "true").unwrap_or(false);
    if test_arb {
        let test_state = state.clone();
        let test_exec_tx = exec_tx.clone();
        let test_dry_run = dry_run;

        tokio::spawn(async move {
            use types::{FastExecutionRequest, ArbType};

            // Wait for WebSocket connections to establish and populate orderbooks
            info!("[TEST] Injecting synthetic Polymarket-only arbitrage opportunity in 10 seconds...");
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

            // Polymarket-only arbitrage: competing outcomes
            let arb_type = ArbType::PolyOnly;
            let (yes_price, no_price, description) = (48, 50, "TokenA=48¢ + TokenB=50¢ = 98¢ → 2¢ profit (NO FEES!)");

            // Find first market with valid state
            let market_count = test_state.market_count();
            for market_id in 0..market_count {
                if let Some(market) = test_state.get_by_id(market_id as u16) {
                    if let Some(pair) = &market.pair {
                        // SIZE: 1000 cents = 10 contracts (Poly $1 min requires ~3 contracts at 40¢)
                        let fake_req = FastExecutionRequest {
                            market_id: market_id as u16,
                            yes_price,
                            no_price,
                            yes_size: 1000,  // 1000¢ = 10 contracts
                            no_size: 1000,   // 1000¢ = 10 contracts
                            arb_type,
                            detected_ns: 0,
                        };

                        warn!("[TEST] 🧪 Injecting synthetic {:?} arbitrage for: {}", arb_type, pair.description);
                        warn!("[TEST]    Scenario: {}", description);
                        warn!("[TEST]    Position size capped to 10 contracts for safety");
                        warn!("[TEST]    Execution mode: DRY_RUN={}", test_dry_run);

                        if let Err(e) = test_exec_tx.send(fake_req).await {
                            error!("[TEST] Failed to send fake arb: {}", e);
                        }
                        break;
                    }
                }
            }
        });
    }

    // Initialize Polymarket WebSocket connection
    let poly_state = state.clone();
    let poly_exec_tx = exec_tx.clone();
    let poly_threshold = threshold_cents;
    let poly_handle = tokio::spawn(async move {
        loop {
            if let Err(e) = polymarket::run_ws(poly_state.clone(), poly_exec_tx.clone(), poly_threshold).await {
                error!("[POLYMARKET] WebSocket disconnected: {} - reconnecting...", e);
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(WS_RECONNECT_DELAY_SECS)).await;
        }
    });

    // System health monitoring and arbitrage diagnostics
    let heartbeat_state = state.clone();
    let heartbeat_threshold = threshold_cents;
    let heartbeat_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        let mut heartbeat_count = 0u64;
        loop {
            interval.tick().await;
            heartbeat_count += 1;
            let market_count = heartbeat_state.market_count();
            let mut with_token_a = 0;
            let mut with_token_b = 0;
            let mut with_both = 0;
            // Track best arbitrage opportunity: (total_cost, market_id, token_a_price, token_b_price)
            let mut best_arb: Option<(u16, u16, u16, u16)> = None;

            for market in heartbeat_state.markets.iter().take(market_count) {
                let (token_a_yes, token_a_no, _, _) = market.kalshi.load();
                let (token_b_yes, token_b_no, _, _) = market.poly.load();
                // Polymarket binary: YES in kalshi.yes, NO in poly.yes
                let has_token_a = token_a_yes > 0;
                let has_token_b = token_b_yes > 0;
                if token_a_yes > 0 || token_a_no > 0 { with_token_a += 1; }
                if token_b_yes > 0 || token_b_no > 0 { with_token_b += 1; }
                if has_token_a && has_token_b {
                    with_both += 1;

                    // For Polymarket-only, we buy YES on both tokens (competing outcomes)
                    // Cost = Token A YES + Token B YES (no fees on Polymarket!)
                    let cost = token_a_yes + token_b_yes;

                    if best_arb.is_none() || cost < best_arb.as_ref().unwrap().0 {
                        best_arb = Some((cost, market.market_id, token_a_yes, token_b_yes));
                    }
                }
            }

            info!("💓 System heartbeat | Markets: {} total, {} with Token A prices, {} with Token B prices, {} with both | threshold={}¢",
                  market_count, with_token_a, with_token_b, with_both, heartbeat_threshold);

            // Debug: Log first few markets with their price status (skip first heartbeat - WebSocket not connected yet)
            if with_both == 0 && market_count > 0 && heartbeat_count > 1 {
                info!("   🔍 Debugging first 3 markets:");
                for (i, market) in heartbeat_state.markets.iter().take(3.min(market_count)).enumerate() {
                    let (token_a_yes, token_a_no, _, _) = market.kalshi.load();
                    let (token_b_yes, token_b_no, _, _) = market.poly.load();
                    let desc = market.pair.as_ref()
                        .map(|p| p.description.as_ref())
                        .unwrap_or("Unknown");
                    info!("   Market {}: {} | TokenA: yes={}¢ no={}¢ | TokenB: yes={}¢ no={}¢",
                        i, desc, token_a_yes, token_a_no, token_b_yes, token_b_no);
                }
            }

            if let Some((cost, market_id, token_a_price, token_b_price)) = best_arb {
                let gap = cost as i16 - heartbeat_threshold as i16;
                let desc = heartbeat_state.get_by_id(market_id)
                    .and_then(|m| m.pair.as_ref())
                    .map(|p| &*p.description)
                    .unwrap_or("Unknown");
                let leg_breakdown = format!("TokenA({}¢) + TokenB({}¢) = {}¢ (NO FEES!)", token_a_price, token_b_price, cost);
                if gap <= 10 {
                    info!("   📊 Best opportunity: {} | {} | gap={:+}¢",
                          desc, leg_breakdown, gap);
                } else {
                    info!("   📊 Best opportunity: {} | {} | gap={:+}¢ (market efficient)",
                          desc, leg_breakdown, gap);
                }
            } else if with_both == 0 && heartbeat_count > 1 {
                // Skip warning on first heartbeat (WebSocket not connected yet)
                warn!("   ⚠️  No markets with both Token A and Token B prices - verify WebSocket connections");
            }
        }
    });

    // Main event loop - run until termination
    info!("✅ All systems operational - entering main event loop");
    let _ = tokio::join!(poly_handle, heartbeat_handle, exec_handle);

    Ok(())
}
