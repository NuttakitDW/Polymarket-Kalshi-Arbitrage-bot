//! Polymarket platform integration client.
//!
//! This module provides WebSocket client for real-time Polymarket price feeds
//! and REST API client for market discovery via the Gamma API.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{interval, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

use crate::config::{POLYMARKET_WS_URL, POLY_PING_INTERVAL_SECS, store_all_arb_enabled, debug_sizes_enabled};
use crate::execution::NanoClock;
use crate::storage::{ArbSnapshotRecord, StorageChannel};
use crate::types::{
    GlobalState, FastExecutionRequest, ArbType, PriceCents, SizeCents,
    parse_price, fxhash_str,
};

// === WebSocket Message Types ===

#[derive(Deserialize, Debug)]
pub struct BookSnapshot {
    pub asset_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub market: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub timestamp: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub hash: Option<String>,
    #[allow(dead_code)]
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
}

#[derive(Deserialize, Debug)]
pub struct PriceLevel {
    pub price: String,
    pub size: String,
}

#[derive(Deserialize, Debug)]
pub struct PriceChangeEvent {
    /// Price changes array - handle different API field names
    #[serde(default, alias = "changes")]
    pub price_changes: Option<Vec<PriceChangeItem>>,
}

#[derive(Deserialize, Debug)]
pub struct PriceChangeItem {
    pub asset_id: String,
    #[allow(dead_code)]
    pub price: Option<String>,
    /// Size of the order that changed (not total liquidity at best_ask)
    pub size: Option<String>,
    #[allow(dead_code)]
    pub side: Option<String>,
    /// Best ask price - this is what we care about for arbitrage
    pub best_ask: Option<String>,
}

#[derive(Serialize)]
struct SubscribeCmd {
    assets_ids: Vec<String>,
    #[serde(rename = "type")]
    sub_type: &'static str,
}

// === Gamma API Client ===

pub struct GammaClient {
    pub http: reqwest::Client,
}

impl GammaClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
        }
    }
    
