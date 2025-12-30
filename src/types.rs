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
    /// [REPURPOSED] Polymarket Token A condition_id (was kalshi_event_ticker)
    pub kalshi_event_ticker: Arc<str>,
    /// [REPURPOSED] Polymarket Token A condition_id (was kalshi_market_ticker)
    pub kalshi_market_ticker: Arc<str>,
    /// Polymarket Token B condition_id (or market slug)
    pub poly_slug: Arc<str>,
    /// Polymarket Token A address
    pub poly_yes_token: Arc<str>,
    /// Polymarket Token B address
    pub poly_no_token: Arc<str>,
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
/// - `kalshi`: Orderbook for Polymarket Token A (e.g., "Chelsea YES")
/// - `poly`: Orderbook for Polymarket Token B (e.g., "Arsenal YES")
pub struct AtomicMarketState {
    /// [REPURPOSED] Polymarket Token A orderbook state (was Kalshi platform)
    pub kalshi: AtomicOrderbook,
    /// Polymarket Token B orderbook state
    pub poly: AtomicOrderbook,
    /// Market pair metadata (immutable after discovery phase)
    pub pair: Option<Arc<MarketPair>>,
    /// Unique market identifier for O(1) lookups
    pub market_id: u16,
}

impl AtomicMarketState {
    pub fn new(market_id: u16) -> Self {
        Self {
            kalshi: AtomicOrderbook::new(),
            poly: AtomicOrderbook::new(),
            pair: None,
            market_id,
        }
    }

    /// Check for arbitrage opportunities.
    ///
    /// In Polymarket-only mode:
    /// - k_yes = Token A YES price (stored in kalshi.yes_ask)
    /// - p_yes = Token B YES price (stored in poly.yes_ask)
    /// - k_no and p_no are NOT used (always 0)
    ///
    /// Returns bitmask: bit 2 = Poly-only arb (Token A YES + Token B YES < threshold)
    #[inline(always)]
    pub fn check_arbs(&self, threshold_cents: PriceCents) -> u8 {
        let (k_yes, _k_no, _, _) = self.kalshi.load();  // Token A YES
        let (p_yes, _p_no, _, _) = self.poly.load();    // Token B YES

        // For Polymarket-only mode: only check Token A YES + Token B YES
        // (k_no and p_no are never set in this mode)
        if k_yes == NO_PRICE || p_yes == NO_PRICE {
            return 0;
        }

        // Poly-only arbitrage: Token A YES + Token B YES (competing outcomes, NO FEES!)
        let poly_only_cost = k_yes + p_yes;

        if poly_only_cost < threshold_cents {
            4  // bit 2 = Poly-only arb
        } else {
            0
        }
    }
}

/// Precomputed Kalshi trading fee lookup table (101 entries for prices 0-100 cents).
/// Fee formula: ceil(0.07 × P × (1-P)) in cents, where P is price in cents.
static KALSHI_FEE_TABLE: [u16; 101] = {
    let mut table = [0u16; 101];
    let mut p = 1u32;
    while p < 100 {
        // fee = ceil(7 × p × (100-p) / 10000)
        let numerator = 7 * p * (100 - p) + 9999;
        table[p as usize] = (numerator / 10000) as u16;
        p += 1;
    }
    table
};

