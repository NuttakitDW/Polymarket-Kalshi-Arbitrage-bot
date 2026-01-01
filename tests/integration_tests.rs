// tests/integration_tests.rs
// Holistic integration tests for the arbitrage bot
//
// These tests verify the full flow:
// 1. Arb detection (with fee awareness)
// 2. Position tracking after fills
// 3. Circuit breaker behavior
// 4. End-to-end scenarios

// Note: Arc and RwLock are used by various test modules below

// Note: Old helper function `make_market_state` and `detection_tests` module were removed
// as they tested the old non-atomic MarketArbState architecture that has been deleted.
// The atomic-equivalent tests are in the `integration_tests` module below.

// ============================================================================
// POSITION TRACKER TESTS - Verify fill recording and P&L calculation
// ============================================================================

mod position_tracker_tests {
    use arb_bot::position_tracker::*;

    /// Test: Recording fills updates position correctly
    #[test]
    fn test_record_fills_updates_position() {
        let mut tracker = PositionTracker::new();

        // Record a NO token fill
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Market",
            "no",
            10.0,   // 10 contracts
            0.50,   // at 50¢
            0.18,   // 18¢ fees (for 10 contracts)
            "order123",
        ));

        // Record a YES token fill
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Market",
            "yes",
            10.0,   // 10 contracts
            0.40,   // at 40¢
            0.0,    // no fees
            "order456",
        ));

        let summary = tracker.summary();

        assert_eq!(summary.open_positions, 1, "Should have 1 open position");
        assert!(summary.total_contracts > 0.0, "Should have contracts");

        // Cost basis: 10 * 0.50 + 10 * 0.40 = $9.00 + fees
        assert!(summary.total_cost_basis > 9.0, "Cost basis should be > $9");
    }

    /// Test: Matched arb calculates guaranteed profit
    #[test]
    fn test_matched_arb_guaranteed_profit() {
        let mut pos = ArbPosition::new("TEST-MARKET", "Test");

        // Buy 10 YES tokens at 40¢
        pos.yes_token.add(10.0, 0.40);

        // Buy 10 NO tokens at 50¢
        pos.no_token.add(10.0, 0.50);

        // Add fees
        pos.total_fees = 0.18;  // 18¢ fees

        // Total cost: $4.00 + $5.00 + $0.18 = $9.18
        // Guaranteed payout: $10.00
        // Guaranteed profit: $0.82

        assert!((pos.total_cost() - 9.18).abs() < 0.01, "Cost should be $9.18");
        assert!((pos.matched_contracts() - 10.0).abs() < 0.01, "Should have 10 matched");
        assert!(pos.guaranteed_profit() > 0.0, "Should have positive guaranteed profit");
        assert!((pos.guaranteed_profit() - 0.82).abs() < 0.01, "Profit should be ~$0.82");
    }

    /// Test: Partial fills create exposure
    #[test]
    fn test_partial_fill_creates_exposure() {
        let mut pos = ArbPosition::new("TEST-MARKET", "Test");

        // Full fill on YES token
        pos.yes_token.add(10.0, 0.40);

        // Partial fill on NO token (only 7 contracts)
        pos.no_token.add(7.0, 0.50);

        assert!((pos.matched_contracts() - 7.0).abs() < 0.01, "Should have 7 matched");
        assert!((pos.unmatched_exposure() - 3.0).abs() < 0.01, "Should have 3 unmatched");
    }

    /// Test: Position resolution calculates realized P&L
    #[test]
    fn test_position_resolution() {
        let mut pos = ArbPosition::new("TEST-MARKET", "Test");

        pos.yes_token.add(10.0, 0.40);   // Cost: $4.00
        pos.no_token.add(10.0, 0.50);    // Cost: $5.00
        pos.total_fees = 0.18;            // Fees: $0.18

        // YES wins -> YES token pays $10
        pos.resolve(true);

        assert_eq!(pos.status, "resolved");
        let pnl = pos.realized_pnl.expect("Should have realized P&L");

        // Payout: $10 - Cost: $9.18 = $0.82 profit
        assert!((pnl - 0.82).abs() < 0.01, "P&L should be ~$0.82, got {}", pnl);
    }

    /// Test: Daily P&L resets
    #[test]
    fn test_daily_pnl_persistence() {
        let mut tracker = PositionTracker::new();

        // Simulate some activity
        tracker.all_time_pnl = 100.0;
        tracker.daily_realized_pnl = 10.0;

        // Reset daily
        tracker.reset_daily();

        assert_eq!(tracker.daily_realized_pnl, 0.0, "Daily should reset");
        assert_eq!(tracker.all_time_pnl, 100.0, "All-time should persist");
    }
}

// ============================================================================
// CIRCUIT BREAKER TESTS - Verify safety limits
// ============================================================================

mod circuit_breaker_tests {
    use arb_bot::circuit_breaker::*;
    
