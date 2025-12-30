//! Intelligent market discovery and matching system.
//!
//! This module handles the discovery of matching markets between Kalshi and Polymarket,
//! with support for caching, incremental updates, and parallel processing.

#![allow(dead_code, unused_variables, unused_imports)]

use anyhow::Result;
use futures_util::{stream, StreamExt};
use governor::{Quota, RateLimiter, state::NotKeyed, clock::DefaultClock, middleware::NoOpMiddleware};
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::config::GAMMA_API_BASE;
use crate::polymarket::GammaClient;
use crate::types::{MarketPair, MarketType, DiscoveryResult};

/// Max concurrent Gamma API requests
const GAMMA_CONCURRENCY: usize = 20;

/// Kalshi rate limit: 2 requests per second (very conservative - they rate limit aggressively)
/// Must be conservative because discovery runs many leagues/series in parallel
const KALSHI_RATE_LIMIT_PER_SEC: u32 = 2;

/// Max concurrent Kalshi API requests GLOBALLY across all leagues/series
/// This is the hard cap - prevents bursting even when rate limiter has tokens
const KALSHI_GLOBAL_CONCURRENCY: usize = 1;

/// Cache file path
const DISCOVERY_CACHE_PATH: &str = ".discovery_cache.json";

/// Cache TTL in seconds (2 hours - new markets appear every ~2 hours)
const CACHE_TTL_SECS: u64 = 2 * 60 * 60;
// UNUSED: 
// UNUSED: /// Task for parallel Gamma lookup
// UNUSED: struct GammaLookupTask {
// UNUSED:     event: Arc<KalshiEvent>,
// UNUSED:     market: KalshiMarket,
// UNUSED:     poly_slug: String,
// UNUSED:     market_type: MarketType,
// UNUSED:     league: String,
// UNUSED: }
// UNUSED: 
// UNUSED: /// Type alias for Kalshi rate limiter
// UNUSED: type KalshiRateLimiter = RateLimiter<NotKeyed, governor::state::InMemoryState, DefaultClock, NoOpMiddleware>;
// UNUSED: 
// UNUSED: /// Persistent cache for discovered market pairs
// UNUSED: #[derive(Debug, Clone, Serialize, Deserialize)]
// UNUSED: struct DiscoveryCache {
// UNUSED:     /// Unix timestamp when cache was created
// UNUSED:     timestamp_secs: u64,
// UNUSED:     /// Cached market pairs
// UNUSED:     pairs: Vec<MarketPair>,
// UNUSED:     /// Set of known Kalshi market tickers (for incremental updates)
// UNUSED:     known_kalshi_tickers: Vec<String>,
// UNUSED: }
// UNUSED: 
// UNUSED: impl DiscoveryCache {
// UNUSED:     fn new(pairs: Vec<MarketPair>) -> Self {
// UNUSED:         let known_kalshi_tickers: Vec<String> = pairs.iter()
// UNUSED:             .map(|p| p.kalshi_market_ticker.to_string())
// UNUSED:             .collect();
// UNUSED:         Self {
// UNUSED:             timestamp_secs: current_unix_secs(),
// UNUSED:             pairs,
// UNUSED:             known_kalshi_tickers,
// UNUSED:         }
// UNUSED:     }
// UNUSED: 
// UNUSED:     fn is_expired(&self) -> bool {
// UNUSED:         let now = current_unix_secs();
// UNUSED:         now.saturating_sub(self.timestamp_secs) > CACHE_TTL_SECS
// UNUSED:     }
// UNUSED: 
// UNUSED:     fn age_secs(&self) -> u64 {
// UNUSED:         current_unix_secs().saturating_sub(self.timestamp_secs)
// UNUSED:     }
// UNUSED: 
// UNUSED:     fn has_ticker(&self, ticker: &str) -> bool {
// UNUSED:         self.known_kalshi_tickers.iter().any(|t| t == ticker)
// UNUSED:     }
// UNUSED: }
// UNUSED: 
// UNUSED: fn current_unix_secs() -> u64 {
// UNUSED:     SystemTime::now()
// UNUSED:         .duration_since(UNIX_EPOCH)
// UNUSED:         .unwrap_or_default()
// UNUSED:         .as_secs()
// UNUSED: }

/// Market discovery and matching client for Polymarket markets
pub struct DiscoveryClient {
    gamma: Arc<GammaClient>,
    gamma_semaphore: Arc<Semaphore>,
}