// UNUSED:     /// Look up Polymarket market by slug, return (yes_token, no_token)
// UNUSED:     /// Tries both the exact date and next day (timezone handling)
// UNUSED:     pub async fn lookup_market(&self, slug: &str) -> Result<Option<(String, String)>> {
// UNUSED:         // Try exact slug first
// UNUSED:         if let Some(tokens) = self.try_lookup_slug(slug).await? {
// UNUSED:             return Ok(Some(tokens));
// UNUSED:         }
// UNUSED:         
// UNUSED:         // Try with next day (Polymarket may use local time)
// UNUSED:         if let Some(next_day_slug) = increment_date_in_slug(slug) {
// UNUSED:             if let Some(tokens) = self.try_lookup_slug(&next_day_slug).await? {
// UNUSED:                 info!("  📅 Found with next-day slug: {}", next_day_slug);
// UNUSED:                 return Ok(Some(tokens));
// UNUSED:             }
// UNUSED:         }
// UNUSED:         
// UNUSED:         Ok(None)
// UNUSED:     }
// UNUSED:     
// UNUSED:     async fn try_lookup_slug(&self, slug: &str) -> Result<Option<(String, String)>> {
// UNUSED:         let url = format!("{}/markets?slug={}", GAMMA_API_BASE, slug);
// UNUSED:         
// UNUSED:         let resp = self.http.get(&url).send().await?;
// UNUSED:         
// UNUSED:         if !resp.status().is_success() {
// UNUSED:             return Ok(None);
// UNUSED:         }
// UNUSED:         
// UNUSED:         let markets: Vec<GammaMarket> = resp.json().await?;
// UNUSED:         
// UNUSED:         if markets.is_empty() {
// UNUSED:             return Ok(None);
// UNUSED:         }
// UNUSED:         
// UNUSED:         let market = &markets[0];
// UNUSED:         
// UNUSED:         // Check if active and not closed
// UNUSED:         if market.closed == Some(true) || market.active == Some(false) {
// UNUSED:             return Ok(None);
// UNUSED:         }
// UNUSED:         
// UNUSED:         // Parse clobTokenIds JSON array
// UNUSED:         let token_ids: Vec<String> = market.clob_token_ids
// UNUSED:             .as_ref()
// UNUSED:             .and_then(|s| serde_json::from_str(s).ok())
// UNUSED:             .unwrap_or_default();
// UNUSED:         
// UNUSED:         if token_ids.len() >= 2 {
// UNUSED:             Ok(Some((token_ids[0].clone(), token_ids[1].clone())))
// UNUSED:         } else {
// UNUSED:             Ok(None)
// UNUSED:         }
// UNUSED:     }
// UNUSED: }
// UNUSED: 
// UNUSED: #[derive(Debug, Deserialize)]
// UNUSED: struct GammaMarket {
// UNUSED:     #[serde(rename = "clobTokenIds")]
// UNUSED:     clob_token_ids: Option<String>,
// UNUSED:     active: Option<bool>,
// UNUSED:     closed: Option<bool>,
}
// UNUSED: 
// UNUSED: /// Increment the date in a Polymarket slug by 1 day
// UNUSED: /// e.g., "epl-che-avl-2025-12-08" -> "epl-che-avl-2025-12-09"
// UNUSED: fn increment_date_in_slug(slug: &str) -> Option<String> {
// UNUSED:     let parts: Vec<&str> = slug.split('-').collect();
// UNUSED:     if parts.len() < 6 {
// UNUSED:         return None;
// UNUSED:     }
// UNUSED:     
// UNUSED:     let year: i32 = parts[3].parse().ok()?;
// UNUSED:     let month: u32 = parts[4].parse().ok()?;
// UNUSED:     let day: u32 = parts[5].parse().ok()?;
// UNUSED:     
// UNUSED:     // Compute next day
// UNUSED:     let days_in_month = match month {
// UNUSED:         1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
// UNUSED:         4 | 6 | 9 | 11 => 30,
// UNUSED:         2 => if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) { 29 } else { 28 },
// UNUSED:         _ => 31,
// UNUSED:     };
// UNUSED:     
// UNUSED:     let (new_year, new_month, new_day) = if day >= days_in_month {
// UNUSED:         if month == 12 { (year + 1, 1, 1) } else { (year, month + 1, 1) }
// UNUSED:     } else {
// UNUSED:         (year, month, day + 1)
// UNUSED:     };
// UNUSED:     
// UNUSED:     // Rebuild slug with owned strings
// UNUSED:     let prefix = parts[..3].join("-");
// UNUSED:     let suffix = if parts.len() > 6 { format!("-{}", parts[6..].join("-")) } else { String::new() };
// UNUSED: 
// UNUSED:     Some(format!("{}-{}-{:02}-{:02}{}", prefix, new_year, new_month, new_day, suffix))
// UNUSED: }
// UNUSED: 
// =============================================================================
// WebSocket Runner
// =============================================================================

/// Parse size from Polymarket (format: "123.45" dollars)
#[inline(always)]
fn parse_size(s: &str) -> SizeCents {
    // Parse as f64 and convert to cents
    s.parse::<f64>()
        .map(|size| (size * 100.0).round() as SizeCents)
        .unwrap_or(0)
}