/// Calculate Kalshi trading fee in cents for a single contract at the given price.
/// For typical prices (10-90 cents), fees are usually 1-2 cents per contract.
#[inline(always)]
#[allow(dead_code)]
pub fn kalshi_fee_cents(price_cents: PriceCents) -> PriceCents {
    if price_cents > 100 {
        return 0;
    }
    KALSHI_FEE_TABLE[price_cents as usize]
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

/// Global market state manager for all tracked markets across both platforms
pub struct GlobalState {
    /// Market states indexed by market_id for O(1) access
    pub markets: Vec<AtomicMarketState>,

    /// Next available market identifier (monotonically increasing)
    next_market_id: u16,

    /// O(1) lookup map: pre-hashed Kalshi ticker → market_id
    pub kalshi_to_id: FxHashMap<u64, u16>,

    /// O(1) lookup map: pre-hashed Polymarket YES token → market_id
    pub poly_yes_to_id: FxHashMap<u64, u16>,

    /// O(1) lookup map: pre-hashed Polymarket NO token → market_id
    pub poly_no_to_id: FxHashMap<u64, u16>,
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
            kalshi_to_id: FxHashMap::default(),
            poly_yes_to_id: FxHashMap::default(),
            poly_no_to_id: FxHashMap::default(),
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
        let kalshi_hash = fxhash_str(&pair.kalshi_market_ticker);
        let poly_yes_hash = fxhash_str(&pair.poly_yes_token);
        let poly_no_hash = fxhash_str(&pair.poly_no_token);

        // Update lookup maps
        self.kalshi_to_id.insert(kalshi_hash, market_id);
        self.poly_yes_to_id.insert(poly_yes_hash, market_id);
        self.poly_no_to_id.insert(poly_no_hash, market_id);

        // Store pair
        self.markets[market_id as usize].pair = Some(Arc::new(pair));

        Some(market_id)
    }

    /// Get market by Kalshi ticker hash (O(1))
    #[inline(always)]
    #[allow(dead_code)]
    pub fn get_by_kalshi_hash(&self, hash: u64) -> Option<&AtomicMarketState> {
        let id = *self.kalshi_to_id.get(&hash)?;
        Some(&self.markets[id as usize])
    }

    /// Get market by Poly YES token hash (O(1))
    #[inline(always)]
    #[allow(dead_code)]
    pub fn get_by_poly_yes_hash(&self, hash: u64) -> Option<&AtomicMarketState> {
        let id = *self.poly_yes_to_id.get(&hash)?;
        Some(&self.markets[id as usize])
    }

    /// Get market by Poly NO token hash (O(1))
    #[inline(always)]
    #[allow(dead_code)]
    pub fn get_by_poly_no_hash(&self, hash: u64) -> Option<&AtomicMarketState> {
        let id = *self.poly_no_to_id.get(&hash)?;
        Some(&self.markets[id as usize])
    }

    /// Get market_id by Poly YES token hash
    #[inline(always)]
    #[allow(dead_code)]
    pub fn id_by_poly_yes_hash(&self, hash: u64) -> Option<u16> {
        self.poly_yes_to_id.get(&hash).copied()
    }

    /// Get market_id by Poly NO token hash
    #[inline(always)]
    #[allow(dead_code)]
    pub fn id_by_poly_no_hash(&self, hash: u64) -> Option<u16> {
        self.poly_no_to_id.get(&hash).copied()
    }

    /// Get market_id by Kalshi ticker hash
    #[inline(always)]
    #[allow(dead_code)]
    pub fn id_by_kalshi_hash(&self, hash: u64) -> Option<u16> {
        self.kalshi_to_id.get(&hash).copied()
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
    Kalshi,
    Polymarket,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Platform::Kalshi => write!(f, "KALSHI"),
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
    // kalshi_fee_cents Tests - Integer fee calculation
    // =========================================================================

    #[test]
    fn test_kalshi_fee_cents_formula() {
        // fee = ceil(7 × P × (100-P) / 10000) cents

        // At 50 cents: ceil(7 * 50 * 50 / 10000) = ceil(1.75) = 2
        assert_eq!(kalshi_fee_cents(50), 2);

        // At 10 cents: ceil(7 * 10 * 90 / 10000) = ceil(0.63) = 1
        assert_eq!(kalshi_fee_cents(10), 1);

        // At 90 cents: ceil(7 * 90 * 10 / 10000) = ceil(0.63) = 1
        assert_eq!(kalshi_fee_cents(90), 1);

        // At 1 cent: ceil(7 * 1 * 99 / 10000) = ceil(0.0693) = 1
        assert_eq!(kalshi_fee_cents(1), 1);

        // At 99 cents: ceil(7 * 99 * 1 / 10000) = ceil(0.0693) = 1
        assert_eq!(kalshi_fee_cents(99), 1);
    }

    #[test]
    fn test_kalshi_fee_cents_edge_cases() {
        // 0 and 100 should have no fee
        assert_eq!(kalshi_fee_cents(0), 0);
        assert_eq!(kalshi_fee_cents(100), 0);

        // Values > 100 should also return 0
        assert_eq!(kalshi_fee_cents(150), 0);
    }

    #[test]
    fn test_kalshi_fee_cents_matches_float_formula() {
        // Verify integer formula matches float formula for all valid prices
        for price_cents in 1..100u16 {
            let p = price_cents as f64 / 100.0;
            let float_fee = (0.07 * p * (1.0 - p) * 100.0).ceil() as u16;
            let int_fee = kalshi_fee_cents(price_cents);

            // Allow 1 cent difference due to rounding differences
            assert!(
                (int_fee as i16 - float_fee as i16).abs() <= 1,
                "Fee mismatch at {}¢: int={}, float={}", price_cents, int_fee, float_fee
            );
        }
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
    /// In this mode:
    /// - kalshi.yes = Token A price (competing outcome 1)
    /// - poly.yes = Token B price (competing outcome 2)
    fn make_market_state(
        token_a_price: PriceCents,
        token_b_price: PriceCents,
    ) -> AtomicMarketState {
        let state = AtomicMarketState::new(0);
        // Token A stored in kalshi.yes (repurposed)
        state.kalshi.store(token_a_price, 0, 1000, 0);
        // Token B stored in poly.yes
        state.poly.store(token_b_price, 0, 1000, 0);
        state
    }

    #[test]
    fn test_check_arbs_poly_only_detected() {
        // Token A 40¢ + Token B 48¢ = 88¢ → ARB (no fees!)
        let state = make_market_state(40, 48);

        let mask = state.check_arbs(100);

        assert!(mask & 4 != 0, "Should detect Poly-only arb (bit 2)");
        assert_eq!(mask, 4, "Only Poly-only arb should be detected");
    }

    #[test]
    fn test_check_arbs_no_arb_efficient_market() {
        // Token A 52¢ + Token B 52¢ = 104¢ → NO ARB (> 100¢)
        let state = make_market_state(52, 52);

        let mask = state.check_arbs(100);

        assert_eq!(mask, 0, "Should detect no arbs in efficient market");
    }

    #[test]
    fn test_check_arbs_missing_token_a_price() {
        // Missing Token A price should return no arbs
        let state = make_market_state(NO_PRICE, 50);

        let mask = state.check_arbs(100);

        assert_eq!(mask, 0, "Should return 0 when Token A price is missing");
    }

    #[test]
    fn test_check_arbs_missing_token_b_price() {
        // Missing Token B price should return no arbs
        let state = make_market_state(50, NO_PRICE);

        let mask = state.check_arbs(100);

        assert_eq!(mask, 0, "Should return 0 when Token B price is missing");
    }

    #[test]
    fn test_check_arbs_marginal_no_arb() {
        // Token A 50¢ + Token B 50¢ = 100¢ exactly → NO ARB (not < 100¢)
        let state = make_market_state(50, 50);

        let mask = state.check_arbs(100);

        assert_eq!(mask, 0, "100¢ exactly should not be an arb (need < threshold)");
    }

    #[test]
    fn test_check_arbs_marginal_arb() {
        // Token A 49¢ + Token B 50¢ = 99¢ → ARB (< 100¢)
        let state = make_market_state(49, 50);

        let mask = state.check_arbs(100);

        assert!(mask & 4 != 0, "99¢ should be detected as arb");
    }

    #[test]
    fn test_check_arbs_large_profit() {
        // Token A 30¢ + Token B 30¢ = 60¢ → Big ARB (40¢ profit!)
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
            kalshi_event_ticker: format!("KXEPLGAME-{}", id).into(),
            kalshi_market_ticker: format!("KXEPLGAME-{}-YES", id).into(),
            poly_slug: format!("test-{}", id).into(),
            poly_yes_token: format!("yes_token_{}", id).into(),
            poly_no_token: format!("no_token_{}", id).into(),
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
        let kalshi_ticker = pair.kalshi_market_ticker.clone();
        let poly_yes = pair.poly_yes_token.clone();
        let poly_no = pair.poly_no_token.clone();

        let id = state.add_pair(pair).expect("Should add pair");

        assert_eq!(id, 0, "First market should have id 0");
        assert_eq!(state.market_count(), 1);

        // Verify lookups work
        let kalshi_hash = fxhash_str(&kalshi_ticker);
        let poly_yes_hash = fxhash_str(&poly_yes);
        let poly_no_hash = fxhash_str(&poly_no);

        assert!(state.kalshi_to_id.contains_key(&kalshi_hash));
        assert!(state.poly_yes_to_id.contains_key(&poly_yes_hash));
        assert!(state.poly_no_to_id.contains_key(&poly_no_hash));
    }

    #[test]
    fn test_global_state_lookups() {
        let mut state = GlobalState::new();

        let pair = make_test_pair("002");
        let kalshi_ticker = pair.kalshi_market_ticker.clone();
        let poly_yes = pair.poly_yes_token.clone();

        let id = state.add_pair(pair).unwrap();

        // Test get_by_id
        let market = state.get_by_id(id).expect("Should find by id");
        assert!(market.pair.is_some());

        // Test get_by_kalshi_hash
        let market = state.get_by_kalshi_hash(fxhash_str(&kalshi_ticker))
            .expect("Should find by Kalshi hash");
        assert!(market.pair.is_some());

        // Test get_by_poly_yes_hash
        let market = state.get_by_poly_yes_hash(fxhash_str(&poly_yes))
            .expect("Should find by Poly YES hash");
        assert!(market.pair.is_some());

        // Test id lookups
        assert_eq!(state.id_by_kalshi_hash(fxhash_str(&kalshi_ticker)), Some(id));
        assert_eq!(state.id_by_poly_yes_hash(fxhash_str(&poly_yes)), Some(id));
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

        // Update Kalshi prices
        let market = state.get_by_id(id).unwrap();
        market.kalshi.store(45, 55, 500, 600);

        // Update Poly prices
        market.poly.store(44, 56, 700, 800);

        // Verify prices
        let (k_yes, k_no, k_yes_sz, k_no_sz) = market.kalshi.load();
        assert_eq!((k_yes, k_no, k_yes_sz, k_no_sz), (45, 55, 500, 600));

        let (p_yes, p_no, p_yes_sz, p_no_sz) = market.poly.load();
        assert_eq!((p_yes, p_no, p_yes_sz, p_no_sz), (44, 56, 700, 800));
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
        let s = "KXEPLGAME-25DEC27CFCARS-CFC";

        // Same string should always produce same hash
        let h1 = fxhash_str(s);
        let h2 = fxhash_str(s);
        assert_eq!(h1, h2);

        // Different strings should (almost certainly) produce different hashes
        let h3 = fxhash_str("KXEPLGAME-25DEC27CFCARS-ARS");
        assert_ne!(h1, h3);
    }

    // =========================================================================
    // Integration: Full Arb Detection Flow
    // =========================================================================

    #[test]
    fn test_full_arb_flow() {
        // Simulate the full flow: add market, update prices, detect arb (Polymarket-only)
        let mut state = GlobalState::new();

        // 1. Add market during discovery (competing outcomes in same event)
        let pair = MarketPair {
            pair_id: "test-arb".into(),
            league: "epl".into(),
            market_type: MarketType::Moneyline,
            description: "Chelsea vs Arsenal".into(),
            kalshi_event_ticker: "token-a-condition".into(),
            kalshi_market_ticker: "token-a-condition".into(),
            poly_slug: "chelsea-vs-arsenal".into(),
            poly_yes_token: "token_a_chelsea".into(),  // Token A (Chelsea)
            poly_no_token: "token_b_arsenal".into(),    // Token B (Arsenal)
            line_value: None,
            team_suffix: Some("CFC".into()),
            category: None,
            event_title: None,
        };

        let poly_yes_token = pair.poly_yes_token.clone();
        let poly_no_token = pair.poly_no_token.clone();

        let market_id = state.add_pair(pair).unwrap();

        // 2. Simulate WebSocket updates for both Polymarket tokens
        // Token A (Chelsea) price update - stored in kalshi field (repurposed)
        let poly_yes_hash = fxhash_str(&poly_yes_token);
        if let Some(id) = state.poly_yes_to_id.get(&poly_yes_hash) {
            // Token A YES = 40¢
            state.markets[*id as usize].kalshi.store(40, 0, 500, 0);
        }

        // Token B (Arsenal) price update - stored in poly field
        let poly_no_hash = fxhash_str(&poly_no_token);
        if let Some(id) = state.poly_no_to_id.get(&poly_no_hash) {
            // Token B YES = 48¢
            state.markets[*id as usize].poly.store(48, 0, 700, 0);
        }

        // 3. Check for arbs (threshold = 100 cents = $1.00)
        // Token A (40¢) + Token B (48¢) = 88¢ < 100¢ → ARB!
        let market = state.get_by_id(market_id).unwrap();
        let arb_mask = market.check_arbs(100);

        // 4. Verify arb detected (bit 2 = Poly-only arb)
        assert!(arb_mask & 4 != 0, "Should detect Poly-only arb (Token A + Token B < 100¢)");

        // 5. Build execution request
        let (token_a_price, _, token_a_sz, _) = market.kalshi.load();
        let (token_b_price, _, token_b_sz, _) = market.poly.load();

        let req = FastExecutionRequest {
            market_id,
            yes_price: token_a_price,
            no_price: token_b_price,
            yes_size: token_a_sz,
            no_size: token_b_sz,
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
        market.kalshi.store(50, 50, 1000, 1000);
        market.poly.store(50, 50, 1000, 1000);

        let handles: Vec<_> = (0..4).map(|i| {
            let state = state.clone();
            thread::spawn(move || {
                for j in 0..1000 {
                    let market = &state.markets[0];
                    if i % 2 == 0 {
                        // Simulate Kalshi updates
                        market.kalshi.update_yes(40 + ((j % 10) as u16), 500 + j as u32);
                    } else {
                        // Simulate Poly updates
                        market.poly.update_no(50 + ((j % 10) as u16), 600 + j as u32);
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
        let (k_yes, k_no, _, _) = market.kalshi.load();
        let (p_yes, p_no, _, _) = market.poly.load();

        assert!(k_yes > 0 && k_yes < 100);
        assert!(k_no > 0 && k_no < 100);
        assert!(p_yes > 0 && p_yes < 100);
        assert!(p_no > 0 && p_no < 100);
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