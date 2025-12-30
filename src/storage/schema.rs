//! Database schema creation and migrations.

use rusqlite::{Connection, Result};

/// Create all database tables and indexes.
pub fn create_tables(conn: &Connection) -> Result<()> {
    // Markets table: static metadata
    conn.execute(
        "CREATE TABLE IF NOT EXISTS markets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            pair_id TEXT UNIQUE NOT NULL,
            league TEXT NOT NULL,
            market_type TEXT NOT NULL,
            description TEXT NOT NULL,
            category TEXT,
            event_title TEXT,
            yes_token_address TEXT NOT NULL,
            no_token_address TEXT NOT NULL,
            created_at INTEGER NOT NULL
        )",
        [],
    )?;

    // Arb snapshots table: time-series data for arb opportunities only
    conn.execute(
        "CREATE TABLE IF NOT EXISTS arb_snapshots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            market_id INTEGER NOT NULL,
            timestamp INTEGER NOT NULL,
            timestamp_ns INTEGER NOT NULL,
            yes_ask INTEGER NOT NULL,
            yes_size INTEGER NOT NULL,
            no_ask INTEGER NOT NULL,
            no_size INTEGER NOT NULL,
            total_cost INTEGER NOT NULL,
            gap_cents INTEGER NOT NULL,
            profit_per_contract INTEGER NOT NULL,
            max_profit_cents INTEGER NOT NULL,
            description TEXT,
            event_title TEXT,
            categories TEXT,
            running_type TEXT,
            FOREIGN KEY (market_id) REFERENCES markets(id)
        )",
        [],
    )?;

    // Indexes for efficient queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_arb_time ON arb_snapshots(timestamp DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_arb_market_time ON arb_snapshots(market_id, timestamp DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_arb_gap ON arb_snapshots(gap_cents)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_markets_category ON markets(category)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_markets_pair_id ON markets(pair_id)",
        [],
    )?;

    // Migration: Add new columns if they don't exist (idempotent for existing databases)
    let _ = conn.execute(
        "ALTER TABLE arb_snapshots ADD COLUMN description TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE arb_snapshots ADD COLUMN event_title TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE arb_snapshots ADD COLUMN categories TEXT",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE arb_snapshots ADD COLUMN running_type TEXT",
        [],
    );

    // Index for category-based queries on arb_snapshots
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_arb_categories ON arb_snapshots(categories)",
        [],
    )?;

    // Index for running_type filtering
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_arb_running_type ON arb_snapshots(running_type)",
        [],
    )?;

    Ok(())
}