/// WebSocket runner
pub async fn run_ws(
    state: Arc<GlobalState>,
    exec_tx: mpsc::Sender<FastExecutionRequest>,
    threshold_cents: PriceCents,
    storage: StorageChannel,
) -> Result<()> {
    let tokens: Vec<String> = state.markets.iter()
        .take(state.market_count())
        .filter_map(|m| m.pair.as_ref())
        .flat_map(|p| [p.yes_token.to_string(), p.no_token.to_string()])
        .collect();

    if tokens.is_empty() {
        info!("[POLY] No markets to monitor");
        tokio::time::sleep(Duration::from_secs(u64::MAX)).await;
        return Ok(());
    }

    // Log sample tokens for debugging
    info!("[POLY] Sample tokens to subscribe (first 3):");
    for (i, token) in tokens.iter().take(3).enumerate() {
        info!("[POLY]   Token {}: {} (len={})", i, &token[..token.len().min(40)], token.len());
    }

    let (ws_stream, _) = connect_async(POLYMARKET_WS_URL)
        .await
        .context("Failed to connect to Polymarket")?;

    info!("[POLY] Connected to {}", POLYMARKET_WS_URL);

    let (mut write, mut read) = ws_stream.split();

    // Subscribe
    let subscribe_msg = SubscribeCmd {
        assets_ids: tokens.clone(),
        sub_type: "market",
    };

    let subscribe_json = serde_json::to_string(&subscribe_msg)?;
    info!("[POLY] Sending subscription (first 200 chars): {}", &subscribe_json[..subscribe_json.len().min(200)]);
    write.send(Message::Text(subscribe_json)).await?;
    info!("[POLY] Subscribed to {} tokens", tokens.len());

    let clock = NanoClock::new();
    let mut ping_interval = interval(Duration::from_secs(POLY_PING_INTERVAL_SECS));
    let mut last_message = Instant::now();

    loop {
        tokio::select! {
            _ = ping_interval.tick() => {
                if let Err(e) = write.send(Message::Ping(vec![])).await {
                    error!("[POLY] Failed to send ping: {}", e);
                    break;
                }
            }

            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        last_message = Instant::now();

                        // Log first 10 messages for debugging
                        static MSG_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                        let msg_num = MSG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if msg_num < 10 {
                            info!("[POLY] 📨 Raw message #{} (len={}, first 300 chars): {}",
                                  msg_num, text.len(), &text[..text.len().min(300)]);
                        }

                        // Try parsing as different message types
                        if let Err(e) = handle_websocket_message(&text, &state, &exec_tx, threshold_cents, &clock, &storage).await {
                            static ERROR_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                            let error_count = ERROR_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if error_count < 5 {
                                // This is an error because we handle all known event types
                                // If we get here, it's either a new event type or malformed JSON
                                error!("[POLY] ❌ Unexpected message format #{}: {}", error_count, e);
                                error!("[POLY] Sample (first 300 chars): {}", &text[..text.len().min(300)]);
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = write.send(Message::Pong(data)).await;
                        last_message = Instant::now();
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_message = Instant::now();
                    }
                    Some(Ok(Message::Close(frame))) => {
                        warn!("[POLY] Server closed: {:?}", frame);
                        break;
                    }
                    Some(Err(e)) => {
                        error!("[POLY] WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        warn!("[POLY] Stream ended");
                        break;
                    }
                    _ => {}
                }
            }
        }

        if last_message.elapsed() > Duration::from_secs(120) {
            warn!("[POLY] Stale connection, reconnecting...");
            break;
        }
    }

    Ok(())
}

/// Handle incoming WebSocket message with efficient routing
async fn handle_websocket_message(
    text: &str,
    state: &GlobalState,
    exec_tx: &mpsc::Sender<FastExecutionRequest>,
    threshold_cents: PriceCents,
    clock: &NanoClock,
    storage: &StorageChannel,
) -> Result<()> {
    // Fast path: Check first character to determine message structure
    // This avoids expensive JSON parsing attempts on wrong types
    let first_char = text.trim_start().chars().next();

    match first_char {
        Some('[') => {
            // Array format - likely book snapshots
            let books = serde_json::from_str::<Vec<BookSnapshot>>(text)
                .context("Failed to parse as book snapshot array")?;

            if !books.is_empty() {
                info!("[POLY] Received {} book snapshots", books.len());
                for book in &books {
                    process_book(state, book, exec_tx, threshold_cents, clock, storage).await;
                }
            }
            Ok(())
        }
        Some('{') => {
            // Object format - need to check what kind
            // Parse once as generic JSON to inspect structure
            let value: serde_json::Value = serde_json::from_str(text)
                .context("Failed to parse JSON object")?;

            // Check for control messages (have "type" field)
            if let Some(msg_type) = value.get("type").and_then(|v| v.as_str()) {
                return handle_control_message(msg_type, &value);
            }

            // Check for event_type field (price_change, last_trade_price, etc.)
            if let Some(event_type) = value.get("event_type").and_then(|v| v.as_str()) {
                match event_type {
                    "price_change" => {
                        let event: PriceChangeEvent = serde_json::from_value(value)
                            .context("Failed to parse price change event")?;

                        if let Some(changes) = &event.price_changes {
                            for change in changes {
                                process_price_change(state, change, exec_tx, threshold_cents, clock, storage).await;
                            }
                        }
                        return Ok(());
                    }
                    "last_trade_price" => {
                        // Silently ignore last trade executions - we use book snapshots for accurate pricing
                        // These events are high-frequency and don't affect our arbitrage detection
                        return Ok(());
                    }
                    _ => {
                        // Silently ignore unknown event types (e.g., future API additions)
                        // This is defensive - we don't crash on new event types
                        return Ok(());
                    }
                }
            }

            // Check for book snapshot (has "asset_id" field but no "event_type")
            if value.get("asset_id").is_some() {
                let book: BookSnapshot = serde_json::from_value(value)
                    .context("Failed to parse single book snapshot")?;

                info!("[POLY] Received single book snapshot for {}",
                      &book.asset_id[..20.min(book.asset_id.len())]);
                process_book(state, &book, exec_tx, threshold_cents, clock, storage).await;
                return Ok(());
            }

            anyhow::bail!("Unknown object format - no recognized fields")
        }
        _ => {
            anyhow::bail!("Invalid JSON - expected array or object")
        }
    }
}