impl DiscoveryClient {
    // Removed: pub fn new() - use new_polymarket_only() instead

// UNUSED:     /// Load cache from disk (async)
// UNUSED:     async fn load_cache() -> Option<DiscoveryCache> {
// UNUSED:         let data = tokio::fs::read_to_string(DISCOVERY_CACHE_PATH).await.ok()?;
// UNUSED:         serde_json::from_str(&data).ok()
// UNUSED:     }
// UNUSED: 
// UNUSED:     /// Save cache to disk (async)
// UNUSED:     async fn save_cache(cache: &DiscoveryCache) -> Result<()> {
// UNUSED:         let data = serde_json::to_string_pretty(cache)?;
// UNUSED:         tokio::fs::write(DISCOVERY_CACHE_PATH, data).await?;
// UNUSED:         Ok(())
// UNUSED:     }
// UNUSED:     
// UNUSED:     /// Discover all market pairs with caching support
// UNUSED:     ///
// UNUSED:     /// Strategy:
// UNUSED:     /// 1. Try to load cache from disk
// UNUSED:     /// 2. If cache exists and is fresh (<2 hours), use it directly
// UNUSED:     /// 3. If cache exists but is stale, load it + fetch incremental updates
// UNUSED:     /// 4. If no cache, do full discovery
// UNUSED:     pub async fn discover_all(&self, leagues: &[&str]) -> DiscoveryResult {
// UNUSED:         // Try to load existing cache
// UNUSED:         let cached = Self::load_cache().await;
// UNUSED: 
// UNUSED:         match cached {
// UNUSED:             Some(cache) if !cache.is_expired() => {
// UNUSED:                 // Cache is fresh - use it directly
// UNUSED:                 info!("📂 Loaded {} pairs from cache (age: {}s)",
// UNUSED:                       cache.pairs.len(), cache.age_secs());
// UNUSED:                 return DiscoveryResult {
// UNUSED:                     pairs: cache.pairs,
// UNUSED:                     kalshi_events_found: 0,  // From cache
// UNUSED:                     poly_matches: 0,
// UNUSED:                     poly_misses: 0,
// UNUSED:                     errors: vec![],
// UNUSED:                 };
// UNUSED:             }
// UNUSED:             Some(cache) => {
// UNUSED:                 // Cache is stale - do incremental discovery
// UNUSED:                 info!("📂 Cache expired (age: {}s), doing incremental refresh...", cache.age_secs());
// UNUSED:                 return self.discover_incremental(leagues, cache).await;
// UNUSED:             }
// UNUSED:             None => {
// UNUSED:                 // No cache - do full discovery
// UNUSED:                 info!("📂 No cache found, doing full discovery...");
// UNUSED:             }
// UNUSED:         }
// UNUSED: 
// UNUSED:         // Full discovery (no cache)
// UNUSED:         let result = self.discover_full(leagues).await;
// UNUSED: 
// UNUSED:         // Save to cache
// UNUSED:         if !result.pairs.is_empty() {
// UNUSED:             let cache = DiscoveryCache::new(result.pairs.clone());
// UNUSED:             if let Err(e) = Self::save_cache(&cache).await {
// UNUSED:                 warn!("Failed to save discovery cache: {}", e);
// UNUSED:             } else {
// UNUSED:                 info!("💾 Saved {} pairs to cache", result.pairs.len());
// UNUSED:             }
// UNUSED:         }
// UNUSED: 
// UNUSED:         result
// UNUSED:     }
// UNUSED: 
// UNUSED:     /// Force full discovery (ignores cache)
// UNUSED:     pub async fn discover_all_force(&self, leagues: &[&str]) -> DiscoveryResult {
// UNUSED:         info!("🔄 Forced full discovery (ignoring cache)...");
// UNUSED:         let result = self.discover_full(leagues).await;
// UNUSED: 
// UNUSED:         // Save to cache
// UNUSED:         if !result.pairs.is_empty() {
// UNUSED:             let cache = DiscoveryCache::new(result.pairs.clone());
// UNUSED:             if let Err(e) = Self::save_cache(&cache).await {
// UNUSED:                 warn!("Failed to save discovery cache: {}", e);
// UNUSED:             } else {
// UNUSED:                 info!("💾 Saved {} pairs to cache", result.pairs.len());
// UNUSED:             }
// UNUSED:         }
// UNUSED: 
// UNUSED:         result
// UNUSED:     }
// UNUSED: 
// UNUSED:     /// Full discovery without cache
// UNUSED:     async fn discover_full(&self, leagues: &[&str]) -> DiscoveryResult {
// UNUSED:         let configs: Vec<_> = if leagues.is_empty() {
// UNUSED:             get_league_configs()
// UNUSED:         } else {
// UNUSED:             leagues.iter()
// UNUSED:                 .filter_map(|l| get_league_config(l))
// UNUSED:                 .collect()
// UNUSED:         };
// UNUSED: 
// UNUSED:         // Parallel discovery across all leagues
// UNUSED:         let league_futures: Vec<_> = configs.iter()
// UNUSED:             .map(|config| self.discover_league(config, None))
// UNUSED:             .collect();
// UNUSED: 
// UNUSED:         let league_results = futures_util::future::join_all(league_futures).await;
// UNUSED: 
// UNUSED:         // Merge results
// UNUSED:         let mut result = DiscoveryResult::default();
// UNUSED:         for league_result in league_results {
// UNUSED:             result.pairs.extend(league_result.pairs);
// UNUSED:             result.poly_matches += league_result.poly_matches;
// UNUSED:             result.errors.extend(league_result.errors);
// UNUSED:         }
// UNUSED:         result.kalshi_events_found = result.pairs.len();
// UNUSED: 
// UNUSED:         result
// UNUSED:     }
// UNUSED: 
// UNUSED:     /// Incremental discovery - merge cached pairs with newly discovered ones
// UNUSED:     async fn discover_incremental(&self, leagues: &[&str], cache: DiscoveryCache) -> DiscoveryResult {
// UNUSED:         let configs: Vec<_> = if leagues.is_empty() {
// UNUSED:             get_league_configs()
// UNUSED:         } else {
// UNUSED:             leagues.iter()
// UNUSED:                 .filter_map(|l| get_league_config(l))
// UNUSED:                 .collect()
// UNUSED:         };
// UNUSED: 
// UNUSED:         // Discover with filter for known tickers
// UNUSED:         let league_futures: Vec<_> = configs.iter()
// UNUSED:             .map(|config| self.discover_league(config, Some(&cache)))
// UNUSED:             .collect();
// UNUSED: 
// UNUSED:         let league_results = futures_util::future::join_all(league_futures).await;
// UNUSED: 
// UNUSED:         // Merge cached pairs with newly discovered ones
// UNUSED:         let mut all_pairs = cache.pairs;
// UNUSED:         let mut new_count = 0;
// UNUSED: 
// UNUSED:         for league_result in league_results {
// UNUSED:             for pair in league_result.pairs {
// UNUSED:                 if !all_pairs.iter().any(|p| *p.kalshi_market_ticker == *pair.kalshi_market_ticker) {
// UNUSED:                     all_pairs.push(pair);
// UNUSED:                     new_count += 1;
// UNUSED:                 }
// UNUSED:             }
// UNUSED:         }
// UNUSED: 
// UNUSED:         if new_count > 0 {
// UNUSED:             info!("🆕 Found {} new market pairs", new_count);
// UNUSED: 
// UNUSED:             // Update cache
// UNUSED:             let new_cache = DiscoveryCache::new(all_pairs.clone());
// UNUSED:             if let Err(e) = Self::save_cache(&new_cache).await {
// UNUSED:                 warn!("Failed to update discovery cache: {}", e);
// UNUSED:             } else {
// UNUSED:                 info!("💾 Updated cache with {} total pairs", all_pairs.len());
// UNUSED:             }
// UNUSED:         } else {
// UNUSED:             info!("✅ No new markets found, using {} cached pairs", all_pairs.len());
// UNUSED: 
// UNUSED:             // Just update timestamp to extend TTL
// UNUSED:             let refreshed_cache = DiscoveryCache::new(all_pairs.clone());
// UNUSED:             let _ = Self::save_cache(&refreshed_cache).await;
// UNUSED:         }
// UNUSED: 
// UNUSED:         DiscoveryResult {
// UNUSED:             pairs: all_pairs,
// UNUSED:             kalshi_events_found: new_count,
// UNUSED:             poly_matches: new_count,
// UNUSED:             poly_misses: 0,
// UNUSED:             errors: vec![],
// UNUSED:         }
// UNUSED:     }
// UNUSED:     
// UNUSED:     /// Discover all market types for a single league (PARALLEL)
// UNUSED:     /// If cache is provided, only discovers markets not already in cache
// UNUSED:     async fn discover_league(&self, config: &LeagueConfig, cache: Option<&DiscoveryCache>) -> DiscoveryResult {
// UNUSED:         info!("🔍 Discovering {} markets...", config.league_code);
// UNUSED: 
// UNUSED:         let market_types = [MarketType::Moneyline, MarketType::Spread, MarketType::Total, MarketType::Btts];
// UNUSED: 
// UNUSED:         // Parallel discovery across market types
// UNUSED:         let type_futures: Vec<_> = market_types.iter()
// UNUSED:             .filter_map(|market_type| {
// UNUSED:                 let series = self.get_series_for_type(config, *market_type)?;
// UNUSED:                 Some(self.discover_series(config, series, *market_type, cache))
// UNUSED:             })
// UNUSED:             .collect();
// UNUSED: 
// UNUSED:         let type_results = futures_util::future::join_all(type_futures).await;
// UNUSED: 
// UNUSED:         let mut result = DiscoveryResult::default();
// UNUSED:         for (pairs_result, market_type) in type_results.into_iter().zip(market_types.iter()) {
// UNUSED:             match pairs_result {
// UNUSED:                 Ok(pairs) => {
// UNUSED:                     let count = pairs.len();
// UNUSED:                     if count > 0 {
// UNUSED:                         info!("  ✅ {} {}: {} pairs", config.league_code, market_type, count);
// UNUSED:                     }
// UNUSED:                     result.poly_matches += count;
// UNUSED:                     result.pairs.extend(pairs);
// UNUSED:                 }
// UNUSED:                 Err(e) => {
// UNUSED:                     result.errors.push(format!("{} {}: {}", config.league_code, market_type, e));
// UNUSED:                 }
// UNUSED:             }
// UNUSED:         }
// UNUSED: 
// UNUSED:         result
// UNUSED:     }
// UNUSED:     
// UNUSED:     fn get_series_for_type(&self, config: &LeagueConfig, market_type: MarketType) -> Option<&'static str> {
// UNUSED:         match market_type {
// UNUSED:             MarketType::Moneyline => Some(config.kalshi_series_game),
// UNUSED:             MarketType::Spread => config.kalshi_series_spread,
// UNUSED:             MarketType::Total => config.kalshi_series_total,
// UNUSED:             MarketType::Btts => config.kalshi_series_btts,
// UNUSED:         }
// UNUSED:     }
// UNUSED:     
// UNUSED:     /// Discover markets for a specific series (PARALLEL Kalshi + Gamma lookups)
// UNUSED:     /// If cache is provided, skips markets already in cache
// UNUSED:     async fn discover_series(
// UNUSED:         &self,
// UNUSED:         config: &LeagueConfig,
// UNUSED:         series: &str,
// UNUSED:         market_type: MarketType,
// UNUSED:         cache: Option<&DiscoveryCache>,
// UNUSED:     ) -> Result<Vec<MarketPair>> {
// UNUSED:         // Get Kalshi client (required for this method)
// UNUSED:         let kalshi = self.kalshi.as_ref()
// UNUSED:             .expect("Kalshi client required for discover_series");
// UNUSED: 
// UNUSED:         // Fetch Kalshi events
// UNUSED:         {
// UNUSED:             let _permit = self.kalshi_semaphore.acquire().await.map_err(|e| anyhow::anyhow!("semaphore closed: {}", e))?;
// UNUSED:             self.kalshi_limiter.until_ready().await;
// UNUSED:         }
// UNUSED:         let events = kalshi.get_events(series, 50).await?;
// UNUSED: 
// UNUSED:         // PHASE 2: Parallel market fetching
// UNUSED:         let kalshi = kalshi.clone();
// UNUSED:         let limiter = self.kalshi_limiter.clone();
// UNUSED:         let semaphore = self.kalshi_semaphore.clone();
// UNUSED: 
// UNUSED:         // Parse events first, filtering out unparseable ones
// UNUSED:         let parsed_events: Vec<_> = events.into_iter()
// UNUSED:             .filter_map(|event| {
// UNUSED:                 let parsed = match parse_kalshi_event_ticker(&event.event_ticker) {
// UNUSED:                     Some(p) => p,
// UNUSED:                     None => {
// UNUSED:                         warn!("  ⚠️ Could not parse event ticker {}", event.event_ticker);
// UNUSED:                         return None;
// UNUSED:                     }
// UNUSED:                 };
// UNUSED:                 Some((parsed, event))
// UNUSED:             })
// UNUSED:             .collect();
// UNUSED: 
// UNUSED:         // Execute market fetches with GLOBAL concurrency limit
// UNUSED:         let market_results: Vec<_> = stream::iter(parsed_events)
// UNUSED:             .map(|(parsed, event)| {
// UNUSED:                 let kalshi = kalshi.clone();
// UNUSED:                 let limiter = limiter.clone();
// UNUSED:                 let semaphore = semaphore.clone();
// UNUSED:                 let event_ticker = event.event_ticker.clone();
// UNUSED:                 async move {
// UNUSED:                     let _permit = semaphore.acquire().await.ok();
// UNUSED:                     // rate limit
// UNUSED:                     limiter.until_ready().await;
// UNUSED:                     let markets_result = kalshi.get_markets(&event_ticker).await;
// UNUSED:                     (parsed, Arc::new(event), markets_result)
// UNUSED:                 }
// UNUSED:             })
// UNUSED:             .buffer_unordered(KALSHI_GLOBAL_CONCURRENCY * 2)  // Allow some buffering, semaphore is the real limit
// UNUSED:             .collect()
// UNUSED:             .await;
// UNUSED: 
// UNUSED:         // Collect all (event, market) pairs
// UNUSED:         let mut event_markets = Vec::with_capacity(market_results.len() * 3);
// UNUSED:         for (parsed, event, markets_result) in market_results {
// UNUSED:             match markets_result {
// UNUSED:                 Ok(markets) => {
// UNUSED:                     for market in markets {
// UNUSED:                         // Skip if already in cache
// UNUSED:                         if let Some(c) = cache {
// UNUSED:                             if c.has_ticker(&market.ticker) {
// UNUSED:                                 continue;
// UNUSED:                             }
// UNUSED:                         }
// UNUSED:                         event_markets.push((parsed.clone(), event.clone(), market));
// UNUSED:                     }
// UNUSED:                 }
// UNUSED:                 Err(e) => {
// UNUSED:                     warn!("  ⚠️ Failed to get markets for {}: {}", event.event_ticker, e);
// UNUSED:                 }
// UNUSED:             }
// UNUSED:         }
// UNUSED:         
// UNUSED:         // Parallel Gamma lookups with semaphore
// UNUSED:         let lookup_futures: Vec<_> = event_markets
// UNUSED:             .into_iter()
// UNUSED:             .map(|(parsed, event, market)| {
// UNUSED:                 let poly_slug = self.build_poly_slug(config.poly_prefix, &parsed, market_type, &market);
// UNUSED:                 
// UNUSED:                 GammaLookupTask {
// UNUSED:                     event,
// UNUSED:                     market,
// UNUSED:                     poly_slug,
// UNUSED:                     market_type,
// UNUSED:                     league: config.league_code.to_string(),
// UNUSED:                 }
// UNUSED:             })
// UNUSED:             .collect();
// UNUSED:         
// UNUSED:         // Execute lookups in parallel 
// UNUSED:         let pairs: Vec<MarketPair> = stream::iter(lookup_futures)
// UNUSED:             .map(|task| {
// UNUSED:                 let gamma = self.gamma.clone();
// UNUSED:                 let semaphore = self.gamma_semaphore.clone();
// UNUSED:                 async move {
// UNUSED:                     let _permit = semaphore.acquire().await.ok()?;
// UNUSED:                     match gamma.lookup_market(&task.poly_slug).await {
// UNUSED:                         Ok(Some((yes_token, no_token))) => {
// UNUSED:                             let team_suffix = extract_team_suffix(&task.market.ticker);
// UNUSED:                             Some(MarketPair {
// UNUSED:                                 pair_id: format!("{}-{}", task.poly_slug, task.market.ticker).into(),
// UNUSED:                                 league: task.league.into(),
// UNUSED:                                 market_type: task.market_type,
// UNUSED:                                 description: format!("{} - {}", task.event.title, task.market.title).into(),
// UNUSED:                                 kalshi_event_ticker: task.event.event_ticker.clone().into(),
// UNUSED:                                 kalshi_market_ticker: task.market.ticker.into(),
// UNUSED:                                 poly_slug: task.poly_slug.into(),
// UNUSED:                                 poly_yes_token: yes_token.into(),
// UNUSED:                                 poly_no_token: no_token.into(),
// UNUSED:                                 line_value: task.market.floor_strike,
// UNUSED:                                 team_suffix: team_suffix.map(|s| s.into()),
// UNUSED:                             })
// UNUSED:                         }
// UNUSED:                         Ok(None) => None,
// UNUSED:                         Err(e) => {
// UNUSED:                             warn!("  ⚠️ Gamma lookup failed for {}: {}", task.poly_slug, e);
// UNUSED:                             None
// UNUSED:                         }
// UNUSED:                     }
// UNUSED:                 }
// UNUSED:             })
// UNUSED:             .buffer_unordered(GAMMA_CONCURRENCY)
// UNUSED:             .filter_map(|x| async { x })
// UNUSED:             .collect()
// UNUSED:             .await;
// UNUSED:         
// UNUSED:         Ok(pairs)
// UNUSED:     }
// UNUSED:     
// UNUSED:     /// Build Polymarket slug from Kalshi event data
// UNUSED:     fn build_poly_slug(
// UNUSED:         &self,
// UNUSED:         poly_prefix: &str,
// UNUSED:         parsed: &ParsedKalshiTicker,
// UNUSED:         market_type: MarketType,
// UNUSED:         market: &KalshiMarket,
// UNUSED:     ) -> String {
// UNUSED:         // Convert Kalshi team codes to Polymarket codes using cache
// UNUSED:         let poly_team1 = self.team_cache
// UNUSED:             .kalshi_to_poly(poly_prefix, &parsed.team1)
// UNUSED:             .unwrap_or_else(|| parsed.team1.to_lowercase());
// UNUSED:         let poly_team2 = self.team_cache
// UNUSED:             .kalshi_to_poly(poly_prefix, &parsed.team2)
// UNUSED:             .unwrap_or_else(|| parsed.team2.to_lowercase());
// UNUSED:         
// UNUSED:         // Convert date from "25DEC27" to "2025-12-27"
// UNUSED:         let date_str = kalshi_date_to_iso(&parsed.date);
// UNUSED:         
// UNUSED:         // Base slug: league-team1-team2-date
// UNUSED:         let base = format!("{}-{}-{}-{}", poly_prefix, poly_team1, poly_team2, date_str);
// UNUSED:         
// UNUSED:         match market_type {
// UNUSED:             MarketType::Moneyline => {
// UNUSED:                 if let Some(suffix) = extract_team_suffix(&market.ticker) {
// UNUSED:                     if suffix.to_lowercase() == "tie" {
// UNUSED:                         format!("{}-draw", base)
// UNUSED:                     } else {
// UNUSED:                         let poly_suffix = self.team_cache
// UNUSED:                             .kalshi_to_poly(poly_prefix, &suffix)
// UNUSED:                             .unwrap_or_else(|| suffix.to_lowercase());
// UNUSED:                         format!("{}-{}", base, poly_suffix)
// UNUSED:                     }
// UNUSED:                 } else {
// UNUSED:                     base
// UNUSED:                 }
// UNUSED:             }
// UNUSED:             MarketType::Spread => {
// UNUSED:                 if let Some(floor) = market.floor_strike {
// UNUSED:                     let floor_str = format!("{:.1}", floor).replace(".", "pt");
// UNUSED:                     format!("{}-spread-{}", base, floor_str)
// UNUSED:                 } else {
// UNUSED:                     format!("{}-spread", base)
// UNUSED:                 }
// UNUSED:             }
// UNUSED:             MarketType::Total => {
// UNUSED:                 if let Some(floor) = market.floor_strike {
// UNUSED:                     let floor_str = format!("{:.1}", floor).replace(".", "pt");
// UNUSED:                     format!("{}-total-{}", base, floor_str)
// UNUSED:                 } else {
// UNUSED:                     format!("{}-total", base)
// UNUSED:                 }
// UNUSED:             }
// UNUSED:             MarketType::Btts => {
// UNUSED:                 format!("{}-btts", base)
// UNUSED:             }
// UNUSED:         }
// UNUSED:     }
// UNUSED: 
// UNUSED:     /// Create a new DiscoveryClient for Polymarket-only arbitrage
    pub fn new_polymarket_only() -> Self {
        Self {
            gamma: Arc::new(GammaClient::new()),
            gamma_semaphore: Arc::new(Semaphore::new(GAMMA_CONCURRENCY)),
        }
    }