    fn test_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            max_total_position: 200,
            max_consecutive_errors: 3,
            cooldown_secs: 60,
            enabled: true,
        }
    }
    
    /// Test: Allows trades within limits
    #[tokio::test]
    async fn test_allows_trades_within_limits() {
        let cb = CircuitBreaker::new(test_config());

        // First trade should be allowed
        let result = cb.can_execute("market1", 10).await;
        assert!(result.is_ok(), "Should allow first trade");

        // Record success (10 matched = 20 total position for YES+NO)
        cb.record_success("market1", 10).await;

        // Second trade on same market should still be allowed
        let result = cb.can_execute("market1", 10).await;
        assert!(result.is_ok(), "Should allow second trade within limit");
    }

    /// Test: Blocks trade exceeding total position limit
    #[tokio::test]
    async fn test_blocks_total_position_limit() {
        let cb = CircuitBreaker::new(test_config());

        // Fill up with trades (each matched = 2x position for YES+NO)
        cb.record_success("market1", 50).await;  // 100 total
        cb.record_success("market2", 50).await;  // 200 total (at limit)

        // Try to add 1 more (would be 201 > 200 limit)
        let result = cb.can_execute("market3", 1).await;

        assert!(matches!(result, Err(TripReason::MaxTotalPosition { .. })),
            "Should block trade exceeding total position limit");
    }
    
    /// Test: Consecutive errors trip the breaker
    #[tokio::test]
    async fn test_consecutive_errors_trip() {
        let cb = CircuitBreaker::new(test_config());
        
        // Record errors up to limit
        cb.record_error().await;
        assert!(cb.is_trading_allowed(), "Should still allow after 1 error");
        
        cb.record_error().await;
        assert!(cb.is_trading_allowed(), "Should still allow after 2 errors");
        
        cb.record_error().await;
        assert!(!cb.is_trading_allowed(), "Should halt after 3 errors");
        
        // Verify trip reason
        let status = cb.status().await;
        assert!(status.halted);
        assert!(matches!(status.trip_reason, Some(TripReason::ConsecutiveErrors { .. })));
    }
    
    /// Test: Success resets error count
    #[tokio::test]
    async fn test_success_resets_errors() {
        let cb = CircuitBreaker::new(test_config());

        // Record 2 errors
        cb.record_error().await;
        cb.record_error().await;

        // Record success
        cb.record_success("market1", 10).await;
        
        // Error count should be reset
        let status = cb.status().await;
        assert_eq!(status.consecutive_errors, 0, "Success should reset error count");
        
        // Should need 3 more errors to trip
        cb.record_error().await;
        cb.record_error().await;
        assert!(cb.is_trading_allowed());
    }
    
    /// Test: Manual reset clears halt
    #[tokio::test]
    async fn test_manual_reset() {
        let cb = CircuitBreaker::new(test_config());
        
        // Trip the breaker
        cb.record_error().await;
        cb.record_error().await;
        cb.record_error().await;
        assert!(!cb.is_trading_allowed());
        
        // Reset
        cb.reset().await;
        assert!(cb.is_trading_allowed(), "Should allow trading after reset");
        
        let status = cb.status().await;
        assert!(!status.halted);
        assert!(status.trip_reason.is_none());
    }
    
    /// Test: Disabled circuit breaker allows everything
    #[tokio::test]
    async fn test_disabled_allows_all() {
        let mut config = test_config();
        config.enabled = false;
        let cb = CircuitBreaker::new(config);
        
        // Should allow even excessive trades
        let result = cb.can_execute("market1", 1000).await;
        assert!(result.is_ok(), "Disabled CB should allow all trades");
        
        // Errors shouldn't trip it
        cb.record_error().await;
        cb.record_error().await;
        cb.record_error().await;
        cb.record_error().await;
        assert!(cb.is_trading_allowed(), "Disabled CB should never halt");
    }
}

// ============================================================================
// END-TO-END SCENARIO TESTS - Full flow simulation
// ============================================================================

mod e2e_tests {
    use arb_bot::position_tracker::*;

    // Note: test_full_arb_lifecycle was removed because it used the deleted
    // make_market_state helper and MarketArbState::check_arbs method.
    // See arb_detection_tests::test_complete_arb_flow for the equivalent.

    /// Scenario: Partial fill creates exposure warning
    #[tokio::test]
    async fn test_partial_fill_exposure_tracking() {
        let mut tracker = PositionTracker::new();

        // Full fill on YES token
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test",
            "yes",
            10.0,
            0.40,
            0.0,
            "order1",
        ));

        // Partial fill on NO token (slippage/liquidity issue)
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test",
            "no",
            7.0,  // Only got 7!
            0.50,
            0.13,
            "order2",
        ));

        let summary = tracker.summary();

        // Should show exposure
        assert!(
            summary.total_unmatched_exposure > 0.0,
            "Should show unmatched exposure: {}",
            summary.total_unmatched_exposure
        );

        // Matched should be limited to the smaller fill
        let position = tracker.get("TEST-MARKET").expect("Should have position");
        assert!((position.matched_contracts() - 7.0).abs() < 0.01);
        assert!((position.unmatched_exposure() - 3.0).abs() < 0.01);
    }

    // Note: test_fees_prevent_false_arb was removed because it used the deleted
    // make_market_state helper and MarketArbState::check_arbs method.
    // See arb_detection_tests::test_fees_eliminate_marginal_arb for the equivalent.
}

// ============================================================================
// FILL DATA ACCURACY TESTS - Verify actual vs expected prices
// ============================================================================

mod fill_accuracy_tests {
    use arb_bot::position_tracker::*;

    /// Test: Actual fill price different from expected
    #[test]
    fn test_fill_price_slippage() {
        let mut tracker = PositionTracker::new();

        // Expected: buy at 40¢, but actually filled at 42¢ (slippage)
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test",
            "yes",
            10.0,
            0.42,  // Actual fill price (worse than expected 0.40)
            0.0,
            "order1",
        ));

        let pos = tracker.get("TEST-MARKET").expect("Should have position");

        // Should use actual price
        assert!((pos.yes_token.avg_price - 0.42).abs() < 0.001);
        assert!((pos.yes_token.cost_basis - 4.20).abs() < 0.01);
    }

    /// Test: Multiple fills at different prices calculates weighted average
    #[test]
    fn test_multiple_fills_weighted_average() {
        let mut pos = ArbPosition::new("TEST", "Test");

        // First fill: 5 contracts at 40¢
        pos.yes_token.add(5.0, 0.40);

        // Second fill: 5 contracts at 44¢ (price moved)
        pos.yes_token.add(5.0, 0.44);

        // Weighted average: (5*0.40 + 5*0.44) / 10 = 0.42
        assert!((pos.yes_token.avg_price - 0.42).abs() < 0.001);
        assert!((pos.yes_token.cost_basis - 4.20).abs() < 0.01);
        assert!((pos.yes_token.contracts - 10.0).abs() < 0.01);
    }

    /// Test: Actual fees from API response
    #[test]
    fn test_actual_fees_recorded() {
        let mut tracker = PositionTracker::new();

        // API reports actual fees in response
        // Expected: 18¢ for 10 contracts at 50¢
        // Actual from API: 20¢ (maybe market-specific fee)
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test",
            "no",
            10.0,
            0.50,
            0.20,  // Actual fees from API
            "order1",
        ));

        let pos = tracker.get("TEST-MARKET").expect("Should have position");
        assert!((pos.total_fees - 0.20).abs() < 0.001, "Should use actual fees");
    }
}