/// Handle control messages (subscriptions, errors, etc.)
#[inline]
fn handle_control_message(msg_type: &str, value: &serde_json::Value) -> Result<()> {
    match msg_type {
        "subscribed" => {
            info!("[POLY] Subscription confirmed");
            Ok(())
        }
        "error" => {
            let error_msg = value.get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("WebSocket error: {}", error_msg)
        }
        _ => {
            anyhow::bail!("Unknown message type: {}", msg_type)
        }
    }
}

/// Process book snapshot
#[inline]
async fn process_book(
    state: &GlobalState,
    book: &BookSnapshot,
    exec_tx: &mpsc::Sender<FastExecutionRequest>,
    threshold_cents: PriceCents,
    clock: &NanoClock,
    storage: &StorageChannel,
) {
    let token_hash = fxhash_str(&book.asset_id);

    // Find best ask (lowest price)
    let (best_ask, ask_size) = book.asks.iter()
        .filter_map(|l| {
            let price = parse_price(&l.price);
            let size = parse_size(&l.size);
            if price > 0 { Some((price, size)) } else { None }
        })
        .min_by_key(|(p, _)| *p)
        .unwrap_or((0, 0));

    // DEBUG: Log all token lookups (first 20)
    static LOGGED_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if LOGGED_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 20 {
        info!("[POLY] Book for token {} (hash: {:x}), best_ask={}, found_in_yes={}, found_in_no={}",
              &book.asset_id[..20.min(book.asset_id.len())],
              token_hash,
              best_ask,
              state.yes_addr_to_id.contains_key(&token_hash),
              state.no_addr_to_id.contains_key(&token_hash));
    }

    // Check if YES token
    if let Some(&market_id) = state.yes_addr_to_id.get(&token_hash) {
        let market = &state.markets[market_id as usize];

        // Debug: log size before storage
        if debug_sizes_enabled() {
            let pair_id = market.pair.as_ref().map(|p| p.pair_id.as_ref()).unwrap_or("unknown");
            info!("[SIZE_DEBUG] BookSnapshot YES | market={} | raw_size={} cents | price={}¢",
                  pair_id, ask_size, best_ask);
        }

        // Update YES token orderbook
        market.yes_book.update_yes(best_ask, ask_size);

        // Debug: verify size after storage
        if debug_sizes_enabled() {
            let (stored_price, _, stored_size, _) = market.yes_book.load();
            let pair_id = market.pair.as_ref().map(|p| p.pair_id.as_ref()).unwrap_or("unknown");
            info!("[SIZE_DEBUG] Stored YES    | market={} | stored_size={} cents | stored_price={}¢",
                  pair_id, stored_size, stored_price);
        }

        // Check arbs
        let arb_mask = market.check_arbs(threshold_cents);
        // Call send_arb_request if: arb detected OR store_all mode enabled
        if arb_mask != 0 || store_all_arb_enabled() {
            send_arb_request(market_id, market, arb_mask, exec_tx, clock, storage).await;
        }
    }
    // Check if NO token
    else if let Some(&market_id) = state.no_addr_to_id.get(&token_hash) {
        let market = &state.markets[market_id as usize];

        // Debug: log size before storage
        if debug_sizes_enabled() {
            let pair_id = market.pair.as_ref().map(|p| p.pair_id.as_ref()).unwrap_or("unknown");
            info!("[SIZE_DEBUG] BookSnapshot NO  | market={} | raw_size={} cents | price={}¢",
                  pair_id, ask_size, best_ask);
        }

        // Update NO token orderbook
        market.no_book.update_yes(best_ask, ask_size);

        // Debug: verify size after storage
        if debug_sizes_enabled() {
            let (stored_price, _, stored_size, _) = market.no_book.load();
            let pair_id = market.pair.as_ref().map(|p| p.pair_id.as_ref()).unwrap_or("unknown");
            info!("[SIZE_DEBUG] Stored NO     | market={} | stored_size={} cents | stored_price={}¢",
                  pair_id, stored_size, stored_price);
        }

        // Check arbs
        let arb_mask = market.check_arbs(threshold_cents);
        // Call send_arb_request if: arb detected OR store_all mode enabled
        if arb_mask != 0 || store_all_arb_enabled() {
            send_arb_request(market_id, market, arb_mask, exec_tx, clock, storage).await;
        }
    }
}

