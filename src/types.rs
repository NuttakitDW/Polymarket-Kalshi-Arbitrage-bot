//! Core type definitions and data structures for the arbitrage trading system.
//!
//! This module provides the foundational types for market state management,
//! orderbook representation, and arbitrage opportunity detection.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use rustc_hash::FxHashMap;

// === Market Types ===

/// Market category for a matched trading pair
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MarketType {
    /// Moneyline/outright winner market
    Moneyline,
    /// Point spread market
    Spread,
    /// Total/over-under market
    Total,
    /// Both teams to score market
    Btts,
}

impl std::fmt::Display for MarketType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarketType::Moneyline => write!(f, "moneyline"),
            MarketType::Spread => write!(f, "spread"),
            MarketType::Total => write!(f, "total"),
            MarketType::Btts => write!(f, "btts"),
        }
    }
}

/// A matched trading pair for Polymarket-only arbitrage (competing outcomes)
///
/// For Polymarket-only mode, this represents two competing outcomes from the same event.
/// Example: "Chelsea YES" vs "Arsenal YES" in a match with multiple outcomes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketPair {
    /// Unique identifier for this market pair
    pub pair_id: Arc<str>,
    /// Sports league identifier (e.g., "epl", "nba")
    pub league: Arc<str>,
    /// Type of market (moneyline, spread, total, etc.)
    pub market_type: MarketType,
    /// Human-readable market description
    pub description: Arc<str>,
    /// Polymarket YES token condition_id
    pub yes_token_condition: Arc<str>,
    /// Polymarket NO token condition_id
    pub no_token_condition: Arc<str>,
    /// Polymarket market slug
    pub poly_slug: Arc<str>,
    /// Polymarket YES token address
    pub yes_token: Arc<str>,
    /// Polymarket NO token address
    pub no_token: Arc<str>,
    /// Line value for spread/total markets (if applicable)
    pub line_value: Option<f64>,
    /// Team suffix for team-specific markets
    pub team_suffix: Option<Arc<str>>,
    /// Category/series from Polymarket events (e.g., "Fed Emergency Rate Cut", "nuclear test")
    pub category: Option<Arc<str>>,
    /// Event title from Polymarket for additional context
    pub event_title: Option<Arc<str>>,
}

/// Price representation in cents (1-99 for $0.01-$0.99), 0 indicates no price available
pub type PriceCents = u16;

/// Size representation in cents (dollar amount × 100), maximum ~$42.9M per side
pub type SizeCents = u32;

/// Maximum number of concurrently tracked markets
/// Increased to handle pagination (can fetch up to 20K markets from Gamma API)
pub const MAX_MARKETS: usize = 8192;

/// Sentinel value indicating no price is currently available
pub const NO_PRICE: PriceCents = 0;

/// Pack prices into a single u32 for atomic operations.
/// Bit layout: [yes_ask:16][no_ask:16]
#[inline(always)]
pub fn pack_prices(yes_ask: PriceCents, no_ask: PriceCents) -> u32 {
    ((yes_ask as u32) << 16) | (no_ask as u32)
}

/// Unpack a u32 prices representation back into its component values
#[inline(always)]
pub fn unpack_prices(packed: u32) -> (PriceCents, PriceCents) {
    let yes_ask = ((packed >> 16) & 0xFFFF) as PriceCents;
    let no_ask = (packed & 0xFFFF) as PriceCents;
    (yes_ask, no_ask)
}

/// Pack sizes into a single u64 for atomic operations.
/// Bit layout: [yes_size:32][no_size:32]
#[inline(always)]
pub fn pack_sizes(yes_size: SizeCents, no_size: SizeCents) -> u64 {
    ((yes_size as u64) << 32) | (no_size as u64)
}

/// Unpack a u64 sizes representation back into its component values
#[inline(always)]
pub fn unpack_sizes(packed: u64) -> (SizeCents, SizeCents) {
    let yes_size = ((packed >> 32) & 0xFFFFFFFF) as SizeCents;
    let no_size = (packed & 0xFFFFFFFF) as SizeCents;
    (yes_size, no_size)
}

/// Lock-free orderbook state for a single trading platform.
/// Uses atomic operations for thread-safe, zero-copy price updates.
/// Split into two atomics to support u32 sizes (up to ~$42.9M per side).
#[repr(align(64))]
pub struct AtomicOrderbook {
    /// Packed prices: [yes_ask:16][no_ask:16]
    prices: AtomicU32,
    /// Packed sizes: [yes_size:32][no_size:32]
    sizes: AtomicU64,
}

impl AtomicOrderbook {
    pub const fn new() -> Self {
        Self {
            prices: AtomicU32::new(0),
            sizes: AtomicU64::new(0),
        }
    }

    /// Load current state
    #[inline(always)]
    pub fn load(&self) -> (PriceCents, PriceCents, SizeCents, SizeCents) {
        let (yes_ask, no_ask) = unpack_prices(self.prices.load(Ordering::Acquire));
        let (yes_size, no_size) = unpack_sizes(self.sizes.load(Ordering::Acquire));
        (yes_ask, no_ask, yes_size, no_size)
    }

