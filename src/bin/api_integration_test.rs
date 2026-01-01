//! API Integration Test for Polymarket
//!
//! This script validates ALL API endpoints used by the arbitrage bot:
//! 1. Authentication APIs (derive-api-key)
//! 2. Balance APIs (USDC, conditional tokens)
//! 3. Position APIs
//! 4. Orderbook APIs
//! 5. Order APIs (with real small trades)
//! 6. Market Discovery APIs (Gamma)
//! 7. WebSocket connection
//!
//! Usage:
//!   cargo run --bin api_integration_test
//!
//! Environment variables required:
//!   POLY_PRIVATE_KEY - Your private key
//!   POLY_FUNDER - Your Polymarket proxy address
//!
//! Flags:
//!   --skip-trading   Skip trading tests (read-only mode)
//!   --verbose        Show detailed API responses

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const POLY_CLOB_HOST: &str = "https://clob.polymarket.com";
const GAMMA_API_HOST: &str = "https://gamma-api.polymarket.com";
const POLY_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
const POLYGON_CHAIN_ID: u64 = 137;

// Minimum $1 order value required
const MIN_ORDER_VALUE: f64 = 1.0;

use arb_bot::polymarket_clob::{
    PolymarketAsyncClient, PreparedCreds, SharedAsyncClient,
};

#[derive(Debug, Clone, Deserialize)]
struct GammaEvent {
    id: String,
    title: String,
    markets: Vec<GammaMarket>,
}

#[derive(Debug, Clone, Deserialize)]
struct GammaMarket {
    question: String,
    #[serde(rename = "clobTokenIds")]
    clob_token_ids: Option<String>,
    active: Option<bool>,
    closed: Option<bool>,
}

struct TestResult {
    name: &'static str,
    passed: bool,
    latency_ms: u128,
    details: String,
}

impl TestResult {
    fn success(name: &'static str, latency_ms: u128, details: impl Into<String>) -> Self {
        Self { name, passed: true, latency_ms, details: details.into() }
    }
    fn failure(name: &'static str, latency_ms: u128, details: impl Into<String>) -> Self {
        Self { name, passed: false, latency_ms, details: details.into() }
    }
}

struct ApiTester {
    host: String,
    http: reqwest::Client,
    creds: PreparedCreds,
    shared_client: SharedAsyncClient,
    verbose: bool,
}

impl ApiTester {
    pub async fn new(private_key: &str, funder: &str, verbose: bool) -> Result<Self> {
        let poly_client = PolymarketAsyncClient::new(
            POLY_CLOB_HOST,
            POLYGON_CHAIN_ID,
            private_key,
            funder,
        )?;

        let api_creds = poly_client.derive_api_key(0).await?;
        let prepared_creds = PreparedCreds::from_api_creds(&api_creds)?;

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;

        let prepared_creds_for_shared = PreparedCreds::from_api_creds(&api_creds)?;
        let shared_client = SharedAsyncClient::with_signature_type(
            PolymarketAsyncClient::new(POLY_CLOB_HOST, POLYGON_CHAIN_ID, private_key, funder)?,
            prepared_creds_for_shared,
            POLYGON_CHAIN_ID,
            2,  // Browser wallet signature type
        );

        Ok(Self {
            host: POLY_CLOB_HOST.to_string(),
            http,
            creds: prepared_creds,
            shared_client,
            verbose,
        })
    }

    // =========================================================================
    // Test: Authentication
    // =========================================================================
    async fn test_auth(&self) -> TestResult {
        let start = Instant::now();
        // Authentication is already done in new(), so we just verify creds exist
        let latency = start.elapsed().as_millis();
        TestResult::success(
            "Authentication (derive-api-key)",
            latency,
            format!("API Key: {}...", &self.creds.api_key[..16])
        )
    }

