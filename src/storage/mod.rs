//! SQLite storage module for market data persistence.
//!
//! Provides non-blocking write operations for storing arbitrage opportunity
//! snapshots and market metadata for later analysis.

pub mod schema;
pub mod types;
pub mod writer;

pub use types::{ArbSnapshotRecord, MarketMetadataRecord, TradeRecord};
pub use writer::{create_storage_channel, StorageChannel};