    /// Store new state
    #[inline(always)]
    #[allow(dead_code)]
    pub fn store(&self, yes_ask: PriceCents, no_ask: PriceCents, yes_size: SizeCents, no_size: SizeCents) {
        self.prices.store(pack_prices(yes_ask, no_ask), Ordering::Release);
        self.sizes.store(pack_sizes(yes_size, no_size), Ordering::Release);
    }

    /// Update YES side only
    #[inline(always)]
    pub fn update_yes(&self, yes_ask: PriceCents, yes_size: SizeCents) {
        // Update prices atomically
        let mut current_prices = self.prices.load(Ordering::Acquire);
        loop {
            let (_, no_ask) = unpack_prices(current_prices);
            let new_prices = pack_prices(yes_ask, no_ask);
            match self.prices.compare_exchange_weak(current_prices, new_prices, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(c) => current_prices = c,
            }
        }
        // Update sizes atomically
        let mut current_sizes = self.sizes.load(Ordering::Acquire);
        loop {
            let (_, no_size) = unpack_sizes(current_sizes);
            let new_sizes = pack_sizes(yes_size, no_size);
            match self.sizes.compare_exchange_weak(current_sizes, new_sizes, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(c) => current_sizes = c,
            }
        }
    }

    /// Update NO side only
    #[inline(always)]
    #[allow(dead_code)]
    pub fn update_no(&self, no_ask: PriceCents, no_size: SizeCents) {
        // Update prices atomically
        let mut current_prices = self.prices.load(Ordering::Acquire);
        loop {
            let (yes_ask, _) = unpack_prices(current_prices);
            let new_prices = pack_prices(yes_ask, no_ask);
            match self.prices.compare_exchange_weak(current_prices, new_prices, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(c) => current_prices = c,
            }
        }
        // Update sizes atomically
        let mut current_sizes = self.sizes.load(Ordering::Acquire);
        loop {
            let (yes_size, _) = unpack_sizes(current_sizes);
            let new_sizes = pack_sizes(yes_size, no_size);
            match self.sizes.compare_exchange_weak(current_sizes, new_sizes, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(c) => current_sizes = c,
            }
        }
    }
}

impl Default for AtomicOrderbook {
    fn default() -> Self {
        Self::new()
    }
}

/// Complete market state tracking orderbooks for competing outcomes in Polymarket-only arbitrage
///
/// For Polymarket-only mode, both orderbooks represent different Polymarket tokens:
/// - `yes_book`: Orderbook for YES token
/// - `no_book`: Orderbook for NO token
pub struct AtomicMarketState {
    /// YES token orderbook state
    pub yes_book: AtomicOrderbook,
    /// NO token orderbook state
    pub no_book: AtomicOrderbook,
    /// Market pair metadata (immutable after discovery phase)
    pub pair: Option<Arc<MarketPair>>,
    /// Unique market identifier for O(1) lookups
    pub market_id: u16,
}

impl AtomicMarketState {
    pub fn new(market_id: u16) -> Self {
        Self {
            yes_book: AtomicOrderbook::new(),
            no_book: AtomicOrderbook::new(),
            pair: None,
            market_id,
        }
    }

    /// Check for arbitrage opportunities.
    ///
    /// In Polymarket-only mode:
    /// - yes_price = YES token price
    /// - no_price = NO token price
    ///
    /// Returns bitmask: bit 2 = Poly-only arb (YES + NO < threshold)
    #[inline(always)]
    pub fn check_arbs(&self, threshold_cents: PriceCents) -> u8 {
        let (yes_price, _, _, _) = self.yes_book.load();
        let (no_price, _, _, _) = self.no_book.load();

        if yes_price == NO_PRICE || no_price == NO_PRICE {
            return 0;
        }

        // Polymarket-only arbitrage: YES + NO (competing outcomes, NO FEES!)
        let total_cost = yes_price + no_price;

        if total_cost < threshold_cents {
            4  // bit 2 = Poly-only arb
        } else {
            0
        }
    }
}

/// Convert f64 price (0.01-0.99) to PriceCents (1-99)
#[inline(always)]
pub fn price_to_cents(price: f64) -> PriceCents {
    ((price * 100.0).round() as PriceCents).clamp(0, 99)
}

/// Convert PriceCents back to f64
#[inline(always)]
pub fn cents_to_price(cents: PriceCents) -> f64 {
    cents as f64 / 100.0
}

/// Parse price from string "0.XX" format (Polymarket)
/// Returns 0 if parsing fails
#[inline(always)]
pub fn parse_price(s: &str) -> PriceCents {
    let bytes = s.as_bytes();
    // Handle "0.XX" format (4 chars)
    if bytes.len() == 4 && bytes[0] == b'0' && bytes[1] == b'.' {
        let d1 = bytes[2].wrapping_sub(b'0');
        let d2 = bytes[3].wrapping_sub(b'0');
        if d1 < 10 && d2 < 10 {
            return (d1 as u16 * 10 + d2 as u16) as PriceCents;
        }
    }
    // Handle "0.X" format (3 chars) for prices like 0.5
    if bytes.len() == 3 && bytes[0] == b'0' && bytes[1] == b'.' {
        let d = bytes[2].wrapping_sub(b'0');
        if d < 10 {
            return (d as u16 * 10) as PriceCents;
        }
    }
    // Fallback to standard parse
    s.parse::<f64>()
        .map(|p| price_to_cents(p))
        .unwrap_or(0)
}