// ============================================================================
// INFRASTRUCTURE INTEGRATION TESTS
// ============================================================================

mod infra_integration_tests {
    use arb_bot::types::*;

    /// Helper to create market state with prices
    fn setup_market(
        yes_price: PriceCents,
        _yes_no_unused: PriceCents,
        no_price: PriceCents,
        _no_no_unused: PriceCents,
    ) -> (GlobalState, u16) {
        let mut state = GlobalState::new();

        let pair = MarketPair {
            pair_id: "arb-test-market".into(),
            league: "epl".into(),
            market_type: MarketType::Moneyline,
            description: "Test Market".into(),
            yes_token_condition: "yes-condition-arb-test".into(),
            no_token_condition: "no-condition-arb-test".into(),
            poly_slug: "arb-test".into(),
            yes_token: "arb_yes_token".into(),
            no_token: "arb_no_token".into(),
            line_value: None,
            team_suffix: Some("CFC".into()),
            category: None,
            event_title: None,
        };

        let market_id = state.add_pair(pair).unwrap();

        // Set prices (Polymarket-only mode: YES and NO orderbooks)
        let market = state.get_by_id(market_id).unwrap();
        market.yes_book.store(yes_price, 0, 1000, 1000);
        market.no_book.store(no_price, 0, 1000, 1000);

        (state, market_id)
    }

    // =========================================================================
    // Arb Detection Tests (check_arbs)
    // =========================================================================

    /// Test: detects Polymarket-only arb (no fees)
    #[test]
    fn test_detects_poly_only_arb() {
        // In Polymarket-only mode:
        // - YES token at 48¢
        // - NO token at 50¢
        // YES + NO = 48 + 50 = 98¢ < 100¢ -> ARB!
        let (state, market_id) = setup_market(48, 0, 50, 0);  // YES=48¢, NO=50¢

        let market = state.get_by_id(market_id).unwrap();
        let arb_mask = market.check_arbs(100);

        assert!(arb_mask != 0, "Should detect Poly-only arb");
    }

    /// Test: correctly rejects marginal arb when prices are too high
    #[test]
    fn test_no_arb_when_prices_too_high() {
        // YES 49¢ + NO 52¢ = 101¢ -> NOT AN ARB (costs more than $1 payout!)
        let (state, market_id) = setup_market(49, 0, 52, 0);

        let market = state.get_by_id(market_id).unwrap();
        let arb_mask = market.check_arbs(100);

        assert_eq!(arb_mask, 0, "Should not detect arb when combined price > 100¢");
    }

    /// Test: returns no arbs for efficient market
    #[test]
    fn test_no_arbs_in_efficient_market() {
        // All prices sum to > $1
        let (state, market_id) = setup_market(55, 0, 52, 0);

        let market = state.get_by_id(market_id).unwrap();
        let arb_mask = market.check_arbs(100);

        assert_eq!(arb_mask, 0, "Should detect no arbs in efficient market");
    }

    /// Test: handles missing prices correctly
    #[test]
    fn test_handles_missing_prices() {
        let (state, market_id) = setup_market(50, 0, NO_PRICE, 0);

        let market = state.get_by_id(market_id).unwrap();
        let arb_mask = market.check_arbs(100);

        assert_eq!(arb_mask, 0, "Should return 0 when any price is missing");
    }

    // Note: Fee calculation tests removed - Polymarket has no trading fees

    // =========================================================================
    // FastExecutionRequest Tests
    // =========================================================================

    /// Test: FastExecutionRequest calculates profit correctly (Polymarket-only)
    #[test]
    fn test_execution_request_profit_calculation() {
        // YES 40¢ + NO 50¢ = 90¢ (no fees on Polymarket)
        // Profit = 100 - 90 = 10¢
        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 50,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        assert_eq!(req.profit_cents(), 10, "Profit should be 10¢");
    }

    /// Test: FastExecutionRequest handles negative profit
    #[test]
    fn test_execution_request_negative_profit() {
        // Prices too high - no profit
        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 52,
            no_price: 52,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        assert!(req.profit_cents() < 0, "Should calculate negative profit");
    }

    // =========================================================================
    // GlobalState Lookup Tests
    // =========================================================================

    /// Test: GlobalState lookup by YES token condition hash
    #[test]
    fn test_lookup_by_yes_condition_hash() {
        let (state, market_id) = setup_market(50, 0, 50, 0);

        // id_by_yes_token_hash looks up by yes_token_condition, not yes_token
        let yes_condition_hash = fxhash_str("yes-condition-arb-test");
        let found_id = state.id_by_yes_token_hash(yes_condition_hash);

        assert_eq!(found_id, Some(market_id), "Should find market by YES condition hash");
    }

    /// Test: GlobalState lookup by token address hashes
    #[test]
    fn test_lookup_by_token_hashes() {
        let (state, market_id) = setup_market(50, 0, 50, 0);

        let yes_addr_hash = fxhash_str("arb_yes_token");
        let no_addr_hash = fxhash_str("arb_no_token");

        assert_eq!(state.id_by_yes_addr_hash(yes_addr_hash), Some(market_id));
        assert_eq!(state.id_by_no_addr_hash(no_addr_hash), Some(market_id));
    }

