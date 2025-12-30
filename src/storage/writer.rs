//! Non-blocking SQLite writer using a dedicated thread and mpsc channel.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use tracing::{error, info, warn};

use super::schema::create_tables;
use super::types::{ArbSnapshotRecord, MarketMetadataRecord, TradeRecord};

/// Messages sent to the storage writer thread.
pub enum StorageMessage {
    /// Register a new market (metadata)
    NewMarket(MarketMetadataRecord),
    /// Record an arbitrage opportunity snapshot
    ArbSnapshot(ArbSnapshotRecord),
    /// Record an executed trade transaction
    Trade(TradeRecord),
    /// Graceful shutdown
    #[allow(dead_code)]
    Shutdown,
}

/// Channel handle for sending storage messages (non-blocking).
#[derive(Clone)]
pub struct StorageChannel {
    tx: Sender<StorageMessage>,
    /// Running type for this session: "DRY_RUN", "SIMULATE", or "REAL_MONEY"
    pub running_type: String,
}

impl StorageChannel {
    /// Record market metadata (call once per market on discovery).
    pub fn record_market(&self, market: MarketMetadataRecord) {
        let _ = self.tx.send(StorageMessage::NewMarket(market));
    }

    /// Record an arbitrage opportunity snapshot.
    pub fn record_arb(&self, snapshot: ArbSnapshotRecord) {
        let _ = self.tx.send(StorageMessage::ArbSnapshot(snapshot));
    }

    /// Record an executed trade transaction.
    pub fn record_trade(&self, trade: TradeRecord) {
        let _ = self.tx.send(StorageMessage::Trade(trade));
    }

    /// Request graceful shutdown.
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        let _ = self.tx.send(StorageMessage::Shutdown);
    }
}

/// Create a storage channel and spawn the writer thread.
///
/// Returns a `StorageChannel` that can be cloned and shared across tasks.
/// `running_type` should be one of: "DRY_RUN", "SIMULATE", or "REAL_MONEY"
pub fn create_storage_channel(db_path: &str, running_type: &str) -> StorageChannel {
    let (tx, rx) = mpsc::channel();
    let path = db_path.to_string();

    // Spawn dedicated writer thread (isolated from async runtime)
    thread::spawn(move || {
        storage_writer_loop(rx, &path);
    });

    StorageChannel {
        tx,
        running_type: running_type.to_string(),
    }
}

/// Main writer loop running in a dedicated thread.
fn storage_writer_loop(rx: Receiver<StorageMessage>, db_path: &str) {
    // Open database connection
    let conn = match Connection::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            error!("[STORAGE] Failed to open database at {}: {}", db_path, e);
            return;
        }
    };

    // Create tables if needed
    if let Err(e) = create_tables(&conn) {
        error!("[STORAGE] Failed to create tables: {}", e);
        return;
    }

    info!("[STORAGE] Database initialized at {}", db_path);

    // Cache for pair_id -> market_id mapping
    let mut market_ids: HashMap<String, i64> = HashMap::new();

    // Load existing market IDs from database
    if let Ok(mut stmt) = conn.prepare("SELECT id, pair_id FROM markets") {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        }) {
            for row in rows.flatten() {
                market_ids.insert(row.1, row.0);
            }
        }
    }

    // Batch buffer for efficient writes
    let mut batch: Vec<StorageMessage> = Vec::with_capacity(100);
    let batch_timeout = Duration::from_millis(100);

    loop {
        match rx.recv_timeout(batch_timeout) {
            Ok(StorageMessage::Shutdown) => {
                // Flush remaining batch before shutdown
                if !batch.is_empty() {
                    flush_batch(&conn, &mut batch, &mut market_ids);
                }
                info!("[STORAGE] Writer shutdown complete");
                break;
            }
            Ok(msg) => {
                batch.push(msg);
                // Flush when batch is full
                if batch.len() >= 100 {
                    flush_batch(&conn, &mut batch, &mut market_ids);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Flush partial batch on timeout
                if !batch.is_empty() {
                    flush_batch(&conn, &mut batch, &mut market_ids);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Channel closed, flush and exit
                if !batch.is_empty() {
                    flush_batch(&conn, &mut batch, &mut market_ids);
                }
                info!("[STORAGE] Channel disconnected, writer exiting");
                break;
            }
        }
    }
}

/// Flush a batch of messages to the database in a single transaction.
fn flush_batch(
    conn: &Connection,
    batch: &mut Vec<StorageMessage>,
    market_ids: &mut HashMap<String, i64>,
) {
    if batch.is_empty() {
        return;
    }

    let tx = match conn.unchecked_transaction() {
        Ok(t) => t,
        Err(e) => {
            error!("[STORAGE] Failed to start transaction: {}", e);
            batch.clear();
            return;
        }
    };

    let mut arb_count = 0;
    let mut market_count = 0;
    let mut trade_count = 0;

    for msg in batch.drain(..) {
        match msg {
            StorageMessage::NewMarket(m) => {
                if insert_market(&tx, &m, market_ids) {
                    market_count += 1;
                }
            }
            StorageMessage::ArbSnapshot(s) => {
                if insert_arb_snapshot(&tx, &s, market_ids) {
                    arb_count += 1;
                }
            }
            StorageMessage::Trade(t) => {
                if insert_trade(&tx, &t, market_ids) {
                    trade_count += 1;
                }
            }
            StorageMessage::Shutdown => {}
        }
    }

    if let Err(e) = tx.commit() {
        error!("[STORAGE] Failed to commit transaction: {}", e);
    } else if arb_count > 0 || market_count > 0 || trade_count > 0 {
        info!(
            "[STORAGE] Flushed {} arb snapshots, {} trades, {} new markets",
            arb_count, trade_count, market_count
        );
    }
}

/// Insert market metadata, returns true if successful.
fn insert_market(
    conn: &Connection,
    market: &MarketMetadataRecord,
    market_ids: &mut HashMap<String, i64>,
) -> bool {
    // Skip if already in cache
    if market_ids.contains_key(&market.pair_id) {
        return false;
    }

    let now = chrono::Utc::now().timestamp();

    let result = conn.execute(
        "INSERT OR IGNORE INTO markets (pair_id, league, market_type, description, category, event_title, yes_token_address, no_token_address, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            market.pair_id,
            market.league,
            market.market_type,
            market.description,
            market.category,
            market.event_title,
            market.yes_token_address,
            market.no_token_address,
            now,
        ],
    );

    match result {
        Ok(rows) if rows > 0 => {
            // Get the inserted ID
            let id = conn.last_insert_rowid();
            market_ids.insert(market.pair_id.clone(), id);
            true
        }
        Ok(_) => {
            // Already exists, fetch ID
            if let Ok(id) = conn.query_row(
                "SELECT id FROM markets WHERE pair_id = ?1",
                [&market.pair_id],
                |row| row.get::<_, i64>(0),
            ) {
                market_ids.insert(market.pair_id.clone(), id);
            }
            false
        }
        Err(e) => {
            warn!("[STORAGE] Failed to insert market {}: {}", market.pair_id, e);
            false
        }
    }
}