/// Arbitrage opportunity type, determining the execution strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArbType {
    /// Same-platform: Buy Polymarket YES + Buy Polymarket NO (competing outcomes)
    PolyOnly,
}

/// High-priority execution request for an arbitrage opportunity
#[derive(Debug, Clone, Copy)]
pub struct FastExecutionRequest {
    /// Market identifier (index into GlobalState.markets array)
    pub market_id: u16,
    /// YES outcome ask price in cents
    pub yes_price: PriceCents,
    /// NO outcome ask price in cents
    pub no_price: PriceCents,
    /// YES outcome available size in cents
    pub yes_size: SizeCents,
    /// NO outcome available size in cents
    pub no_size: SizeCents,
    /// Arbitrage type (determines execution strategy)
    pub arb_type: ArbType,
    /// Detection timestamp in nanoseconds since system start
    pub detected_ns: u64,
}

impl FastExecutionRequest {
    #[inline(always)]
    pub fn profit_cents(&self) -> i16 {
        100 - (self.yes_price as i16 + self.no_price as i16 + self.estimated_fee_cents() as i16)
    }

    #[inline(always)]
    pub fn estimated_fee_cents(&self) -> PriceCents {
        match self.arb_type {
            // Poly-only: no fees
            ArbType::PolyOnly => 0,
        }
    }
}

/// Global market state manager for all tracked Polymarket markets
pub struct GlobalState {
    /// Market states indexed by market_id for O(1) access
    pub markets: Vec<AtomicMarketState>,

    /// Next available market identifier (monotonically increasing)
    next_market_id: u16,

    /// O(1) lookup map: pre-hashed YES token condition → market_id
    pub yes_token_to_id: FxHashMap<u64, u16>,

    /// O(1) lookup map: pre-hashed YES token address → market_id
    pub yes_addr_to_id: FxHashMap<u64, u16>,

    /// O(1) lookup map: pre-hashed NO token address → market_id
    pub no_addr_to_id: FxHashMap<u64, u16>,
}

impl GlobalState {
    pub fn new() -> Self {
        // Allocate market slots
        let markets: Vec<AtomicMarketState> = (0..MAX_MARKETS)
            .map(|i| AtomicMarketState::new(i as u16))
            .collect();

        Self {
            markets,
            next_market_id: 0,
            yes_token_to_id: FxHashMap::default(),
            yes_addr_to_id: FxHashMap::default(),
            no_addr_to_id: FxHashMap::default(),
        }
    }

    /// Add a market pair, returns market_id
    pub fn add_pair(&mut self, pair: MarketPair) -> Option<u16> {
        if self.next_market_id as usize >= MAX_MARKETS {
            return None;
        }

        let market_id = self.next_market_id;
        self.next_market_id += 1;

        // Pre-compute hashes
        let yes_condition_hash = fxhash_str(&pair.yes_token_condition);
        let yes_addr_hash = fxhash_str(&pair.yes_token);
        let no_addr_hash = fxhash_str(&pair.no_token);

        // Update lookup maps
        self.yes_token_to_id.insert(yes_condition_hash, market_id);
        self.yes_addr_to_id.insert(yes_addr_hash, market_id);
        self.no_addr_to_id.insert(no_addr_hash, market_id);

        // Store pair
        self.markets[market_id as usize].pair = Some(Arc::new(pair));

        Some(market_id)
    }

    /// Get market by YES token condition hash (O(1))
    #[inline(always)]
    #[allow(dead_code)]
    pub fn get_by_yes_token_hash(&self, hash: u64) -> Option<&AtomicMarketState> {
        let id = *self.yes_token_to_id.get(&hash)?;
        Some(&self.markets[id as usize])
    }

    /// Get market by YES address hash (O(1))
    #[inline(always)]
    #[allow(dead_code)]
    pub fn get_by_yes_addr_hash(&self, hash: u64) -> Option<&AtomicMarketState> {
        let id = *self.yes_addr_to_id.get(&hash)?;
        Some(&self.markets[id as usize])
    }

    /// Get market by NO address hash (O(1))
    #[inline(always)]
    #[allow(dead_code)]
    pub fn get_by_no_addr_hash(&self, hash: u64) -> Option<&AtomicMarketState> {
        let id = *self.no_addr_to_id.get(&hash)?;
        Some(&self.markets[id as usize])
    }

    /// Get market_id by YES address hash
    #[inline(always)]
    #[allow(dead_code)]
    pub fn id_by_yes_addr_hash(&self, hash: u64) -> Option<u16> {
        self.yes_addr_to_id.get(&hash).copied()
    }

    /// Get market_id by NO address hash
    #[inline(always)]
    #[allow(dead_code)]
    pub fn id_by_no_addr_hash(&self, hash: u64) -> Option<u16> {
        self.no_addr_to_id.get(&hash).copied()
    }

    /// Get market_id by YES token condition hash
    #[inline(always)]
    #[allow(dead_code)]
    pub fn id_by_yes_token_hash(&self, hash: u64) -> Option<u16> {
        self.yes_token_to_id.get(&hash).copied()
    }