    /// Discover Polymarket-only arbitrage opportunities
    ///
    /// Strategy: Find multi-outcome markets on Polymarket where you can buy
    /// competing outcomes for less than $1.00 total
    pub async fn discover_polymarket_only(&self, _leagues: &[&str]) -> DiscoveryResult {
        info!("🔍 Discovering Polymarket-only arbitrage opportunities...");

        let mut result = DiscoveryResult {
            pairs: vec![],
            poly_matches: 0,
            errors: vec![],
        };

        // Fetch all active Polymarket markets
        let markets = match self.fetch_polymarket_markets().await {
            Ok(m) => m,
            Err(e) => {
                result.errors.push(format!("Failed to fetch Polymarket markets: {}", e));
                return result;
            }
        };

        info!("📊 Fetched {} Polymarket markets", markets.len());

        // Track tag statistics (each market can have multiple tags)
        let mut tag_stats: HashMap<String, usize> = HashMap::new();
        let mut markets_with_tags = 0;
        let mut markets_without_tags = 0;

        // Debug: show sample of markets
        if !markets.is_empty() {
            let sample = &markets[0];
            info!("   Sample market: question='{}', tokens={}, outcomes={}, tags={:?}",
                  sample.question.chars().take(50).collect::<String>(),
                  sample.token_ids.len(),
                  sample.outcomes.len(),
                  sample.tags);
        }

        // Filter for multi-outcome markets (2+ outcomes)
        let multi_outcome_markets: Vec<_> = markets.into_iter()
            .filter(|m| {
                let has_multiple = m.token_ids.len() >= 2 && m.outcomes.len() >= 2;
                if !has_multiple {
                    tracing::debug!("   Skipped: '{}' (tokens={}, outcomes={})",
                        m.question.chars().take(40).collect::<String>(),
                        m.token_ids.len(),
                        m.outcomes.len());
                }
                has_multiple
            })
            .collect();

        info!("🎯 Found {} multi-outcome markets", multi_outcome_markets.len());

        // For each multi-outcome market, create pairs from all competing outcome combinations
        for market in multi_outcome_markets {
            let num_outcomes = market.token_ids.len();

            // Track tags for this market
            if market.tags.is_empty() {
                markets_without_tags += 1;
                *tag_stats.entry("(no tags)".to_string()).or_insert(0) += 1;
            } else {
                markets_with_tags += 1;
                // Count each tag separately
                for tag in &market.tags {
                    *tag_stats.entry(tag.clone()).or_insert(0) += 1;
                }
            }

            // Create pairs for all combinations of competing outcomes
            for i in 0..num_outcomes {
                for j in (i + 1)..num_outcomes {
                    if let Some(pair) = self.create_token_pair(&market, i, j) {
                        result.pairs.push(pair);
                        result.poly_matches += 1;
                    }
                }
            }
        }

        info!("✅ Found {} competing outcome pairs across all markets", result.pairs.len());

        // Log tag statistics
        info!("🏷️  Tag distribution ({} with tags, {} without):",
              markets_with_tags, markets_without_tags);

        // Sort tags by count (descending)
        let mut tag_vec: Vec<_> = tag_stats.into_iter().collect();
        tag_vec.sort_by(|a, b| b.1.cmp(&a.1));

        // Log top tags (limit to top 20 for readability)
        for (i, (tag, count)) in tag_vec.iter().take(20).enumerate() {
            info!("   {:2}. {} ({} markets)", i + 1, tag, count);
        }

        if tag_vec.len() > 20 {
            info!("   ... and {} more tags", tag_vec.len() - 20);
        }

        result
    }