/// Insert arb snapshot, returns true if successful.
fn insert_arb_snapshot(
    conn: &Connection,
    snapshot: &ArbSnapshotRecord,
    market_ids: &HashMap<String, i64>,
) -> bool {
    let market_id = match market_ids.get(&snapshot.pair_id) {
        Some(id) => *id,
        None => {
            warn!(
                "[STORAGE] Unknown market {} for arb snapshot, skipping",
                snapshot.pair_id
            );
            return false;
        }
    };

    let result = conn.execute(
        "INSERT INTO arb_snapshots (market_id, timestamp, timestamp_ns, yes_ask, yes_size, no_ask, no_size, total_cost, gap_cents, profit_per_contract, max_profit_cents, description, event_title, categories, running_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        rusqlite::params![
            market_id,
            snapshot.timestamp_secs,
            snapshot.timestamp_ns,
            snapshot.yes_ask,
            snapshot.yes_size,
            snapshot.no_ask,
            snapshot.no_size,
            snapshot.total_cost,
            snapshot.gap_cents,
            snapshot.profit_per_contract,
            snapshot.max_profit_cents,
            snapshot.description,
            snapshot.event_title,
            snapshot.categories,
            snapshot.running_type,
        ],
    );

    match result {
        Ok(_) => true,
        Err(e) => {
            warn!("[STORAGE] Failed to insert arb snapshot: {}", e);
            false
        }
    }
}

/// Insert trade record, returns true if successful.
fn insert_trade(
    conn: &Connection,
    trade: &TradeRecord,
    market_ids: &HashMap<String, i64>,
) -> bool {
    let market_id = match market_ids.get(&trade.pair_id) {
        Some(id) => *id,
        None => {
            warn!(
                "[STORAGE] Unknown market {} for trade, skipping",
                trade.pair_id
            );
            return false;
        }
    };

    let result = conn.execute(
        "INSERT INTO trades (market_id, timestamp, timestamp_ns, yes_order_id, no_order_id, yes_filled, no_filled, matched_contracts, yes_cost_cents, no_cost_cents, total_cost_cents, profit_cents, yes_price, no_price, latency_us, success, error, running_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        rusqlite::params![
            market_id,
            trade.timestamp_secs,
            trade.timestamp_ns,
            trade.yes_order_id,
            trade.no_order_id,
            trade.yes_filled,
            trade.no_filled,
            trade.matched_contracts,
            trade.yes_cost_cents,
            trade.no_cost_cents,
            trade.total_cost_cents,
            trade.profit_cents,
            trade.yes_price,
            trade.no_price,
            trade.latency_us,
            trade.success as i32,
            trade.error,
            trade.running_type,
        ],
    );

    match result {
        Ok(_) => true,
        Err(e) => {
            warn!("[STORAGE] Failed to insert trade: {}", e);
            false
        }
    }
}
