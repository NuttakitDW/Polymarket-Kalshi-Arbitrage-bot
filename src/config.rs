//! System configuration and league mapping definitions.
//!
//! This module contains all configuration constants, league mappings, and
//! environment variable parsing for the trading system.

/// Polymarket WebSocket URL
pub const POLYMARKET_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

/// Gamma API base URL (Polymarket market data)
pub const GAMMA_API_BASE: &str = "https://gamma-api.polymarket.com";

/// Arb threshold: alert when total cost < this (e.g., 0.995 = 0.5% profit)
pub const ARB_THRESHOLD: f64 = 0.995;

/// Maximum portfolio size in cents (e.g., 1500 = $15.00)
/// This caps the total position size per trade (YES + NO legs combined)
pub const MAX_PORTFOLIO_CENTS: u32 = 1500;

/// Polymarket ping interval (seconds) - keep connection alive
pub const POLY_PING_INTERVAL_SECS: u64 = 30;

/// WebSocket reconnect delay (seconds)
pub const WS_RECONNECT_DELAY_SECS: u64 = 5;

/// Which leagues to monitor (empty slice = all)
pub const ENABLED_LEAGUES: &[&str] = &[];

/// Price logging enabled (set PRICE_LOGGING=1 to enable)
#[allow(dead_code)]
pub fn price_logging_enabled() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("PRICE_LOGGING")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false)
    })
}

/// Store all arb opportunities to DB (set STORE_ALL_ARB=1 to enable)
/// When enabled, stores ALL detected arbs regardless of order value threshold.
/// Useful for verifying storage logic before live trading.
/// Default: false (only store arbs that meet $1 minimum order value)
pub fn store_all_arb_enabled() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("STORE_ALL_ARB")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false)
    })
}

/// Debug order sizes (set DEBUG_SIZES=1 to enable)
/// When enabled, logs detailed size information at each pipeline stage:
/// - BookSnapshot receipt: raw size from WebSocket
/// - Storage: size stored in yes_book/no_book
/// - Execution: sizes in FastExecutionRequest
/// Default: false
pub fn debug_sizes_enabled() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("DEBUG_SIZES")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false)
    })
}