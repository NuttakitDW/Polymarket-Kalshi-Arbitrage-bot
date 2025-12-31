//! Storage record types for SQLite persistence.

/// Record for market metadata (stored once per market on discovery)
#[derive(Debug, Clone)]
pub struct MarketMetadataRecord {
    pub pair_id: String,
    pub league: String,
    pub market_type: String,
    pub description: String,
    pub category: Option<String>,
    pub event_title: Option<String>,
    pub yes_token_address: String,
    pub no_token_address: String,
}

/// Record for an executed trade transaction
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub pair_id: String,
    pub timestamp_secs: i64,
    pub timestamp_ns: u32,

    // Order details
    pub yes_order_id: String,
    pub no_order_id: String,

    // Fill information
    pub yes_filled: i64,
    pub no_filled: i64,
    pub matched_contracts: i64,

    // Cost and profit (in cents)
    pub yes_cost_cents: i64,
    pub no_cost_cents: i64,
    pub total_cost_cents: i64,
    pub profit_cents: i64,

    // Prices at execution (in cents)
    pub yes_price: u16,
    pub no_price: u16,

    // Execution metadata
    pub latency_us: u64,
    pub success: bool,
    pub error: Option<String>,
    pub running_type: String,
}

/// Record for an arbitrage opportunity snapshot
#[derive(Debug, Clone)]
pub struct ArbSnapshotRecord {
    pub pair_id: String,
    pub timestamp_secs: i64,
    pub timestamp_ns: u32,

    // YES outcome orderbook
    pub yes_ask: u16,

    // NO outcome orderbook
    pub no_ask: u16,

    // Calculated values
    pub total_cost: u16,
    pub gap_cents: i16,
    /// Profit per contract in cents (e.g., 2 = 2¢ profit per $1 contract)
    pub profit_per_contract: u16,
    /// Maximum profit in cents based on available liquidity: profit_per_contract * min(yes_size, no_size) / 100
    pub max_profit_cents: u32,

    // Denormalized market metadata for easier queries
    pub description: Option<String>,
    pub event_title: Option<String>,
    /// Categories as JSON array (e.g., `["Politics", "Business"]`)
    pub categories: Option<String>,
    /// Running type: "DRY_RUN", "SIMULATE", or "REAL_MONEY"
    pub running_type: String,
}