    /// Test: GlobalState handles multiple markets
    #[test]
    fn test_multiple_markets() {
        let mut state = GlobalState::new();

        // Add 5 markets
        for i in 0..5 {
            let pair = MarketPair {
                pair_id: format!("market-{}", i).into(),
                league: "epl".into(),
                market_type: MarketType::Moneyline,
                description: format!("Market {}", i).into(),
                yes_token_condition: format!("yes-condition-{}", i).into(),
                no_token_condition: format!("no-condition-{}", i).into(),
                poly_slug: format!("test-{}", i).into(),
                yes_token: format!("yes_{}", i).into(),
                no_token: format!("no_{}", i).into(),
                line_value: None,
                team_suffix: None,
                category: None,
                event_title: None,
            };

            let id = state.add_pair(pair).unwrap();
            assert_eq!(id, i as u16);
        }

        assert_eq!(state.market_count(), 5);

        // All should be findable
        for i in 0..5 {
            assert!(state.get_by_id(i as u16).is_some());
            // id_by_yes_token_hash uses yes_token_condition, id_by_yes_addr_hash uses yes_token
            assert_eq!(state.id_by_yes_addr_hash(fxhash_str(&format!("yes_{}", i))), Some(i as u16));
        }
    }

    // =========================================================================
    // Price Conversion Tests
    // =========================================================================

    /// Test: Price conversion roundtrip
    #[test]
    fn test_price_conversion_roundtrip() {
        for cents in [1u16, 10, 25, 50, 75, 90, 99] {
            let price = cents_to_price(cents);
            let back = price_to_cents(price);
            assert_eq!(back, cents, "Roundtrip failed for {}¢", cents);
        }
    }

    /// Test: Fast price parsing
    #[test]
    fn test_parse_price_accuracy() {
        assert_eq!(parse_price("0.50"), 50);
        assert_eq!(parse_price("0.01"), 1);
        assert_eq!(parse_price("0.99"), 99);
        assert_eq!(parse_price("0.5"), 50);  // Short format
        assert_eq!(parse_price("invalid"), 0);  // Invalid
    }

    // =========================================================================
    // Full Flow Integration Test
    // =========================================================================

    /// Test: Complete arb detection -> execution request flow (Polymarket-only)
    #[test]
    fn test_complete_arb_flow() {
        // 1. Setup market with arb opportunity (YES 40¢ + NO 50¢ = 90¢)
        let (state, market_id) = setup_market(40, 0, 50, 0);

        // 2. Detect arb (threshold = 100 cents = $1.00)
        let market = state.get_by_id(market_id).unwrap();
        let arb_mask = market.check_arbs(100);

        assert!(arb_mask != 0, "Step 2: Should detect arb");

        // 3. Extract prices for execution
        let (yes_ask, _, yes_sz, _) = market.yes_book.load();
        let (no_ask, _, no_sz, _) = market.no_book.load();

        // 4. Build execution request
        let req = FastExecutionRequest {
            market_id,
            yes_price: yes_ask,
            no_price: no_ask,
            yes_size: yes_sz,
            no_size: no_sz,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // 5. Verify request is valid
        assert_eq!(req.yes_price, 40, "YES price should be 40¢");
        assert_eq!(req.no_price, 50, "NO price should be 50¢");
        assert!(req.profit_cents() > 0, "Should have positive profit");

        // 6. Verify we can access market pair for execution
        let pair = market.pair.as_ref().expect("Should have pair");
        assert!(!pair.yes_token.is_empty());
        assert!(!pair.no_token.is_empty());
    }

    // Note: test_vs_legacy_arb_detection_consistency was removed because
    // it tested against the deleted MarketArbState, PlatformState, and MarketSide types.
    // The arb detection (check_arbs) is tested comprehensively above.
}

// ============================================================================
// EXECUTION ENGINE TESTS - Test execution without real API calls
// ============================================================================

mod execution_tests {
    use arb_bot::types::*;
    use arb_bot::circuit_breaker::*;
    use arb_bot::position_tracker::*;

    /// Test: ExecutionEngine correctly filters low-profit opportunities
    #[tokio::test]
    async fn test_execution_profit_threshold() {
        // This tests the logic flow - actual execution would need mocked clients
        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 50,
            no_price: 52,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // 50 + 52 = 102 > 100 -> negative profit
        assert!(req.profit_cents() < 0, "Should have negative profit");
    }

    /// Test: ExecutionEngine respects circuit breaker
    #[tokio::test]
    async fn test_circuit_breaker_integration() {
        let config = CircuitBreakerConfig {
            max_total_position: 100,
            max_consecutive_errors: 3,
            cooldown_secs: 60,
            enabled: true,
        };

        let cb = CircuitBreaker::new(config);

        // Fill up total position (50 matched = 100 total, at limit)
        cb.record_success("market1", 50).await;

        // Should block when adding more (100 + 1 > 100)
        let result = cb.can_execute("market2", 1).await;
        assert!(matches!(result, Err(TripReason::MaxTotalPosition { .. })));
    }

    /// Test: Position tracker records fills correctly
    #[tokio::test]
    async fn test_position_tracker_integration() {
        let mut tracker = PositionTracker::new();

        // Simulate fill recording (what ExecutionEngine does)
        tracker.record_fill(&FillRecord::new(
            "test-market-1",
            "Test Market",
            "no",
            10.0,
            0.50,
            0.18,  // fees
            "test_order_123",
        ));

        tracker.record_fill(&FillRecord::new(
            "test-market-1",
            "Test Market",
            "yes",
            10.0,
            0.40,
            0.0,
            "test_order_456",
        ));

        let summary = tracker.summary();
        assert_eq!(summary.open_positions, 1);
        assert!(summary.total_guaranteed_profit > 0.0);
    }

    /// Test: NanoClock provides monotonic timing
    #[test]
    fn test_nano_clock_monotonic() {
        use arb_bot::execution::NanoClock;

        let clock = NanoClock::new();

        let t1 = clock.now_ns();
        std::thread::sleep(std::time::Duration::from_micros(100));
        let t2 = clock.now_ns();

        assert!(t2 > t1, "Clock should be monotonic");
        assert!(t2 - t1 >= 100_000, "Should measure at least 100µs");
    }
}

// ============================================================================
// MISMATCHED FILL / AUTO-CLOSE EXPOSURE TESTS
// ============================================================================
// These tests verify that when YES and NO token fills have different quantities,
// the system correctly handles the unmatched exposure.

mod mismatched_fill_tests {
    use arb_bot::position_tracker::*;