/// Process price change - extract best_ask price from the event
#[inline]
async fn process_price_change(
    state: &GlobalState,
    change: &PriceChangeItem,
    exec_tx: &mpsc::Sender<FastExecutionRequest>,
    threshold_cents: PriceCents,
    clock: &NanoClock,
    storage: &StorageChannel,
) {
    // Use best_ask from the price_change event (this is the current best ask for the token)
    let Some(price_str) = &change.best_ask else { return };
    let price = parse_price(price_str);
    if price == 0 { return; }

    let token_hash = fxhash_str(&change.asset_id);

    // Debug: log first 10 price changes with lookup results
    static PRICE_CHANGE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let pc_num = PRICE_CHANGE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if pc_num < 10 {
        let found_yes = state.yes_addr_to_id.contains_key(&token_hash);
        let found_no = state.no_addr_to_id.contains_key(&token_hash);
        info!("[POLY] PriceChange #{}: asset={} best_ask={} found_in_yes={} found_in_no={}",
              pc_num, &change.asset_id[..change.asset_id.len().min(30)], price_str, found_yes, found_no);
    }

    // Parse size from event if available (size of the order that changed, in dollars -> cents)
    // Note: This is a single order's size, not total liquidity at best_ask
    let event_size: Option<SizeCents> = change.size.as_ref()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|size| (size * 100.0).round() as SizeCents);

    // Check YES token
    if let Some(&market_id) = state.yes_addr_to_id.get(&token_hash) {
        let market = &state.markets[market_id as usize];
        let (current_yes, _, current_yes_size, _) = market.yes_book.load();

        // Update price (best_ask is always the current best, so always update)
        if price != current_yes {
            // Keep existing size from BookSnapshot. Price_change size is a single order,
            // not total liquidity. Only use event_size if we have no size yet (0).
            let new_size = if current_yes_size == 0 {
                event_size.unwrap_or(0)
            } else {
                current_yes_size
            };
            market.yes_book.update_yes(price, new_size);

            let arb_mask = market.check_arbs(threshold_cents);
            // Call send_arb_request if: arb detected OR store_all mode enabled
            if arb_mask != 0 || store_all_arb_enabled() {
                send_arb_request(market_id, market, arb_mask, exec_tx, clock, storage).await;
            }
        }
    }
    // Check NO token
    else if let Some(&market_id) = state.no_addr_to_id.get(&token_hash) {
        let market = &state.markets[market_id as usize];
        let (current_no, _, current_no_size, _) = market.no_book.load();

        if price != current_no {
            // Keep existing size from BookSnapshot. Price_change size is a single order,
            // not total liquidity. Only use event_size if we have no size yet (0).
            let new_size = if current_no_size == 0 {
                event_size.unwrap_or(0)
            } else {
                current_no_size
            };
            market.no_book.update_yes(price, new_size);

            let arb_mask = market.check_arbs(threshold_cents);
            // Call send_arb_request if: arb detected OR store_all mode enabled
            if arb_mask != 0 || store_all_arb_enabled() {
                send_arb_request(market_id, market, arb_mask, exec_tx, clock, storage).await;
            }
        }
    }
}

/// Convert comma-separated categories to JSON array string.
/// Input: "Politics, Business" -> Output: `["Politics","Business"]`
fn categories_to_json(category: &Option<Arc<str>>) -> Option<String> {
    category.as_ref().map(|cat| {
        let parts: Vec<&str> = cat
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        serde_json::to_string(&parts).unwrap_or_else(|_| "[]".to_string())
    })
}