    /// Discover Polymarket-only arbitrage opportunities with forced refresh
    pub async fn discover_polymarket_only_force(&self, leagues: &[&str]) -> DiscoveryResult {
        self.discover_polymarket_only(leagues).await
    }

    /// Fetch all active Polymarket markets from Gamma API /events endpoint with pagination
    /// Uses /events instead of /markets to get proper category tags
    async fn fetch_polymarket_markets(&self) -> Result<Vec<PolymarketMarket>> {
        const PAGE_SIZE: usize = 100; // Events endpoint uses smaller pages
        const MAX_PAGES: usize = 50; // Safety limit: 50 * 100 = 5,000 events max

        let mut all_events: Vec<crate::types::GammaEventResponse> = Vec::new();
        let mut offset = 0;
        let mut page = 0;

        // Paginate through all events
        loop {
            if page >= MAX_PAGES {
                warn!("⚠️ Reached max pages limit ({}), stopping pagination", MAX_PAGES);
                break;
            }

            let url = format!(
                "{}/events?active=true&closed=false&limit={}&offset={}",
                GAMMA_API_BASE, PAGE_SIZE, offset
            );

            info!("   Fetching events page {} (offset={})...", page + 1, offset);

            let response = self.gamma.http.get(&url)
                .send()
                .await?;

            if !response.status().is_success() {
                anyhow::bail!("Gamma API returned status: {}", response.status());
            }

            let events: Vec<crate::types::GammaEventResponse> = response.json().await?;
            let fetched_count = events.len();

            info!("   Page {}: fetched {} events", page + 1, fetched_count);

            all_events.extend(events);

            // If we got fewer than PAGE_SIZE, we've reached the end
            if fetched_count < PAGE_SIZE {
                info!("   Reached end of events (got {} < {})", fetched_count, PAGE_SIZE);
                break;
            }

            offset += PAGE_SIZE;
            page += 1;

            // Small delay between pages to be nice to the API
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        info!("📊 Gamma API returned {} total events across {} pages", all_events.len(), page + 1);

        // Convert to our internal format - extract markets from each event with their tags
        let mut poly_markets: Vec<PolymarketMarket> = Vec::new();
        let mut filtered_out_closed = 0;
        let mut filtered_out_no_tokens = 0;
        let mut total_markets_found = 0;

        for event in all_events {
            // Extract tags from this event (e.g., ["Tech", "AI", "Business"])
            let event_tags: Vec<String> = event.tags
                .as_ref()
                .map(|tags| {
                    tags.iter()
                        .filter_map(|t| t.label.clone())
                        .collect()
                })
                .unwrap_or_default();

            let event_title = event.title.clone();

            // Process each market within this event
            if let Some(markets) = event.markets {
                for market in markets {
                    total_markets_found += 1;

                    // Only include active, non-closed markets
                    if market.closed == Some(true) || market.active == Some(false) {
                        filtered_out_closed += 1;
                        continue;
                    }

                    // Parse token IDs
                    let token_ids: Vec<String> = market.clob_token_ids
                        .as_ref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default();

                    if token_ids.is_empty() {
                        filtered_out_no_tokens += 1;
                        continue;
                    }

                    // Parse outcomes
                    let outcomes: Vec<String> = market.outcomes
                        .as_ref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default();

                    poly_markets.push(PolymarketMarket {
                        question: market.question.unwrap_or_default(),
                        slug: market.slug.unwrap_or_default(),
                        outcomes,
                        token_ids,
                        tags: event_tags.clone(),
                        event_title: event_title.clone(),
                    });
                }
            }
        }

        info!("   Total markets in events: {}", total_markets_found);
        info!("   Filtered: {} closed/inactive, {} without tokens, {} remaining",
              filtered_out_closed, filtered_out_no_tokens, poly_markets.len());

        Ok(poly_markets)
    }

    /// Create a MarketPair from two competing outcomes within the same Polymarket market
    fn create_token_pair(
        &self,
        market: &PolymarketMarket,
        index_a: usize,
        index_b: usize,
    ) -> Option<MarketPair> {
        // Get the two token IDs from the same market
        let token_a = market.token_ids.get(index_a)?;
        let token_b = market.token_ids.get(index_b)?;

        // Get outcome names
        let outcome_a = market.outcomes.get(index_a)
            .map(|s| s.as_str())
            .unwrap_or("Outcome A");
        let outcome_b = market.outcomes.get(index_b)
            .map(|s| s.as_str())
            .unwrap_or("Outcome B");

        // Create unique pair ID from token prefixes
        let pair_id: Arc<str> = format!("poly-{}-{}",
            &token_a[..8.min(token_a.len())],
            &token_b[..8.min(token_b.len())]
        ).into();

        // Create description from market question and outcomes
        let description: Arc<str> = format!("{}: {} vs {}",
            market.question.chars().take(50).collect::<String>(),
            outcome_a,
            outcome_b
        ).into();

        // Join tags into a single string for category (e.g., "Tech, AI, Business")
        let category: Option<Arc<str>> = if market.tags.is_empty() {
            None
        } else {
            Some(market.tags.join(", ").into())
        };

        Some(MarketPair {
            pair_id,
            league: "polymarket".into(),
            market_type: MarketType::Moneyline,
            description,
            // Repurposed fields for Polymarket-only arbitrage:
            kalshi_event_ticker: market.slug.clone().into(),    // Market slug
            kalshi_market_ticker: token_a.clone().into(),       // Token A condition_id
            poly_slug: token_b.clone().into(),                  // Token B condition_id
            poly_yes_token: token_a.clone().into(),             // Token A address (for WebSocket lookup)
            poly_no_token: token_b.clone().into(),              // Token B address (for WebSocket lookup)
            line_value: None,
            team_suffix: None,
            category,
            event_title: market.event_title.clone().map(|s| s.into()),
        })
    }
}

/// Internal representation of a Polymarket market
#[derive(Debug, Clone)]
struct PolymarketMarket {
    question: String,
    slug: String,
    outcomes: Vec<String>,
    token_ids: Vec<String>,
    /// Tags from parent event (e.g., ["Tech", "AI", "Business"])
    tags: Vec<String>,
    /// Event title for additional context
    event_title: Option<String>,
}

// === Helpers ===

#[derive(Debug, Clone)]
struct ParsedKalshiTicker {
    date: String,  // "25DEC27"
    team1: String, // "CFC"
    team2: String, // "AVL"
}

/// Parse Kalshi event ticker like "KXEPLGAME-25DEC27CFCAVL" or "KXNCAAFGAME-25DEC27M-OHFRES"
fn parse_kalshi_event_ticker(ticker: &str) -> Option<ParsedKalshiTicker> {
    let parts: Vec<&str> = ticker.split('-').collect();
    if parts.len() < 2 {
        return None;
    }

    // Handle two formats:
    // 1. "KXEPLGAME-25DEC27CFCAVL" - date+teams in parts[1]
    // 2. "KXNCAAFGAME-25DEC27M-OHFRES" - date in parts[1], teams in parts[2]
    let (date, teams_part) = if parts.len() >= 3 && parts[2].len() >= 4 {
        // Format 2: 3-part ticker with separate teams section
        // parts[1] is like "25DEC27M" (date + optional suffix)
        let date_part = parts[1];
        let date = if date_part.len() >= 7 {
            date_part[..7].to_uppercase()
        } else {
            return None;
        };
        (date, parts[2])
    } else {
        // Format 1: 2-part ticker with combined date+teams
        let date_teams = parts[1];
        // Minimum: 7 (date) + 2 + 2 (min team codes) = 11
        if date_teams.len() < 11 {
            return None;
        }
        let date = date_teams[..7].to_uppercase();
        let teams = &date_teams[7..];
        (date, teams)
    };

    // Split team codes - try to find the best split point
    // Team codes range from 2-4 chars (e.g., OM, CFC, FRES)
    let (team1, team2) = split_team_codes(teams_part);

    Some(ParsedKalshiTicker { date, team1, team2 })
}

/// Split a combined team string into two team codes
/// Tries multiple split strategies based on string length
fn split_team_codes(teams: &str) -> (String, String) {
    let len = teams.len();

    // For 6 chars, could be 3+3, 2+4, or 4+2
    // For 5 chars, could be 2+3 or 3+2
    // For 4 chars, must be 2+2
    // For 7 chars, could be 3+4 or 4+3
    // For 8 chars, could be 4+4, 3+5, 5+3

    match len {
        4 => (teams[..2].to_uppercase(), teams[2..].to_uppercase()),
        5 => {
            // Prefer 2+3 (common for OM+ASM, OL+PSG)
            (teams[..2].to_uppercase(), teams[2..].to_uppercase())
        }
        6 => {
            // Check if it looks like 2+4 pattern (e.g., OHFRES = OH+FRES)
            // Common 2-letter codes: OM, OL, OH, SF, LA, NY, KC, TB, etc.
            let first_two = &teams[..2].to_uppercase();
            if is_likely_two_letter_code(first_two) {
                (first_two.clone(), teams[2..].to_uppercase())
            } else {
                // Default to 3+3
                (teams[..3].to_uppercase(), teams[3..].to_uppercase())
            }
        }
        7 => {
            // Could be 3+4 or 4+3 - prefer 3+4
            (teams[..3].to_uppercase(), teams[3..].to_uppercase())
        }
        _ if len >= 8 => {
            // 4+4 or longer
            (teams[..4].to_uppercase(), teams[4..].to_uppercase())
        }
        _ => {
            let mid = len / 2;
            (teams[..mid].to_uppercase(), teams[mid..].to_uppercase())
        }
    }
}

/// Check if a 2-letter code is a known/likely team abbreviation
fn is_likely_two_letter_code(code: &str) -> bool {
    matches!(
        code,
        // European football (Ligue 1, etc.)
        "OM" | "OL" | "FC" |
        // US sports common abbreviations
        "OH" | "SF" | "LA" | "NY" | "KC" | "TB" | "GB" | "NE" | "NO" | "LV" |
        // Generic short codes
        "BC" | "SC" | "AC" | "AS" | "US"
    )
}

/// Convert Kalshi date "25DEC27" to ISO "2025-12-27"
fn kalshi_date_to_iso(kalshi_date: &str) -> String {
    if kalshi_date.len() != 7 {
        return kalshi_date.to_string();
    }
    
    let year = format!("20{}", &kalshi_date[..2]);
    let month = match &kalshi_date[2..5].to_uppercase()[..] {
        "JAN" => "01", "FEB" => "02", "MAR" => "03", "APR" => "04",
        "MAY" => "05", "JUN" => "06", "JUL" => "07", "AUG" => "08",
        "SEP" => "09", "OCT" => "10", "NOV" => "11", "DEC" => "12",
        _ => "01",
    };
    let day = &kalshi_date[5..7];
    
    format!("{}-{}-{}", year, month, day)
}

/// Extract team suffix from market ticker (e.g., "KXEPLGAME-25DEC27CFCAVL-CFC" -> "CFC")
fn extract_team_suffix(ticker: &str) -> Option<String> {
    let mut splits = ticker.splitn(3, '-');
    splits.next()?; // series
    splits.next()?; // event
    splits.next().map(|s| s.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_kalshi_ticker() {
        let parsed = parse_kalshi_event_ticker("KXEPLGAME-25DEC27CFCAVL").unwrap();
        assert_eq!(parsed.date, "25DEC27");
        assert_eq!(parsed.team1, "CFC");
        assert_eq!(parsed.team2, "AVL");
    }
    
    #[test]
    fn test_kalshi_date_to_iso() {
        assert_eq!(kalshi_date_to_iso("25DEC27"), "2025-12-27");
        assert_eq!(kalshi_date_to_iso("25JAN01"), "2025-01-01");
    }
}