    /// Test: When YES fills more than NO, we have excess YES exposure
    /// that needs to be sold to close the position.
    #[test]
    fn test_yes_fills_more_than_no_creates_exposure() {
        let mut tracker = PositionTracker::new();

        // Scenario: Requested 10 contracts
        // NO filled: 7 contracts at 50¢
        // YES filled: 10 contracts at 40¢
        // Excess: 3 YES contracts that aren't hedged

        // Record NO fill (only 7)
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "no",
            7.0,      // Only 7 filled
            0.50,
            0.12,     // fees
            "no_order_1",
        ));

        // Record YES fill (full 10)
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "yes",
            10.0,     // Full 10 filled
            0.40,
            0.0,
            "yes_order_1",
        ));

        let pos = tracker.get("TEST-MARKET").expect("Should have position");

        // Matched position: 7 contracts
        // Unmatched YES: 3 contracts
        assert!((pos.yes_token.contracts - 10.0).abs() < 0.01, "YES should have 10 contracts");
        assert!((pos.no_token.contracts - 7.0).abs() < 0.01, "NO should have 7 contracts");

        // This position has EXPOSURE because yes_token (10) != no_token (7)
        // The 7 matched contracts are hedged (guaranteed profit)
        // The 3 excess yes_token contracts are unhedged exposure
    }

    /// Test: When NO fills more than YES, we have excess NO exposure
    /// that needs to be sold to close the position.
    #[test]
    fn test_no_fills_more_than_yes_creates_exposure() {
        let mut tracker = PositionTracker::new();

        // Scenario: Requested 10 contracts
        // NO filled: 10 contracts at 50¢
        // YES filled: 6 contracts at 40¢
        // Excess: 4 NO contracts that aren't hedged

        // Record NO fill (full 10)
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "no",
            10.0,     // Full 10 filled
            0.50,
            0.18,     // fees
            "no_order_1",
        ));

        // Record YES fill (only 6)
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "yes",
            6.0,      // Only 6 filled
            0.40,
            0.0,
            "yes_order_1",
        ));

        let pos = tracker.get("TEST-MARKET").expect("Should have position");

        assert!((pos.no_token.contracts - 10.0).abs() < 0.01, "NO should have 10 contracts");
        assert!((pos.yes_token.contracts - 6.0).abs() < 0.01, "YES should have 6 contracts");

        // The 6 matched contracts are hedged
        // The 4 excess no_token contracts are unhedged exposure
    }

    /// Test: After auto-closing excess YES, position should be balanced
    #[test]
    fn test_auto_close_yes_excess_balances_position() {
        let mut tracker = PositionTracker::new();

        // Initial mismatched fill
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "no",
            7.0,
            0.50,
            0.12,
            "no_order_1",
        ));

        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "yes",
            10.0,
            0.40,
            0.0,
            "yes_order_1",
        ));

        // Simulate auto-close: SELL 3 YES to close exposure
        // (In real execution, this would be a market order to dump the excess)
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "yes",
            -3.0,     // Negative = selling/reducing position
            0.38,     // Might get worse price on the close
            0.0,
            "yes_close_order",
        ));

        let pos = tracker.get("TEST-MARKET").expect("Should have position");

        // After auto-close, both sides should have 7 contracts
        assert!(
            (pos.yes_token.contracts - 7.0).abs() < 0.01,
            "YES should be reduced to 7 contracts, got {}",
            pos.yes_token.contracts
        );
        assert!(
            (pos.no_token.contracts - 7.0).abs() < 0.01,
            "NO should still have 7 contracts, got {}",
            pos.no_token.contracts
        );
    }

    /// Test: After auto-closing excess NO, position should be balanced
    #[test]
    fn test_auto_close_no_excess_balances_position() {
        let mut tracker = PositionTracker::new();

        // Initial mismatched fill
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "no",
            10.0,
            0.50,
            0.18,
            "no_order_1",
        ));

        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "yes",
            6.0,
            0.40,
            0.0,
            "yes_order_1",
        ));

        // Simulate auto-close: SELL 4 NO to close exposure
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "no",
            -4.0,     // Negative = selling/reducing position
            0.48,     // Might get worse price on the close
            0.07,     // Still pay fees
            "no_close_order",
        ));

        let pos = tracker.get("TEST-MARKET").expect("Should have position");

        // After auto-close, both sides should have 6 contracts
        assert!(
            (pos.no_token.contracts - 6.0).abs() < 0.01,
            "NO should be reduced to 6 contracts, got {}",
            pos.no_token.contracts
        );
        assert!(
            (pos.yes_token.contracts - 6.0).abs() < 0.01,
            "YES should still have 6 contracts, got {}",
            pos.yes_token.contracts
        );
    }

    /// Test: Complete failure on one side creates full exposure
    /// (e.g., NO fills 10, YES fills 0)
    #[test]
    fn test_complete_one_side_failure_full_exposure() {
        let mut tracker = PositionTracker::new();

        // NO succeeds
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "no",
            10.0,
            0.50,
            0.18,
            "no_order_1",
        ));

        // YES completely fails - no fill recorded

        let pos = tracker.get("TEST-MARKET").expect("Should have position");

        // Full NO exposure - must be closed immediately
        assert!((pos.no_token.contracts - 10.0).abs() < 0.01);
        assert!((pos.yes_token.contracts - 0.0).abs() < 0.01);

        // This is a dangerous situation - 10 unhedged NO contracts
        // Auto-close should sell all 10 NO to eliminate exposure
    }

    /// Test: Auto-close after complete one-side failure
    #[test]
    fn test_auto_close_after_complete_failure() {
        let mut tracker = PositionTracker::new();

        // NO fills, YES fails completely
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "no",
            10.0,
            0.50,
            0.18,
            "no_order_1",
        ));

        // Auto-close: Sell ALL 10 NO contracts
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "no",
            -10.0,    // Sell everything
            0.45,     // Might get a bad price in emergency close
            0.18,     // More fees
            "no_close_order",
        ));

        let pos = tracker.get("TEST-MARKET").expect("Should have position");

        // Position should be flat (0 contracts on both sides)
        assert!(
            (pos.no_token.contracts - 0.0).abs() < 0.01,
            "NO should be 0 after emergency close, got {}",
            pos.no_token.contracts
        );
    }

    /// Test: Profit calculation with partial fill and auto-close
    #[test]
    fn test_profit_with_partial_fill_and_auto_close() {
        let mut tracker = PositionTracker::new();

        // Requested 10 contracts
        // NO fills 8 @ 50¢ (cost: $4.00 + 0.14 fees)
        // YES fills 10 @ 40¢ (cost: $4.00)
        // Need to close 2 excess YES @ 38¢ (receive: $0.76)

        // Initial fills
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "no",
            8.0,
            0.50,
            0.14,
            "no_order_1",
        ));

        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "yes",
            10.0,
            0.40,
            0.0,
            "yes_order_1",
        ));

        // Auto-close 2 excess YES
        tracker.record_fill(&FillRecord::new(
            "TEST-MARKET",
            "Test Match",
            "yes",
            -2.0,
            0.38,     // Sold at 38¢ (worse than 40¢ buy price)
            0.0,
            "yes_close_order",
        ));

        let pos = tracker.get("TEST-MARKET").expect("Should have position");

        // Net position: 8 matched contracts
        // NO: 8 @ 50¢ = $4.00 cost + $0.14 fees
        // YES: 8 @ ~40¢ = ~$3.20 cost (10*0.40 - 2*0.38 = 4.00 - 0.76 = 3.24 effective for 8)

        assert!(
            (pos.no_token.contracts - 8.0).abs() < 0.01,
            "Should have 8 matched NO"
        );
        assert!(
            (pos.yes_token.contracts - 8.0).abs() < 0.01,
            "Should have 8 matched YES"
        );

        // The matched 8 contracts have guaranteed profit:
        // $1.00 payout - $0.50 no - ~$0.405 yes = ~$0.095 per contract
        // But we also lost $0.02 per contract on the 2 we had to close (40¢ - 38¢)
    }
}