// Counters for debugging arb storage issues
static ARB_CALL_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ARB_BOTH_PRICES_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ARB_SHOULD_STORE_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ARB_PAIR_SOME_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Get arb storage debug stats (call_count, both_prices, should_store, pair_some)
pub fn get_arb_debug_stats() -> (usize, usize, usize, usize) {
    (
        ARB_CALL_COUNT.load(std::sync::atomic::Ordering::Relaxed),
        ARB_BOTH_PRICES_COUNT.load(std::sync::atomic::Ordering::Relaxed),
        ARB_SHOULD_STORE_COUNT.load(std::sync::atomic::Ordering::Relaxed),
        ARB_PAIR_SOME_COUNT.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Send arb request to execution engine and record to storage
#[inline]
async fn send_arb_request(
    market_id: u16,
    market: &crate::types::AtomicMarketState,
    arb_mask: u8,
    exec_tx: &mpsc::Sender<FastExecutionRequest>,
    clock: &NanoClock,
    storage: &StorageChannel,
) {
    ARB_CALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let (yes_ask, _yes_bid, yes_size, _) = market.yes_book.load();
    let (no_ask, _no_bid, no_size, _) = market.no_book.load();

    // Debug: log sizes at execution request time
    if debug_sizes_enabled() {
        let pair_id = market.pair.as_ref().map(|p| p.pair_id.as_ref()).unwrap_or("unknown");
        info!("[SIZE_DEBUG] ArbRequest    | market={} | yes_size={} cents | no_size={} cents | MATCH={}",
              pair_id, yes_size, no_size, if yes_size == no_size { "⚠️ SAME" } else { "✓ DIFF" });
    }

    // Need both prices to record meaningful data
    if yes_ask == 0 || no_ask == 0 {
        return;
    }

    ARB_BOTH_PRICES_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Use current prices for storage (even if no arb detected)
    let yes_price = yes_ask;
    let no_price = no_ask;
    let yes_sz = yes_size;
    let no_sz = no_size;

    // Check if this is an actual arb opportunity (arb_mask & 4 = Poly-only arb)
    let is_profitable_arb = arb_mask & 4 != 0;

    let detected_ns = clock.now_ns();
    let total_cost = yes_price + no_price;
    let gap_cents = total_cost as i16 - 100;
    let profit_per_contract = if gap_cents < 0 { (-gap_cents) as u16 } else { 0 };
    // Max profit = profit_per_contract * min(yes_size, no_size) / 100
    // Size is in cents, divide by 100 to get number of $1 contracts
    let min_size = yes_sz.min(no_sz) as u32;
    let max_profit_cents = (profit_per_contract as u32 * min_size) / 100;

    // Check if order value meets $1 minimum per leg
    // Order value = (contracts) × price = (min_size/100) × price_cents
    // For $1 minimum: (min_size/100) × price >= 100 cents
    // Simplified: min_size × price >= 10000
    const MIN_ORDER_VALUE_THRESHOLD: u32 = 10000; // $1.00 per leg
    let yes_order_value = min_size * yes_price as u32;
    let no_order_value = min_size * no_price as u32;
    let meets_threshold = yes_order_value >= MIN_ORDER_VALUE_THRESHOLD
        && no_order_value >= MIN_ORDER_VALUE_THRESHOLD;

    // Record to SQLite storage
    // When STORE_ALL_ARB=1: store ALL price snapshots for verification
    // Otherwise: only store profitable arbs that meet the $1 minimum threshold
    let should_store = store_all_arb_enabled() || (is_profitable_arb && meets_threshold);

    if should_store {
        ARB_SHOULD_STORE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(pair) = &market.pair {
            ARB_PAIR_SOME_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let now = chrono::Utc::now();
            storage.record_arb(ArbSnapshotRecord {
                pair_id: pair.pair_id.to_string(),
                timestamp_secs: now.timestamp(),
                timestamp_ns: (detected_ns % 1_000_000_000) as u32,
                yes_ask: yes_price,
                no_ask: no_price,
                total_cost,
                gap_cents,
                profit_per_contract,
                max_profit_cents,
                description: Some(pair.description.to_string()),
                event_title: pair.event_title.as_ref().map(|s| s.to_string()),
                categories: categories_to_json(&pair.category),
                running_type: storage.running_type.clone(),
            });
        }
    }

    // Only execute if: profitable arb detected AND meets threshold
    if !is_profitable_arb || !meets_threshold {
        return;
    }

    let req = FastExecutionRequest {
        market_id,
        yes_price,
        no_price,
        yes_size: yes_sz,
        no_size: no_sz,
        arb_type: ArbType::PolyOnly,
        detected_ns,
    };

    // send! ~~
    let _ = exec_tx.try_send(req);
}