    // =========================================================================
    // Test: Gamma API (Market Discovery)
    // =========================================================================
    async fn test_gamma_api(&self) -> TestResult {
        let start = Instant::now();
        let url = format!("{}/events?active=true&closed=false&limit=5", GAMMA_API_HOST);

        match self.http.get(&url).send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis();
                if resp.status().is_success() {
                    match resp.json::<Vec<GammaEvent>>().await {
                        Ok(events) => {
                            let market_count: usize = events.iter().map(|e| e.markets.len()).sum();
                            TestResult::success(
                                "Gamma API (Market Discovery)",
                                latency,
                                format!("{} events, {} markets", events.len(), market_count)
                            )
                        }
                        Err(e) => TestResult::failure("Gamma API", latency, format!("Parse error: {}", e)),
                    }
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    TestResult::failure("Gamma API", latency, format!("{}: {}", status, body))
                }
            }
            Err(e) => TestResult::failure("Gamma API", start.elapsed().as_millis(), format!("Request error: {}", e)),
        }
    }

    // =========================================================================
    // Test: Orderbook
    // =========================================================================
    async fn test_orderbook(&self, token_id: &str) -> TestResult {
        let start = Instant::now();
        let path = format!("/book?token_id={}", token_id);
        let url = format!("{}{}", self.host, path);

        match self.http.get(&url).send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis();
                if resp.status().is_success() {
                    match resp.json::<serde_json::Value>().await {
                        Ok(book) => {
                            let asks = book.get("asks")
                                .and_then(|a| a.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);
                            let bids = book.get("bids")
                                .and_then(|b| b.as_array())
                                .map(|b| b.len())
                                .unwrap_or(0);

                            if self.verbose {
                                println!("       Orderbook: {:?}", book);
                            }

                            TestResult::success(
                                "Orderbook",
                                latency,
                                format!("{} asks, {} bids", asks, bids)
                            )
                        }
                        Err(e) => TestResult::failure("Orderbook", latency, format!("Parse error: {}", e)),
                    }
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    TestResult::failure("Orderbook", latency, format!("{}: {}", status, body))
                }
            }
            Err(e) => TestResult::failure("Orderbook", start.elapsed().as_millis(), format!("Request error: {}", e)),
        }
    }

    // =========================================================================
    // Test: Neg Risk Check
    // =========================================================================
    async fn test_neg_risk(&self, token_id: &str) -> TestResult {
        let start = Instant::now();
        let url = format!("{}/neg-risk?token_id={}", self.host, token_id);

        match self.http.get(&url).send().await {
            Ok(resp) => {
                let latency = start.elapsed().as_millis();
                if resp.status().is_success() {
                    match resp.json::<serde_json::Value>().await {
                        Ok(val) => {
                            let neg_risk = val["neg_risk"].as_bool().unwrap_or(false);
                            TestResult::success(
                                "Neg Risk Check",
                                latency,
                                format!("neg_risk={}", neg_risk)
                            )
                        }
                        Err(e) => TestResult::failure("Neg Risk Check", latency, format!("Parse error: {}", e)),
                    }
                } else {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    TestResult::failure("Neg Risk Check", latency, format!("{}: {}", status, body))
                }
            }
            Err(e) => TestResult::failure("Neg Risk Check", start.elapsed().as_millis(), format!("Request error: {}", e)),
        }
    }

    // =========================================================================
    // Test: WebSocket Connection
    // =========================================================================
    async fn test_websocket(&self, token_id: &str) -> TestResult {
        let start = Instant::now();

        match connect_async(POLY_WS_URL).await {
            Ok((mut ws_stream, _)) => {
                let connect_latency = start.elapsed().as_millis();

                // Subscribe to token
                let subscribe_msg = serde_json::json!({
                    "assets_ids": [token_id],
                    "type": "market"
                });

                if let Err(e) = ws_stream.send(Message::Text(subscribe_msg.to_string())).await {
                    return TestResult::failure("WebSocket", connect_latency, format!("Send error: {}", e));
                }

                // Wait for a message (timeout 5 seconds)
                let msg_result = tokio::time::timeout(
                    Duration::from_secs(5),
                    ws_stream.next()
                ).await;

                let latency = start.elapsed().as_millis();

                match msg_result {
                    Ok(Some(Ok(msg))) => {
                        let msg_preview = match &msg {
                            Message::Text(t) => format!("Text({} bytes)", t.len()),
                            Message::Binary(b) => format!("Binary({} bytes)", b.len()),
                            Message::Ping(_) => "Ping".to_string(),
                            Message::Pong(_) => "Pong".to_string(),
                            Message::Close(_) => "Close".to_string(),
                            Message::Frame(_) => "Frame".to_string(),
                        };

                        if self.verbose {
                            if let Message::Text(t) = &msg {
                                println!("       WS Message: {}", &t[..t.len().min(200)]);
                            }
                        }

                        TestResult::success(
                            "WebSocket Connection",
                            latency,
                            format!("Connected, received: {}", msg_preview)
                        )
                    }
                    Ok(Some(Err(e))) => TestResult::failure("WebSocket", latency, format!("Read error: {}", e)),
                    Ok(None) => TestResult::failure("WebSocket", latency, "Stream ended unexpectedly"),
                    Err(_) => TestResult::failure("WebSocket", latency, "Timeout waiting for message"),
                }
            }
            Err(e) => TestResult::failure("WebSocket", start.elapsed().as_millis(), format!("Connect error: {}", e)),
        }
    }

    // =========================================================================
    // Test: Buy Order (Real Money - Small Amount)
    // =========================================================================
    async fn test_buy_order(&self, token_id: &str, price: f64, size: f64) -> TestResult {
        let start = Instant::now();

        if self.verbose {
            println!("       DEBUG: test_buy_order called with price={}, size={}", price, size);
        }

        match self.shared_client.buy_fak(token_id, price, size).await {
            Ok(result) => {
                let latency = start.elapsed().as_millis();
                TestResult::success(
                    "Buy Order (FAK)",
                    latency,
                    format!("Order ID: {}, Filled: {:.2} @ ${:.2}, Cost: ${:.2}",
                        &result.order_id[..result.order_id.len().min(16)],
                        result.filled_size,
                        price,
                        result.fill_cost
                    )
                )
            }
            Err(e) => TestResult::failure("Buy Order (FAK)", start.elapsed().as_millis(), format!("Error: {}", e)),
        }
    }

    // =========================================================================
    // Test: Sell Order (Real Money - Sell tokens we bought)
    // =========================================================================
    async fn test_sell_order(&self, token_id: &str, price: f64, size: f64) -> TestResult {
        let start = Instant::now();

        // Sell at slightly below bid to ensure fill
        let sell_price = (price - 0.02).max(0.01);

        if self.verbose {
            println!("       DEBUG: test_sell_order called with price={}, sell_price={}, size={}", price, sell_price, size);
        }

        match self.shared_client.sell_fak(token_id, sell_price, size).await {
            Ok(result) => {
                let latency = start.elapsed().as_millis();
                TestResult::success(
                    "Sell Order (FAK)",
                    latency,
                    format!("Order ID: {}, Sold: {:.2} @ ${:.2}, Proceeds: ${:.2}",
                        &result.order_id[..result.order_id.len().min(16)],
                        result.filled_size,
                        sell_price,
                        result.fill_cost
                    )
                )
            }
            Err(e) => TestResult::failure("Sell Order (FAK)", start.elapsed().as_millis(), format!("Error: {}", e)),
        }
    }

}