// ============================================================================
// PROCESS MOCK TESTS - Simulate execution flow without real APIs
// ============================================================================
// These tests verify that process correctly:
// 1. Records fills to the position tracker
// 2. Handles mismatched fills
// 3. Updates circuit breaker state
// 4. Captures order IDs

mod process_mock_tests {
    use arb_bot::types::*;
    use arb_bot::circuit_breaker::*;
    use arb_bot::position_tracker::*;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// Simulates what process does after execute_both_legs_async returns
    /// This allows testing the position tracking logic without real API clients
    struct MockExecutionResult {
        yes_filled: i64,
        no_filled: i64,
        yes_cost: i64,  // cents
        no_cost: i64,   // cents
        yes_order_id: String,
        no_order_id: String,
    }

    /// Simulates the position tracking logic from process
    async fn simulate_process_position_tracking(
        tracker: &Arc<RwLock<PositionTracker>>,
        circuit_breaker: &CircuitBreaker,
        pair: &MarketPair,
        _req: &FastExecutionRequest,
        result: MockExecutionResult,
    ) -> (i64, i16) {  // Returns (matched, profit_cents)
        let matched = result.yes_filled.min(result.no_filled);
        let actual_profit = matched as i16 * 100 - (result.yes_cost + result.no_cost) as i16;

        // Record success to circuit breaker
        if matched > 0 {
            circuit_breaker.record_success(&pair.pair_id, matched).await;
        }

        // === UPDATE POSITION TRACKER (mirrors process logic) ===
        if matched > 0 || result.yes_filled > 0 || result.no_filled > 0 {
            let mut tracker_guard = tracker.write().await;

            // Record YES fill
            if result.yes_filled > 0 {
                tracker_guard.record_fill(&FillRecord::new(
                    &pair.pair_id,
                    &pair.description,
                    "yes",
                    matched as f64,
                    result.yes_cost as f64 / 100.0 / result.yes_filled.max(1) as f64,
                    0.0,
                    &result.yes_order_id,
                ));
            }

            // Record NO fill
            if result.no_filled > 0 {
                tracker_guard.record_fill(&FillRecord::new(
                    &pair.pair_id,
                    &pair.description,
                    "no",
                    matched as f64,
                    result.no_cost as f64 / 100.0 / result.no_filled.max(1) as f64,
                    0.0,
                    &result.no_order_id,
                ));
            }

            tracker_guard.save_async();
        }

        (matched, actual_profit)
    }

    fn test_market_pair() -> MarketPair {
        MarketPair {
            pair_id: "process-fast-test".into(),
            league: "epl".into(),
            market_type: MarketType::Moneyline,
            description: "Process Fast Test Market".into(),
            yes_token_condition: "yes-condition-process".into(),
            no_token_condition: "no-condition-process".into(),
            poly_slug: "process-fast-test".into(),
            yes_token: "pf_yes_token".into(),
            no_token: "pf_no_token".into(),
            line_value: None,
            team_suffix: None,
            category: None,
            event_title: None,
        }
    }