    /// Get market by ID
    #[inline(always)]
    pub fn get_by_id(&self, id: u16) -> Option<&AtomicMarketState> {
        if (id as usize) < self.markets.len() {
            Some(&self.markets[id as usize])
        } else {
            None
        }
    }

    pub fn market_count(&self) -> usize {
        self.next_market_id as usize
    }
}

impl Default for GlobalState {
    fn default() -> Self {
        Self::new()
    }
}

/// Fast string hashing function using FxHash for O(1) lookups
#[inline(always)]
pub fn fxhash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    s.hash(&mut hasher);
    hasher.finish()
}

// === Platform Enum ===

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Platform {
    Polymarket,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Platform::Polymarket => write!(f, "POLYMARKET"),
        }
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    // =========================================================================
    // Pack/Unpack Tests - Verify bit manipulation correctness
    // =========================================================================

    #[test]
    fn test_pack_unpack_prices_roundtrip() {
        // Test various price values pack and unpack correctly
        let test_cases = vec![
            (50, 50),   // Common mid-price
            (1, 99),    // Edge prices
            (99, 1),    // Reversed edge
            (0, 0),     // All zeros
            (NO_PRICE, NO_PRICE),  // No prices
        ];

        for (yes_ask, no_ask) in test_cases {
            let packed = pack_prices(yes_ask, no_ask);
            let (y, n) = unpack_prices(packed);
            assert_eq!((y, n), (yes_ask, no_ask),
                "Prices roundtrip failed for ({}, {})", yes_ask, no_ask);
        }
    }

    #[test]
    fn test_pack_unpack_sizes_roundtrip() {
        // Test various size values pack and unpack correctly (now u32)
        let test_cases: Vec<(SizeCents, SizeCents)> = vec![
            (1000, 1000),           // Common size
            (100, 100),             // Small size
            (100_000, 100_000),     // $1000 in cents (was overflow before!)
            (1_000_000, 1_000_000), // $10,000 in cents
            (u32::MAX, u32::MAX),   // Max sizes (~$42.9M)
            (0, 0),                 // All zeros
        ];

        for (yes_size, no_size) in test_cases {
            let packed = pack_sizes(yes_size, no_size);
            let (ys, ns) = unpack_sizes(packed);
            assert_eq!((ys, ns), (yes_size, no_size),
                "Sizes roundtrip failed for ({}, {})", yes_size, no_size);
        }
    }

    #[test]
    fn test_pack_prices_bit_layout() {
        // Verify the exact bit layout: [yes_ask:16][no_ask:16]
        let packed = pack_prices(0xABCD, 0x1234);

        assert_eq!((packed >> 16) & 0xFFFF, 0xABCD, "yes_ask should be in bits 16-31");
        assert_eq!(packed & 0xFFFF, 0x1234, "no_ask should be in bits 0-15");
    }

    #[test]
    fn test_pack_sizes_bit_layout() {
        // Verify the exact bit layout: [yes_size:32][no_size:32]
        let packed = pack_sizes(0xABCD1234, 0x56789ABC);

        assert_eq!((packed >> 32) & 0xFFFFFFFF, 0xABCD1234, "yes_size should be in bits 32-63");
        assert_eq!(packed & 0xFFFFFFFF, 0x56789ABC, "no_size should be in bits 0-31");
    }

    // =========================================================================
    // AtomicOrderbook Tests
    // =========================================================================

    #[test]
    fn test_atomic_orderbook_store_load() {
        let book = AtomicOrderbook::new();

        // Initially all zeros
        let (y, n, ys, ns) = book.load();
        assert_eq!((y, n, ys, ns), (0, 0, 0, 0));

        // Store and load
        book.store(45, 55, 500, 600);
        let (y, n, ys, ns) = book.load();
        assert_eq!((y, n, ys, ns), (45, 55, 500, 600));
    }

    #[test]
    fn test_atomic_orderbook_update_yes() {
        let book = AtomicOrderbook::new();

        // Set initial state
        book.store(40, 60, 100, 200);

        // Update only YES side
        book.update_yes(42, 150);

        let (y, n, ys, ns) = book.load();
        assert_eq!(y, 42, "YES ask should be updated");
        assert_eq!(ys, 150, "YES size should be updated");
        assert_eq!(n, 60, "NO ask should be unchanged");
        assert_eq!(ns, 200, "NO size should be unchanged");
    }

    #[test]
    fn test_atomic_orderbook_update_no() {
        let book = AtomicOrderbook::new();

        // Set initial state
        book.store(40, 60, 100, 200);

        // Update only NO side
        book.update_no(58, 250);

        let (y, n, ys, ns) = book.load();
        assert_eq!(y, 40, "YES ask should be unchanged");
        assert_eq!(ys, 100, "YES size should be unchanged");
        assert_eq!(n, 58, "NO ask should be updated");
        assert_eq!(ns, 250, "NO size should be updated");
    }

    #[test]
    fn test_atomic_orderbook_concurrent_updates() {
        // Verify correctness under concurrent access
        let book = Arc::new(AtomicOrderbook::new());
        book.store(50, 50, 1000, 1000);

        let handles: Vec<_> = (0..4).map(|i| {
            let book = book.clone();
            thread::spawn(move || {
                for _ in 0..1000 {
                    if i % 2 == 0 {
                        book.update_yes(45 + (i as u16), 500);
                    } else {
                        book.update_no(55 + (i as u16), 500);
                    }
                }
            })
        }).collect();

        for h in handles {
            h.join().unwrap();
        }

        // State should be consistent (not corrupted)
        let (y, n, ys, ns) = book.load();
        assert!(y > 0 && y < 100, "YES ask should be valid");
        assert!(n > 0 && n < 100, "NO ask should be valid");
        assert_eq!(ys, 500, "YES size should be consistent");
        assert_eq!(ns, 500, "NO size should be consistent");
    }

    // =========================================================================
    // Price Conversion Tests
    // =========================================================================

    #[test]
    fn test_price_to_cents() {
        assert_eq!(price_to_cents(0.50), 50);
        assert_eq!(price_to_cents(0.01), 1);
        assert_eq!(price_to_cents(0.99), 99);
        assert_eq!(price_to_cents(0.0), 0);
        assert_eq!(price_to_cents(1.0), 99);  // Clamped to 99
        assert_eq!(price_to_cents(0.505), 51);  // Rounded
        assert_eq!(price_to_cents(0.504), 50);  // Rounded
    }

    #[test]
    fn test_cents_to_price() {
        assert!((cents_to_price(50) - 0.50).abs() < 0.001);
        assert!((cents_to_price(1) - 0.01).abs() < 0.001);
        assert!((cents_to_price(99) - 0.99).abs() < 0.001);
        assert!((cents_to_price(0) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_price() {
        // Standard "0.XX" format
        assert_eq!(parse_price("0.50"), 50);
        assert_eq!(parse_price("0.01"), 1);
        assert_eq!(parse_price("0.99"), 99);

        // "0.X" format
        assert_eq!(parse_price("0.5"), 50);

        // Fallback parsing
        assert_eq!(parse_price("0.505"), 51);

        // Invalid input
        assert_eq!(parse_price("invalid"), 0);
        assert_eq!(parse_price(""), 0);
    }

    // =========================================================================
    // check_arbs Tests (Polymarket-only mode)
    // =========================================================================

    /// Create market state for Polymarket-only arbitrage testing.
    /// - yes_book = YES token price
    /// - no_book = NO token price
    fn make_market_state(
        yes_price: PriceCents,
        no_price: PriceCents,
    ) -> AtomicMarketState {
        let state = AtomicMarketState::new(0);
        state.yes_book.store(yes_price, 0, 1000, 0);
        state.no_book.store(no_price, 0, 1000, 0);
        state
    }

    #[test]
    fn test_check_arbs_poly_only_detected() {
        // YES 40¢ + NO 48¢ = 88¢ → ARB (no fees!)
        let state = make_market_state(40, 48);

        let mask = state.check_arbs(100);

        assert!(mask & 4 != 0, "Should detect Poly-only arb (bit 2)");
        assert_eq!(mask, 4, "Only Poly-only arb should be detected");
    }

    #[test]
    fn test_check_arbs_no_arb_efficient_market() {
        // YES 52¢ + NO 52¢ = 104¢ → NO ARB (> 100¢)
        let state = make_market_state(52, 52);

        let mask = state.check_arbs(100);

        assert_eq!(mask, 0, "Should detect no arbs in efficient market");
    }

    #[test]
    fn test_check_arbs_missing_yes_price() {
        // Missing YES price should return no arbs
        let state = make_market_state(NO_PRICE, 50);

        let mask = state.check_arbs(100);

        assert_eq!(mask, 0, "Should return 0 when YES price is missing");
    }

    #[test]
    fn test_check_arbs_missing_no_price() {
        // Missing NO price should return no arbs
        let state = make_market_state(50, NO_PRICE);

        let mask = state.check_arbs(100);

        assert_eq!(mask, 0, "Should return 0 when NO price is missing");
    }

    #[test]
    fn test_check_arbs_marginal_no_arb() {
        // YES 50¢ + NO 50¢ = 100¢ exactly → NO ARB (not < 100¢)
        let state = make_market_state(50, 50);

        let mask = state.check_arbs(100);

        assert_eq!(mask, 0, "100¢ exactly should not be an arb (need < threshold)");
    }

    #[test]
    fn test_check_arbs_marginal_arb() {
        // YES 49¢ + NO 50¢ = 99¢ → ARB (< 100¢)
        let state = make_market_state(49, 50);

        let mask = state.check_arbs(100);

        assert!(mask & 4 != 0, "99¢ should be detected as arb");
    }

    #[test]
    fn test_check_arbs_large_profit() {
        // YES 30¢ + NO 30¢ = 60¢ → Big ARB (40¢ profit!)
        let state = make_market_state(30, 30);

        let mask = state.check_arbs(100);

        assert!(mask & 4 != 0, "Large price discrepancy should be detected");
    }

    // =========================================================================
    // GlobalState Tests
    // =========================================================================

    fn make_test_pair(id: &str) -> MarketPair {
        MarketPair {
            pair_id: id.into(),
            league: "epl".into(),
            market_type: MarketType::Moneyline,
            description: format!("Test Market {}", id).into(),
            yes_token_condition: format!("condition-{}", id).into(),
            no_token_condition: format!("condition-{}-no", id).into(),
            poly_slug: format!("test-{}", id).into(),
            yes_token: format!("yes_token_{}", id).into(),
            no_token: format!("no_token_{}", id).into(),
            line_value: None,
            team_suffix: None,
            category: None,
            event_title: None,
        }
    }

    #[test]
    fn test_global_state_add_pair() {
        let mut state = GlobalState::new();

        let pair = make_test_pair("001");
        let yes_condition = pair.yes_token_condition.clone();
        let yes_addr = pair.yes_token.clone();
        let no_addr = pair.no_token.clone();

        let id = state.add_pair(pair).expect("Should add pair");

        assert_eq!(id, 0, "First market should have id 0");
        assert_eq!(state.market_count(), 1);

        // Verify lookups work
        let yes_condition_hash = fxhash_str(&yes_condition);
        let yes_addr_hash = fxhash_str(&yes_addr);
        let no_addr_hash = fxhash_str(&no_addr);

        assert!(state.yes_token_to_id.contains_key(&yes_condition_hash));
        assert!(state.yes_addr_to_id.contains_key(&yes_addr_hash));
        assert!(state.no_addr_to_id.contains_key(&no_addr_hash));
    }

    #[test]
    fn test_global_state_lookups() {
        let mut state = GlobalState::new();

        let pair = make_test_pair("002");
        let yes_condition = pair.yes_token_condition.clone();
        let yes_addr = pair.yes_token.clone();

        let id = state.add_pair(pair).unwrap();

        // Test get_by_id
        let market = state.get_by_id(id).expect("Should find by id");
        assert!(market.pair.is_some());

        // Test get_by_yes_token_hash
        let market = state.get_by_yes_token_hash(fxhash_str(&yes_condition))
            .expect("Should find by YES condition hash");
        assert!(market.pair.is_some());

        // Test get_by_yes_addr_hash
        let market = state.get_by_yes_addr_hash(fxhash_str(&yes_addr))
            .expect("Should find by YES address hash");
        assert!(market.pair.is_some());

        // Test id lookups
        assert_eq!(state.id_by_yes_token_hash(fxhash_str(&yes_condition)), Some(id));
        assert_eq!(state.id_by_yes_addr_hash(fxhash_str(&yes_addr)), Some(id));
    }

    #[test]
    fn test_global_state_multiple_markets() {
        let mut state = GlobalState::new();

        // Add multiple markets
        for i in 0..10 {
            let pair = make_test_pair(&format!("{:03}", i));
            let id = state.add_pair(pair).unwrap();
            assert_eq!(id, i as u16);
        }

        assert_eq!(state.market_count(), 10);

        // All should be findable
        for i in 0..10 {
            let market = state.get_by_id(i as u16);
            assert!(market.is_some(), "Market {} should exist", i);
        }
    }

    #[test]
    fn test_global_state_update_prices() {
        let mut state = GlobalState::new();

        let pair = make_test_pair("003");
        let id = state.add_pair(pair).unwrap();

        // Update YES token prices
        let market = state.get_by_id(id).unwrap();
        market.yes_book.store(45, 55, 500, 600);

        // Update NO token prices
        market.no_book.store(44, 56, 700, 800);

        // Verify prices
        let (yes_ask, yes_bid, yes_sz, yes_bid_sz) = market.yes_book.load();
        assert_eq!((yes_ask, yes_bid, yes_sz, yes_bid_sz), (45, 55, 500, 600));

        let (no_ask, no_bid, no_sz, no_bid_sz) = market.no_book.load();
        assert_eq!((no_ask, no_bid, no_sz, no_bid_sz), (44, 56, 700, 800));
    }

    // =========================================================================
    // FastExecutionRequest Tests (Polymarket-only mode)
    // =========================================================================

    #[test]
    fn test_execution_request_profit_cents_poly_only() {
        // Poly Token A 40¢ + Poly Token B 48¢ = 88¢
        // No fees on Polymarket
        // Profit = 100 - 88 - 0 = 12¢
        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 48,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        assert_eq!(req.profit_cents(), 12);
        assert_eq!(req.estimated_fee_cents(), 0);
    }

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

        assert!(req.profit_cents() < 0, "Should have negative profit");
    }

    #[test]
    fn test_execution_request_poly_only_no_fees() {
        // PolyOnly → no fees
        let req = FastExecutionRequest {
            market_id: 0,
            yes_price: 40,
            no_price: 50,
            yes_size: 1000,
            no_size: 1000,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };
        assert_eq!(req.estimated_fee_cents(), 0);
    }

    // =========================================================================
    // fxhash_str Tests
    // =========================================================================

    #[test]
    fn test_fxhash_str_consistency() {
        let s = "yes_token_12345";

        // Same string should always produce same hash
        let h1 = fxhash_str(s);
        let h2 = fxhash_str(s);
        assert_eq!(h1, h2);

        // Different strings should (almost certainly) produce different hashes
        let h3 = fxhash_str("no_token_12345");
        assert_ne!(h1, h3);
    }

    // =========================================================================
    // Integration: Full Arb Detection Flow
    // =========================================================================

    #[test]
    fn test_full_arb_flow() {
        // Simulate the full flow: add market, update prices, detect arb (Polymarket-only)
        let mut state = GlobalState::new();

        // 1. Add market during discovery
        let pair = MarketPair {
            pair_id: "test-arb".into(),
            league: "epl".into(),
            market_type: MarketType::Moneyline,
            description: "Chelsea vs Arsenal".into(),
            yes_token_condition: "yes-condition".into(),
            no_token_condition: "no-condition".into(),
            poly_slug: "chelsea-vs-arsenal".into(),
            yes_token: "yes_token_addr".into(),
            no_token: "no_token_addr".into(),
            line_value: None,
            team_suffix: Some("CFC".into()),
            category: None,
            event_title: None,
        };

        let yes_token = pair.yes_token.clone();
        let no_token = pair.no_token.clone();

        let market_id = state.add_pair(pair).unwrap();

        // 2. Simulate WebSocket updates for both Polymarket tokens
        let yes_hash = fxhash_str(&yes_token);
        if let Some(id) = state.yes_addr_to_id.get(&yes_hash) {
            // YES = 40¢
            state.markets[*id as usize].yes_book.store(40, 0, 500, 0);
        }

        let no_hash = fxhash_str(&no_token);
        if let Some(id) = state.no_addr_to_id.get(&no_hash) {
            // NO = 48¢
            state.markets[*id as usize].no_book.store(48, 0, 700, 0);
        }

        // 3. Check for arbs (threshold = 100 cents = $1.00)
        // YES (40¢) + NO (48¢) = 88¢ < 100¢ → ARB!
        let market = state.get_by_id(market_id).unwrap();
        let arb_mask = market.check_arbs(100);

        // 4. Verify arb detected (bit 2 = Poly-only arb)
        assert!(arb_mask & 4 != 0, "Should detect Poly-only arb (YES + NO < 100¢)");

        // 5. Build execution request
        let (yes_price, _, yes_sz, _) = market.yes_book.load();
        let (no_price, _, no_sz, _) = market.no_book.load();

        let req = FastExecutionRequest {
            market_id,
            yes_price,
            no_price,
            yes_size: yes_sz,
            no_size: no_sz,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // Profit = 100 - 40 - 48 - 0 (no fees) = 12¢
        assert_eq!(req.profit_cents(), 12, "Should have 12¢ profit");
    }

    #[test]
    fn test_price_update_race_condition() {
        // Simulate concurrent price updates from different WebSocket feeds
        let state = Arc::new(GlobalState::default());

        // Pre-populate with a market
        let market = &state.markets[0];
        market.yes_book.store(50, 50, 1000, 1000);
        market.no_book.store(50, 50, 1000, 1000);

        let handles: Vec<_> = (0..4).map(|i| {
            let state = state.clone();
            thread::spawn(move || {
                for j in 0..1000 {
                    let market = &state.markets[0];
                    if i % 2 == 0 {
                        // Simulate YES token updates
                        market.yes_book.update_yes(40 + ((j % 10) as u16), 500 + j as u32);
                    } else {
                        // Simulate NO token updates
                        market.no_book.update_no(50 + ((j % 10) as u16), 600 + j as u32);
                    }

                    // Check arbs (should never panic) - threshold = 100 cents
                    let _ = market.check_arbs(100);
                }
            })
        }).collect();

        for h in handles {
            h.join().unwrap();
        }

        // Final state should be valid
        let market = &state.markets[0];
        let (yes_ask, yes_bid, _, _) = market.yes_book.load();
        let (no_ask, no_bid, _, _) = market.no_book.load();

        assert!(yes_ask > 0 && yes_ask < 100);
        assert!(yes_bid > 0 && yes_bid < 100);
        assert!(no_ask > 0 && no_ask < 100);
        assert!(no_bid > 0 && no_bid < 100);
    }

    // =========================================================================
    // YES/NO Size Independence Tests - Verify sizes are stored separately
    // =========================================================================

    #[test]
    fn test_yes_no_sizes_stored_independently() {
        // This test verifies that YES and NO sizes are stored in separate orderbooks
        // and don't interfere with each other (addressing the bug where sizes were same)

        let market = AtomicMarketState::new(0);

        // Store different sizes for YES and NO
        let yes_price = 45_u16;
        let yes_size = 1500_u32;  // $15.00 in cents
        let no_price = 55_u16;
        let no_size = 2500_u32;   // $25.00 in cents

        // Simulate what polymarket.rs does:
        // YES token update goes to yes_book
        market.yes_book.update_yes(yes_price, yes_size);

        // NO token update goes to no_book (using update_yes because it stores in position 0)
        market.no_book.update_yes(no_price, no_size);

        // Now load and verify - simulating what send_arb_request does
        let (loaded_yes_price, _, loaded_yes_size, _) = market.yes_book.load();
        let (loaded_no_price, _, loaded_no_size, _) = market.no_book.load();

        // Verify prices are correct
        assert_eq!(loaded_yes_price, yes_price, "YES price should match");
        assert_eq!(loaded_no_price, no_price, "NO price should match");

        // CRITICAL: Verify sizes are different (this is the bug check)
        assert_eq!(loaded_yes_size, yes_size, "YES size should be 1500");
        assert_eq!(loaded_no_size, no_size, "NO size should be 2500");
        assert_ne!(loaded_yes_size, loaded_no_size,
            "YES and NO sizes MUST be different! YES={} NO={}", loaded_yes_size, loaded_no_size);
    }

    #[test]
    fn test_updating_yes_size_does_not_affect_no_size() {
        let market = AtomicMarketState::new(0);

        // Set initial state for both
        market.yes_book.update_yes(40, 1000);
        market.no_book.update_yes(60, 2000);

        // Update YES size only
        market.yes_book.update_yes(42, 1500);

        // Verify NO size is unchanged
        let (_, _, no_size, _) = market.no_book.load();
        assert_eq!(no_size, 2000, "NO size should be unchanged after YES update");

        let (_, _, yes_size, _) = market.yes_book.load();
        assert_eq!(yes_size, 1500, "YES size should be updated to 1500");
    }

    #[test]
    fn test_updating_no_size_does_not_affect_yes_size() {
        let market = AtomicMarketState::new(0);

        // Set initial state for both
        market.yes_book.update_yes(40, 1000);
        market.no_book.update_yes(60, 2000);

        // Update NO size only
        market.no_book.update_yes(58, 3000);

        // Verify YES size is unchanged
        let (_, _, yes_size, _) = market.yes_book.load();
        assert_eq!(yes_size, 1000, "YES size should be unchanged after NO update");

        let (_, _, no_size, _) = market.no_book.load();
        assert_eq!(no_size, 3000, "NO size should be updated to 3000");
    }

    #[test]
    fn test_fast_execution_request_receives_different_sizes() {
        // Simulate the full flow from storage to FastExecutionRequest creation
        let market = AtomicMarketState::new(0);

        // Simulate BookSnapshot updates with different sizes
        market.yes_book.update_yes(48, 800);   // YES: $8.00 available
        market.no_book.update_yes(50, 1200);   // NO: $12.00 available

        // Load sizes the way send_arb_request does
        let (yes_price, _, yes_size, _) = market.yes_book.load();
        let (no_price, _, no_size, _) = market.no_book.load();

        // Create execution request (simplified)
        let req = FastExecutionRequest {
            market_id: 0,
            yes_price,
            no_price,
            yes_size,
            no_size,
            arb_type: ArbType::PolyOnly,
            detected_ns: 0,
        };

        // Verify the request has different sizes
        assert_eq!(req.yes_size, 800, "Request YES size should be 800");
        assert_eq!(req.no_size, 1200, "Request NO size should be 1200");
        assert_ne!(req.yes_size, req.no_size,
            "Request sizes MUST be different! YES={} NO={}", req.yes_size, req.no_size);

        // Verify max_from_liquidity calculation uses minimum
        let max_from_liquidity = (req.yes_size.min(req.no_size) / 100) as i64;
        assert_eq!(max_from_liquidity, 8, "Should use min(800,1200)/100 = 8 contracts");
    }

    #[test]
    fn test_size_edge_cases() {
        let market = AtomicMarketState::new(0);

        // Test with zero sizes
        market.yes_book.update_yes(50, 0);
        market.no_book.update_yes(50, 1000);

        let (_, _, yes_size, _) = market.yes_book.load();
        let (_, _, no_size, _) = market.no_book.load();

        assert_eq!(yes_size, 0, "YES size should be 0");
        assert_eq!(no_size, 1000, "NO size should be 1000");

        // Test with very large sizes
        market.yes_book.update_yes(50, 100_000_000);  // $1M
        market.no_book.update_yes(50, 50_000_000);    // $500K

        let (_, _, yes_size, _) = market.yes_book.load();
        let (_, _, no_size, _) = market.no_book.load();

        assert_eq!(yes_size, 100_000_000, "YES size should handle $1M");
        assert_eq!(no_size, 50_000_000, "NO size should handle $500K");
        assert_ne!(yes_size, no_size, "Large sizes should also be different");
    }
}

// === Polymarket/Gamma API Types ===

/// Tag/category information from Polymarket events
#[derive(Debug, Clone, Deserialize)]
pub struct GammaTag {
    /// Tag label (e.g., "Tech", "Politics", "Business")
    pub label: Option<String>,
}

/// Market object nested within an event (from /events endpoint)
#[derive(Debug, Clone, Deserialize)]
pub struct GammaEventMarket {
    pub slug: Option<String>,
    pub question: Option<String>,
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: Option<String>,
    pub outcomes: Option<String>,
    pub active: Option<bool>,
    pub closed: Option<bool>,
}

/// Top-level event from /events endpoint (contains tags + markets)
#[derive(Debug, Clone, Deserialize)]
pub struct GammaEventResponse {
    pub title: Option<String>,
    /// Tags array with real categories like "Tech", "Politics", "Business"
    pub tags: Option<Vec<GammaTag>>,
    /// Markets nested under this event
    pub markets: Option<Vec<GammaEventMarket>>,
}

// === Discovery Result ===

#[derive(Debug, Default)]
pub struct DiscoveryResult {
    pub pairs: Vec<MarketPair>,
    pub poly_matches: usize,
    pub errors: Vec<String>,
}