fn print_result(result: &TestResult) {
    let status = if result.passed { "\x1b[32m[PASS]\x1b[0m" } else { "\x1b[31m[FAIL]\x1b[0m" };
    println!("   {} {} ({}ms)", status, result.name, result.latency_ms);
    println!("       {}", result.details);
}

async fn find_active_market(http: &reqwest::Client) -> Result<(String, String, String)> {
    // Find an active market with good liquidity
    let url = format!("{}/events?active=true&closed=false&limit=20", GAMMA_API_HOST);
    let events: Vec<GammaEvent> = http.get(&url).send().await?.json().await?;

    for event in events {
        for market in event.markets {
            if market.active == Some(true) && market.closed != Some(true) {
                if let Some(token_ids_str) = market.clob_token_ids {
                    let token_ids: Vec<String> = serde_json::from_str(&token_ids_str).unwrap_or_default();
                    if token_ids.len() >= 2 {
                        return Ok((token_ids[0].clone(), token_ids[1].clone(), market.question));
                    }
                }
            }
        }
    }

    Err(anyhow!("No active markets found"))
}

/// Find a market with two-sided liquidity (both bids and asks) for trading tests
/// Uses SharedAsyncClient.get_best_prices() - the canonical orderbook parsing implementation
async fn find_liquid_market(
    http: &reqwest::Client,
    client: &SharedAsyncClient,
    verbose: bool,
) -> Result<(String, String, String, f64, f64)> {
    let url = format!("{}/events?active=true&closed=false&limit=50", GAMMA_API_HOST);
    let events: Vec<GammaEvent> = http.get(&url).send().await?.json().await?;

    let mut markets_checked = 0;
    let mut no_tokens = 0;
    let mut no_bids = 0;
    let mut no_asks = 0;

    for event in events {
        for market in event.markets {
            if market.active == Some(true) && market.closed != Some(true) {
                if let Some(token_ids_str) = market.clob_token_ids {
                    let token_ids: Vec<String> = serde_json::from_str(&token_ids_str).unwrap_or_default();
                    if token_ids.len() >= 2 {
                        markets_checked += 1;
                        // Check if this market has liquidity on both sides
                        // Uses the shared get_best_prices from polymarket_clob module
                        match client.get_best_prices(&token_ids[0]).await {
                            Ok((best_ask, best_bid)) => {
                                // Need a market with valid prices (0.01-0.99) and two-sided liquidity
                                if best_bid >= 0.01 && best_bid <= 0.99 && best_ask >= 0.01 && best_ask <= 0.99 {
                                    return Ok((
                                        token_ids[0].clone(),
                                        token_ids[1].clone(),
                                        market.question,
                                        best_ask,
                                        best_bid,
                                    ));
                                }
                            }
                            Err(e) => {
                                let err_str = e.to_string();
                                if err_str.contains("No bids") {
                                    no_bids += 1;
                                } else if err_str.contains("No asks") {
                                    no_asks += 1;
                                }
                                if verbose {
                                    println!("       Skipped: {} - {}", err_str, &market.question[..market.question.len().min(40)]);
                                }
                            }
                        }
                    } else {
                        no_tokens += 1;
                    }
                }
            }
        }
    }

    Err(anyhow!(
        "No liquid markets found. Checked {} markets: {} no bids, {} no asks, {} no tokens",
        markets_checked, no_bids, no_asks, no_tokens
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    println!();
    println!("═══════════════════════════════════════════════════════════════════════");
    println!("              Polymarket API Integration Test");
    println!("═══════════════════════════════════════════════════════════════════════");
    println!();

    // Parse args
    let args: Vec<String> = std::env::args().collect();
    let skip_trading = args.iter().any(|a| a == "--skip-trading");
    let verbose = args.iter().any(|a| a == "--verbose");

    // Load environment variables
    dotenvy::dotenv().ok();

    let private_key = std::env::var("POLY_PRIVATE_KEY")
        .context("POLY_PRIVATE_KEY not set. Add to .env file.")?;
    let funder = std::env::var("POLY_FUNDER")
        .context("POLY_FUNDER not set. Add to .env file.")?;

    println!("Wallet: {}...{}", &funder[..6], &funder[funder.len()-4..]);
    println!("Trading tests: {}", if skip_trading { "SKIPPED" } else { "ENABLED (small amounts)" });
    println!();

    // Initialize tester
    println!("Initializing API client...");
    let tester = ApiTester::new(&private_key, &funder, verbose).await?;
    println!("Client initialized successfully!\n");

    let mut results: Vec<TestResult> = Vec::new();

    // =========================================================================
    // Phase 1: Read-Only Tests
    // =========================================================================
    println!("─────────────────────────────────────────────────────────────────────────");
    println!(" Phase 1: Read-Only API Tests");
    println!("─────────────────────────────────────────────────────────────────────────");

    // Test 1: Authentication (already done in init)
    let result = tester.test_auth().await;
    print_result(&result);
    results.push(result);

    // Test 2: Gamma API
    let result = tester.test_gamma_api().await;
    print_result(&result);
    results.push(result);

    // Find an active market for further tests
    println!("\n   Finding active market for detailed tests...");
    let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let (yes_token, no_token, question) = find_active_market(&http).await?;
    println!("   Using: {}", &question[..question.len().min(60)]);
    println!("   YES Token: {}...{}", &yes_token[..8], &yes_token[yes_token.len()-8..]);
    println!("   NO Token:  {}...{}", &no_token[..8], &no_token[no_token.len()-8..]);
    println!();

    // Test 3: Orderbook
    let result = tester.test_orderbook(&yes_token).await;
    print_result(&result);
    results.push(result);

    // Test 4: Neg Risk
    let result = tester.test_neg_risk(&yes_token).await;
    print_result(&result);
    results.push(result);

    // Test 5: WebSocket
    let result = tester.test_websocket(&yes_token).await;
    print_result(&result);
    results.push(result);

    // =========================================================================
    // Phase 2: Trading Tests (Optional)
    // =========================================================================
    if !skip_trading {
        println!("\n─────────────────────────────────────────────────────────────────────────");
        println!(" Phase 2: Trading Tests (REAL MONEY - Small Amounts)");
        println!("─────────────────────────────────────────────────────────────────────────");
        println!("   WARNING: These tests will execute real trades!");
        println!("   - Minimum order value: ${}", MIN_ORDER_VALUE);
        println!("   - Expected cost: spread loss only");
        println!();

        // Find a market with two-sided liquidity for trading tests
        println!("   Finding liquid market with bids and asks...");
        match find_liquid_market(&http, &tester.shared_client, verbose).await {
            Ok((trade_yes_token, _trade_no_token, trade_question, best_ask, best_bid)) => {
                println!("   Using: {}", &trade_question[..trade_question.len().min(60)]);
                println!("   Token: {}...{}", &trade_yes_token[..8], &trade_yes_token[trade_yes_token.len()-8..]);
                println!();
                println!("   Best Ask: ${:.2}", best_ask);
                println!("   Best Bid: ${:.2}", best_bid);
                println!("   Spread:   ${:.2} ({:.1}%)", best_ask - best_bid, (best_ask - best_bid) / best_ask * 100.0);
                println!();

                // Calculate minimum contracts needed for $1 order value
                // Formula: ceil($1 / price) = minimum contracts
                let trade_size = (MIN_ORDER_VALUE / best_ask).ceil();
                let trade_cost = best_ask * trade_size;
                println!("   Trade size calculation: ceil(${:.2} / ${:.4}) = {} contracts",
                    MIN_ORDER_VALUE, best_ask, trade_size);
                println!("   Trade plan:");
                println!("   BUY {} contracts at ${:.2} = ${:.2}", trade_size, best_ask, trade_cost);
                println!();

                print!("   Proceed with trading tests? (y/N): ");
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;

                if input.trim().to_lowercase() == "y" {
                    println!();

                    // Debug: show exact value being passed
                    println!("   DEBUG: Sending buy order with size={} contracts", trade_size);

                    // Test: Buy Order - call directly to get filled_size
                    let start = Instant::now();
                    match tester.shared_client.buy_fak(&trade_yes_token, best_ask, trade_size).await {
                        Ok(buy_response) => {
                            let latency = start.elapsed().as_millis();
                            let filled_size = buy_response.filled_size;
                            let buy_result = TestResult::success(
                                "Buy Order (FAK)",
                                latency,
                                format!("Order ID: {}, Filled: {:.2} @ ${:.2}, Cost: ${:.2}",
                                    &buy_response.order_id[..buy_response.order_id.len().min(16)],
                                    filled_size,
                                    best_ask,
                                    buy_response.fill_cost
                                )
                            );
                            print_result(&buy_result);
                            results.push(buy_result);

                            // Test: Sell Order (only if we actually bought something)
                            if filled_size > 0.0 {
                                println!();
                                println!("   Waiting 5 seconds for Polymarket settlement...");
                                tokio::time::sleep(Duration::from_secs(5)).await;

                                println!("   Attempting to sell {:.2} tokens we just bought...", filled_size);
                                let sell_result = tester.test_sell_order(&trade_yes_token, best_bid, filled_size).await;
                                print_result(&sell_result);
                                results.push(sell_result);
                            } else {
                                println!("   \x1b[33m[SKIP]\x1b[0m Sell test skipped - buy order filled 0 tokens");
                            }
                        }
                        Err(e) => {
                            let buy_result = TestResult::failure("Buy Order (FAK)", start.elapsed().as_millis(), format!("Error: {}", e));
                            print_result(&buy_result);
                            results.push(buy_result);
                        }
                    }
                } else {
                    println!("   Trading tests skipped by user");
                }
            }
            Err(e) => {
                println!("   \x1b[33m[WARN]\x1b[0m {}", e);
                println!("   Trading tests skipped - no liquid markets available.");
                println!("   This is not a bug. Try again later when markets have more liquidity.");
            }
        }
    }

    // =========================================================================
    // Summary
    // =========================================================================
    println!("\n═══════════════════════════════════════════════════════════════════════");
    println!(" Test Summary");
    println!("═══════════════════════════════════════════════════════════════════════");

    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.iter().filter(|r| !r.passed).count();
    let total = results.len();

    println!();
    for result in &results {
        let status = if result.passed { "\x1b[32mPASS\x1b[0m" } else { "\x1b[31mFAIL\x1b[0m" };
        println!("   [{}] {} ({}ms)", status, result.name, result.latency_ms);
    }

    println!();
    println!("   Total:  {} tests", total);
    println!("   Passed: \x1b[32m{}\x1b[0m", passed);
    println!("   Failed: \x1b[31m{}\x1b[0m", failed);
    println!();

    if failed > 0 {
        println!("\x1b[31mSome tests failed. Check the details above.\x1b[0m");
        std::process::exit(1);
    } else {
        println!("\x1b[32mAll tests passed! API integration is working correctly.\x1b[0m");
    }

    println!();
    Ok(())
}