    fn test_circuit_breaker_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            max_total_position: 500,
            max_consecutive_errors: 5,
            cooldown_secs: 60,
            enabled: true,
        }
    }

    /// Test: process records both fills to position tracker with correct order IDs
    #[tokio::test]
    async fn test_process_records_fills_with_order_ids() {
        let tracker = Arc::new(RwLock::new(PositionTracker::new()));
        let cb = CircuitBreaker::new(test_circuit_breaker_config());
        let pair = test_market_pair();

        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 50,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        let result = MockExecutionResult {
            yes_filled: 10,
            no_filled: 10,
            yes_cost: 400,  // 10 contracts x 40¢
            no_cost: 500,   // 10 contracts x 50¢
            yes_order_id: "yes_order_abc123".to_string(),
            no_order_id: "no_order_xyz789".to_string(),
        };

        let (matched, profit) = simulate_process_position_tracking(
            &tracker, &cb, &pair, &req, result
        ).await;

        // Verify matched contracts
        assert_eq!(matched, 10, "Should have 10 matched contracts");

        // Verify profit: 10 contracts x $1 payout - $4 YES - $5 NO = $1 = 100¢
        assert_eq!(profit, 100, "Should have 100¢ profit");

        // Verify position tracker was updated
        let tracker_guard = tracker.read().await;
        let summary = tracker_guard.summary();

        assert_eq!(summary.open_positions, 1, "Should have 1 open position");
        assert!(summary.total_contracts > 0.0, "Should have contracts recorded");

        // Verify the position has both legs recorded
        let pos = tracker_guard.get(&pair.pair_id).expect("Should have position");
        assert!(pos.yes_token.contracts > 0.0, "Should have YES contracts");
        assert!(pos.no_token.contracts > 0.0, "Should have NO contracts");
    }

    /// Test: process updates circuit breaker on success
    #[tokio::test]
    async fn test_process_updates_circuit_breaker() {
        let tracker = Arc::new(RwLock::new(PositionTracker::new()));
        let cb = CircuitBreaker::new(test_circuit_breaker_config());
        let pair = test_market_pair();

        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 50,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        let result = MockExecutionResult {
            yes_filled: 10,
            no_filled: 10,
            yes_cost: 400,
            no_cost: 500,
            yes_order_id: "y_order_3".to_string(),
            no_order_id: "n_order_3".to_string(),
        };

        simulate_process_position_tracking(&tracker, &cb, &pair, &req, result).await;

        // Verify circuit breaker was updated
        let status = cb.status().await;
        assert_eq!(status.consecutive_errors, 0, "Errors should be reset after success");
        assert!(status.total_position > 0, "Total position should be tracked");
    }

    /// Test: process handles partial NO fill correctly
    #[tokio::test]
    async fn test_process_partial_no_fill() {
        let tracker = Arc::new(RwLock::new(PositionTracker::new()));
        let cb = CircuitBreaker::new(test_circuit_breaker_config());
        let pair = test_market_pair();

        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 50,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // YES fills 10, NO only fills 6
        let result = MockExecutionResult {
            yes_filled: 10,
            no_filled: 6,
            yes_cost: 400,  // 10 x 40¢
            no_cost: 300,   // 6 x 50¢
            yes_order_id: "y_full".to_string(),
            no_order_id: "n_partial".to_string(),
        };

        let (matched, _profit) = simulate_process_position_tracking(
            &tracker, &cb, &pair, &req, result
        ).await;

        // Matched should be min(10, 6) = 6
        assert_eq!(matched, 6, "Matched should be min of both fills");
    }

    /// Test: process handles zero YES fill
    #[tokio::test]
    async fn test_process_zero_yes_fill() {
        let tracker = Arc::new(RwLock::new(PositionTracker::new()));
        let cb = CircuitBreaker::new(test_circuit_breaker_config());
        let pair = test_market_pair();

        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 50,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // YES fills 0, NO fills 10 (complete failure on one side)
        let result = MockExecutionResult {
            yes_filled: 0,
            no_filled: 10,
            yes_cost: 0,
            no_cost: 500,
            yes_order_id: "".to_string(),
            no_order_id: "n_only".to_string(),
        };

        let (matched, _profit) = simulate_process_position_tracking(
            &tracker, &cb, &pair, &req, result
        ).await;

        // No matched contracts since one side is 0
        assert_eq!(matched, 0, "No matched contracts when one side is 0");

        // But position tracker still records the NO fill (for exposure tracking)
        let tracker_guard = tracker.read().await;
        let _pos = tracker_guard.get(&pair.pair_id).expect("Should have position even with partial fills");
    }

    /// Test: process handles zero NO fill
    #[tokio::test]
    async fn test_process_zero_no_fill() {
        let tracker = Arc::new(RwLock::new(PositionTracker::new()));
        let cb = CircuitBreaker::new(test_circuit_breaker_config());
        let pair = test_market_pair();

        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 50,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // YES fills 10, NO fills 0
        let result = MockExecutionResult {
            yes_filled: 10,
            no_filled: 0,
            yes_cost: 400,
            no_cost: 0,
            yes_order_id: "y_only".to_string(),
            no_order_id: "".to_string(),
        };

        let (matched, _profit) = simulate_process_position_tracking(
            &tracker, &cb, &pair, &req, result
        ).await;

        assert_eq!(matched, 0, "No matched contracts when NO is 0");
    }

    /// Test: process correctly calculates profit with full fills
    #[tokio::test]
    async fn test_process_profit_calculation_full_fill() {
        let tracker = Arc::new(RwLock::new(PositionTracker::new()));
        let cb = CircuitBreaker::new(test_circuit_breaker_config());
        let pair = test_market_pair();

        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,  // YES at 40¢
            no_price: 50,   // NO at 50¢
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        let result = MockExecutionResult {
            yes_filled: 10,
            no_filled: 10,
            yes_cost: 400,  // 10 x 40¢ = $4.00 = 400¢
            no_cost: 500,   // 10 x 50¢ = $5.00 = 500¢
            yes_order_id: "y_profit".to_string(),
            no_order_id: "n_profit".to_string(),
        };

        let (matched, profit) = simulate_process_position_tracking(
            &tracker, &cb, &pair, &req, result
        ).await;

        // Profit = matched x $1 payout - costs
        // = 10 x 100¢ - 400¢ - 500¢
        // = 1000¢ - 900¢
        // = 100¢
        assert_eq!(matched, 10);
        assert_eq!(profit, 100, "Profit should be 100¢ ($1.00)");
    }

    /// Test: process correctly calculates profit with partial fill
    #[tokio::test]
    async fn test_process_profit_calculation_partial_fill() {
        let tracker = Arc::new(RwLock::new(PositionTracker::new()));
        let cb = CircuitBreaker::new(test_circuit_breaker_config());
        let pair = test_market_pair();

        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 50,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // Partial fill: YES 10, NO 7
        let result = MockExecutionResult {
            yes_filled: 10,
            no_filled: 7,
            yes_cost: 400,  // 10 x 40¢
            no_cost: 350,   // 7 x 50¢ (but only 7 are matched)
            yes_order_id: "y_partial_profit".to_string(),
            no_order_id: "n_partial_profit".to_string(),
        };

        let (matched, profit) = simulate_process_position_tracking(
            &tracker, &cb, &pair, &req, result
        ).await;

        // Profit = matched x $1 payout - ALL costs (including unmatched)
        // = 7 x 100¢ - 400¢ - 350¢
        // = 700¢ - 750¢
        // = -50¢ (LOSS because we paid for 10 YES but only matched 7!)
        assert_eq!(matched, 7);
        assert_eq!(profit, -50, "Should have -50¢ loss due to unmatched YES contracts");
    }

    /// Test: Multiple executions accumulate in position tracker
    #[tokio::test]
    async fn test_process_multiple_executions_accumulate() {
        let tracker = Arc::new(RwLock::new(PositionTracker::new()));
        let cb = CircuitBreaker::new(test_circuit_breaker_config());
        let pair = test_market_pair();

        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 50,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // First execution: 10 contracts
        let result1 = MockExecutionResult {
            yes_filled: 10,
            no_filled: 10,
            yes_cost: 400,
            no_cost: 500,
            yes_order_id: "y_exec_1".to_string(),
            no_order_id: "n_exec_1".to_string(),
        };

        simulate_process_position_tracking(&tracker, &cb, &pair, &req, result1).await;

        // Second execution: 5 more contracts
        let result2 = MockExecutionResult {
            yes_filled: 5,
            no_filled: 5,
            yes_cost: 200,
            no_cost: 250,
            yes_order_id: "y_exec_2".to_string(),
            no_order_id: "n_exec_2".to_string(),
        };

        simulate_process_position_tracking(&tracker, &cb, &pair, &req, result2).await;

        // Verify accumulated position
        let tracker_guard = tracker.read().await;
        let pos = tracker_guard.get(&pair.pair_id).expect("Should have position");

        // Should have 15 total contracts (10 + 5)
        assert!(
            (pos.yes_token.contracts - 15.0).abs() < 0.01,
            "Should have accumulated 15 YES contracts, got {}",
            pos.yes_token.contracts
        );
        assert!(
            (pos.no_token.contracts - 15.0).abs() < 0.01,
            "Should have accumulated 15 NO contracts, got {}",
            pos.no_token.contracts
        );
    }

    /// Test: Circuit breaker tracks accumulated position per market
    #[tokio::test]
    async fn test_circuit_breaker_accumulates_position() {
        let tracker = Arc::new(RwLock::new(PositionTracker::new()));
        let cb = CircuitBreaker::new(test_circuit_breaker_config());
        let pair = test_market_pair();

        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 50,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // Execute multiple times
        for i in 0..5 {
            let result = MockExecutionResult {
                yes_filled: 10,
                no_filled: 10,
                yes_cost: 400,
                no_cost: 500,
                yes_order_id: format!("y_cb_{}", i),
                no_order_id: format!("n_cb_{}", i),
            };

            simulate_process_position_tracking(&tracker, &cb, &pair, &req, result).await;
        }

        // Circuit breaker tracks contracts on BOTH sides (yes + no)
        // 5 executions x 10 matched x 2 sides = 100 total
        let status = cb.status().await;
        assert_eq!(status.total_position, 100, "Circuit breaker should track 100 contracts total (both sides)");
    }

    // =========================================================================
    // POLYMARKET-ONLY ARB TESTS
    // =========================================================================

    /// Test: PolyOnly arb (YES + NO tokens on Polymarket - zero fees)
    #[tokio::test]
    async fn test_process_poly_only_arb() {
        let tracker = Arc::new(RwLock::new(PositionTracker::new()));
        let cb = CircuitBreaker::new(test_circuit_breaker_config());
        let pair = test_market_pair();

        // PolyOnly: Buy YES and NO both on Polymarket
        // Profitable when YES + NO < $1
        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 48,  // YES at 48¢
            no_price: 50,   // NO at 50¢ (total = 98¢, 2¢ profit with NO fees!)
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // Both fills are from Polymarket
        let result = MockExecutionResult {
            yes_filled: 10,
            no_filled: 10,
            yes_cost: 480,  // 10 x 48¢
            no_cost: 500,   // 10 x 50¢
            yes_order_id: "yes_order".to_string(),
            no_order_id: "no_order".to_string(),
        };

        let (matched, profit) = simulate_process_position_tracking(
            &tracker, &cb, &pair, &req, result
        ).await;

        assert_eq!(matched, 10);
        // 10 x 100¢ - 480¢ - 500¢ = 1000 - 980 = 20¢
        assert_eq!(profit, 20);

        // Verify the ArbType has ZERO fees
        assert_eq!(req.estimated_fee_cents(), 0, "PolyOnly should have ZERO fees");
        assert_eq!(req.profit_cents(), 2, "PolyOnly profit = 100 - 48 - 50 - 0 = 2¢");
    }

    /// Test: PolyOnly fee calculation is always zero
    #[test]
    fn test_poly_only_zero_fees() {
        for yes_price in [10u16, 25, 50, 75, 90] {
            for no_price in [10u16, 25, 50, 75, 90] {
                let req = FastExecutionRequest {
                    market_id: 0,
                    yes_price,
                    no_price,
                    yes_size: 1000,
                    no_size: 1000,
                    arb_type: ArbType::PolyOnly,
                    detected_ns: 0,
                };
                assert_eq!(req.estimated_fee_cents(), 0,
                    "PolyOnly should always have 0 fees, got {} for prices ({}, {})",
                    req.estimated_fee_cents(), yes_price, no_price);
            }
        }
    }

    // NOTE: The system now only supports Polymarket-only arbitrage with YES/NO tokens.
}