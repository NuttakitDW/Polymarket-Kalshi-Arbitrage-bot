//! Test Trading Script for Polymarket
//!
//! This script tests basic trading functionality:
//! 1. Check USDC balance and allowance
//! 2. Buy tokens (small test amount)
//! 3. Sell tokens
//!
//! Usage:
//!   cargo run --bin test_trading
//!
//! Environment variables required:
//!   POLY_PRIVATE_KEY - Your private key (from https://reveal.magic.link/polymarket)
//!   POLY_FUNDER - Your Polymarket proxy address (shown below profile picture on Polymarket)

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::io::{self, Write};

/// Polymarket CLOB API host
const POLY_CLOB_HOST: &str = "https://clob.polymarket.com";
/// Polygon chain ID
const POLYGON_CHAIN_ID: u64 = 137;

// Re-use the library's client code
use arb_bot::polymarket_clob::{
    PolymarketAsyncClient, PreparedCreds, SharedAsyncClient,
};

/// Balance and allowance response from Polymarket
#[derive(Debug, Clone, Deserialize)]
pub struct BalanceAllowance {
    #[serde(default)]
    pub balance: String,
    #[serde(default)]
    pub allowance: String,
}

/// Open position in a market
#[derive(Debug, Clone, Deserialize)]
pub struct OpenPosition {
    pub asset: String,
    pub size: String,
    #[serde(rename = "avgPrice")]
    pub avg_price: Option<String>,
    #[serde(rename = "unrealizedPnl")]
    pub unrealized_pnl: Option<String>,
    #[serde(rename = "realizedPnl")]
    pub realized_pnl: Option<String>,
    #[serde(rename = "curPrice")]
    pub cur_price: Option<String>,
}

/// Market info for display
#[derive(Debug, Clone, Deserialize)]
pub struct MarketInfo {
    pub condition_id: String,
    pub question: String,
    pub tokens: Vec<TokenInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenInfo {
    pub token_id: String,
    pub outcome: String,
    pub price: Option<f64>,
}

/// Extended client with balance checking
pub struct TestTradingClient {
    host: String,
    http: reqwest::Client,
    wallet_address: String,
    creds: PreparedCreds,
    shared_client: SharedAsyncClient,
}

impl TestTradingClient {
    pub async fn new(private_key: &str, funder: &str) -> Result<Self> {
        println!("🔐 Initializing Polymarket client...");

        let poly_client = PolymarketAsyncClient::new(
            POLY_CLOB_HOST,
            POLYGON_CHAIN_ID,
            private_key,
            funder,
        )?;

        println!("🔑 Creating/Deriving API credentials...");
        let api_creds = poly_client.create_or_derive_api_key(0).await?;
        println!("   API Key: {}...", &api_creds.api_key[..16]);
        let prepared_creds = PreparedCreds::from_api_creds(&api_creds)?;

        let wallet_address = funder.to_string();

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let prepared_creds_for_shared = PreparedCreds::from_api_creds(&api_creds)?;
        // signature_type=2 for MetaMask/Browser wallet with Polymarket proxy
        let shared_client = SharedAsyncClient::with_signature_type(
            PolymarketAsyncClient::new(POLY_CLOB_HOST, POLYGON_CHAIN_ID, private_key, funder)?,
            prepared_creds_for_shared,
            POLYGON_CHAIN_ID,
            2,  // Browser wallet signature type
        );

        println!("✅ Client initialized for: {}...{}", &funder[..6], &funder[funder.len()-4..]);

        Ok(Self {
            host: POLY_CLOB_HOST.to_string(),
            http,
            wallet_address,
            creds: prepared_creds,
            shared_client,
        })
    }

    /// Build L2 headers for authenticated requests
    fn build_l2_headers(&self, method: &str, path: &str, body: Option<&str>) -> Result<reqwest::header::HeaderMap> {
        use reqwest::header::{HeaderMap, HeaderValue};
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mut message = format!("{}{}{}", timestamp, method, path);
        if let Some(b) = body { message.push_str(b); }

        let sig_b64 = self.creds.sign_b64(message.as_bytes());

        let mut headers = HeaderMap::new();
        headers.insert("POLY_ADDRESS", HeaderValue::from_str(&self.wallet_address)?);
        headers.insert("POLY_SIGNATURE", HeaderValue::from_str(&sig_b64)?);
        headers.insert("POLY_TIMESTAMP", HeaderValue::from_str(&timestamp.to_string())?);
        headers.insert("POLY_API_KEY", self.creds.api_key_header());
        headers.insert("POLY_PASSPHRASE", self.creds.passphrase_header());
        headers.insert("User-Agent", HeaderValue::from_static("py_clob_client"));
        headers.insert("Accept", HeaderValue::from_static("*/*"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));

        Ok(headers)
    }

    /// Check USDC balance and allowance
    pub async fn get_balance_allowance(&self) -> Result<BalanceAllowance> {
        let path = "/balance-allowance";
        let query = format!("{}?asset_type=USDC", path);
        let url = format!("{}{}", self.host, query);
        let headers = self.build_l2_headers("GET", &query, None)?;

        let resp = self.http.get(&url).headers(headers).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("balance-allowance failed: {} - {}", status, body));
        }

        Ok(resp.json().await?)
    }

    /// Get conditional token balance for a specific token_id
    pub async fn get_conditional_balance(&self, token_id: &str) -> Result<BalanceAllowance> {
        let path = format!("/balance-allowance?asset_type=CONDITIONAL&token_id={}", token_id);
        let url = format!("{}{}", self.host, path);
        let headers = self.build_l2_headers("GET", &path, None)?;

        let resp = self.http.get(&url).headers(headers).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("conditional balance failed: {} - {}", status, body));
        }

        Ok(resp.json().await?)
    }

    /// Get all open positions
    pub async fn get_open_positions(&self) -> Result<Vec<OpenPosition>> {
        let path = "/data/positions";
        let url = format!("{}{}", self.host, path);
        let headers = self.build_l2_headers("GET", path, None)?;

        let resp = self.http.get(&url).headers(headers).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("get positions failed: {} - {}", status, body));
        }

        Ok(resp.json().await?)
    }

    /// Get current best prices for a token (from orderbook)
    pub async fn get_orderbook(&self, token_id: &str) -> Result<serde_json::Value> {
        let path = format!("/book?token_id={}", token_id);
        let url = format!("{}{}", self.host, path);

        let resp = self.http.get(&url).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("get orderbook failed: {} - {}", status, body));
        }

        Ok(resp.json().await?)
    }

    /// Buy tokens using FAK (Fill And Kill) order
    pub async fn buy(&self, token_id: &str, price: f64, size: f64) -> Result<String> {
        println!("\n📈 Placing BUY order...");
        println!("   Token: {}...{}", &token_id[..8], &token_id[token_id.len()-8..]);
        println!("   Price: ${:.2}", price);
        println!("   Size: {} contracts", size);
        println!("   Total cost: ${:.2}", price * size);

        let result = self.shared_client.buy_fak(token_id, price, size).await?;

        println!("✅ Order filled!");
        println!("   Order ID: {}", result.order_id);
        println!("   Filled: {} contracts", result.filled_size);
        println!("   Cost: ${:.2}", result.fill_cost);

        Ok(result.order_id)
    }

    /// Sell tokens using FAK (Fill And Kill) order
    pub async fn sell(&self, token_id: &str, price: f64, size: f64) -> Result<String> {
        println!("\n📉 Placing SELL order...");
        println!("   Token: {}...{}", &token_id[..8], &token_id[token_id.len()-8..]);
        println!("   Price: ${:.2}", price);
        println!("   Size: {} contracts", size);
        println!("   Expected revenue: ${:.2}", price * size);

        let result = self.shared_client.sell_fak(token_id, price, size).await?;

        println!("✅ Order filled!");
        println!("   Order ID: {}", result.order_id);
        println!("   Sold: {} contracts", result.filled_size);
        println!("   Revenue: ${:.2}", result.fill_cost);

        Ok(result.order_id)
    }
}

fn prompt(msg: &str) -> String {
    print!("{}", msg);
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().to_string()
}

fn prompt_f64(msg: &str) -> f64 {
    loop {
        let input = prompt(msg);
        match input.parse::<f64>() {
            Ok(v) => return v,
            Err(_) => println!("❌ Invalid number, try again"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("═══════════════════════════════════════════════════════════════");
    println!("           🧪 Polymarket Test Trading Script");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    // Load environment variables
    dotenvy::dotenv().ok();

    let private_key = std::env::var("POLY_PRIVATE_KEY")
        .context("POLY_PRIVATE_KEY not set in .env")?;
    let funder = std::env::var("POLY_FUNDER")
        .context("POLY_FUNDER not set in .env")?;

    // Initialize client
    let client = TestTradingClient::new(&private_key, &funder).await?;

    // Main menu loop
    loop {
        println!();
        println!("═══════════════════════════════════════════════════════════════");
        println!("  What would you like to do?");
        println!("═══════════════════════════════════════════════════════════════");
        println!("  1. Check USDC balance");
        println!("  2. Check token balance (for a specific token)");
        println!("  3. View open positions");
        println!("  4. View orderbook for a token");
        println!("  5. Buy tokens");
        println!("  6. Sell tokens");
        println!("  7. Quick test: Buy and Sell 1 contract");
        println!("  0. Exit");
        println!();

        let choice = prompt("Enter choice: ");

        match choice.as_str() {
            "1" => {
                println!("\n💰 Checking USDC balance...");
                match client.get_balance_allowance().await {
                    Ok(bal) => {
                        let balance: f64 = bal.balance.parse().unwrap_or(0.0) / 1_000_000.0;
                        let allowance: f64 = bal.allowance.parse().unwrap_or(0.0) / 1_000_000.0;
                        println!("✅ USDC Balance: ${:.2}", balance);
                        println!("   Allowance: ${:.2}", allowance);
                    }
                    Err(e) => println!("❌ Error: {}", e),
                }
            }

            "2" => {
                let token_id = prompt("Enter token ID: ");
                if token_id.is_empty() {
                    println!("❌ Token ID required");
                    continue;
                }

                println!("\n💰 Checking token balance...");
                match client.get_conditional_balance(&token_id).await {
                    Ok(bal) => {
                        let balance: f64 = bal.balance.parse().unwrap_or(0.0) / 1_000_000.0;
                        let allowance: f64 = bal.allowance.parse().unwrap_or(0.0) / 1_000_000.0;
                        println!("✅ Token Balance: {} contracts", balance);
                        println!("   Allowance: {}", allowance);
                    }
                    Err(e) => println!("❌ Error: {}", e),
                }
            }

            "3" => {
                println!("\n📊 Fetching open positions...");
                match client.get_open_positions().await {
                    Ok(positions) => {
                        if positions.is_empty() {
                            println!("   No open positions");
                        } else {
                            for pos in positions {
                                println!("   Token: {}...{}", &pos.asset[..8], &pos.asset[pos.asset.len()-8..]);
                                println!("   Size: {} contracts", pos.size);
                                if let Some(avg) = pos.avg_price {
                                    println!("   Avg Price: ${}", avg);
                                }
                                if let Some(cur) = pos.cur_price {
                                    println!("   Current Price: ${}", cur);
                                }
                                println!();
                            }
                        }
                    }
                    Err(e) => println!("❌ Error: {}", e),
                }
            }

            "4" => {
                let token_id = prompt("Enter token ID: ");
                if token_id.is_empty() {
                    println!("❌ Token ID required");
                    continue;
                }

                println!("\n📖 Fetching orderbook...");
                match client.get_orderbook(&token_id).await {
                    Ok(book) => {
                        println!("{}", serde_json::to_string_pretty(&book)?);
                    }
                    Err(e) => println!("❌ Error: {}", e),
                }
            }

            "5" => {
                let token_id = prompt("Enter token ID to buy: ");
                if token_id.is_empty() {
                    println!("❌ Token ID required");
                    continue;
                }

                // Show current orderbook first
                println!("\n📖 Current orderbook:");
                if let Ok(book) = client.get_orderbook(&token_id).await {
                    if let Some(asks) = book.get("asks") {
                        println!("   Best asks (sell offers):");
                        if let Some(arr) = asks.as_array() {
                            for (i, ask) in arr.iter().take(3).enumerate() {
                                if let (Some(p), Some(s)) = (ask.get("price"), ask.get("size")) {
                                    println!("   {}. ${} x {} contracts", i+1, p, s);
                                }
                            }
                        }
                    }
                }

                let price = prompt_f64("Enter price (0.01-0.99): ");
                if price < 0.01 || price > 0.99 {
                    println!("❌ Price must be between 0.01 and 0.99");
                    continue;
                }

                let size = prompt_f64("Enter size (contracts): ");
                if size < 1.0 {
                    println!("❌ Size must be at least 1");
                    continue;
                }

                let confirm = prompt(&format!("Confirm BUY {} @ ${:.2} (total ${:.2})? (y/n): ", size, price, price * size));
                if confirm.to_lowercase() != "y" {
                    println!("❌ Cancelled");
                    continue;
                }

                match client.buy(&token_id, price, size).await {
                    Ok(_) => println!("🎉 Buy order complete!"),
                    Err(e) => println!("❌ Error: {}", e),
                }
            }

            "6" => {
                let token_id = prompt("Enter token ID to sell: ");
                if token_id.is_empty() {
                    println!("❌ Token ID required");
                    continue;
                }

                // Show current orderbook first
                println!("\n📖 Current orderbook:");
                if let Ok(book) = client.get_orderbook(&token_id).await {
                    if let Some(bids) = book.get("bids") {
                        println!("   Best bids (buy offers):");
                        if let Some(arr) = bids.as_array() {
                            for (i, bid) in arr.iter().take(3).enumerate() {
                                if let (Some(p), Some(s)) = (bid.get("price"), bid.get("size")) {
                                    println!("   {}. ${} x {} contracts", i+1, p, s);
                                }
                            }
                        }
                    }
                }

                let price = prompt_f64("Enter price (0.01-0.99): ");
                if price < 0.01 || price > 0.99 {
                    println!("❌ Price must be between 0.01 and 0.99");
                    continue;
                }

                let size = prompt_f64("Enter size (contracts): ");
                if size < 1.0 {
                    println!("❌ Size must be at least 1");
                    continue;
                }

                let confirm = prompt(&format!("Confirm SELL {} @ ${:.2} (total ${:.2})? (y/n): ", size, price, price * size));
                if confirm.to_lowercase() != "y" {
                    println!("❌ Cancelled");
                    continue;
                }

                match client.sell(&token_id, price, size).await {
                    Ok(_) => println!("🎉 Sell order complete!"),
                    Err(e) => println!("❌ Error: {}", e),
                }
            }

            "7" => {
                println!("\n🧪 Quick Test: Buy and Sell 1 contract");
                println!("   This will buy 1 contract at a low price and immediately sell it.");
                println!();

                let token_id = prompt("Enter token ID for test: ");
                if token_id.is_empty() {
                    println!("❌ Token ID required");
                    continue;
                }

                // Get orderbook to find good prices
                println!("\n📖 Fetching orderbook...");
                let book = client.get_orderbook(&token_id).await?;

                let best_ask = book.get("asks")
                    .and_then(|a| a.as_array())
                    .and_then(|a| a.first())
                    .and_then(|a| a.get("price"))
                    .and_then(|p| p.as_str())
                    .and_then(|p| p.parse::<f64>().ok());

                let best_bid = book.get("bids")
                    .and_then(|b| b.as_array())
                    .and_then(|b| b.first())
                    .and_then(|b| b.get("price"))
                    .and_then(|p| p.as_str())
                    .and_then(|p| p.parse::<f64>().ok());

                match (best_ask, best_bid) {
                    (Some(ask), Some(bid)) => {
                        println!("   Best Ask: ${:.2}", ask);
                        println!("   Best Bid: ${:.2}", bid);
                        println!("   Spread: ${:.2} ({:.1}%)", ask - bid, (ask - bid) / ask * 100.0);
                        println!();
                        println!("   Test plan:");
                        println!("   1. Buy 1 contract at ${:.2} (market buy)", ask);
                        println!("   2. Sell 1 contract at ${:.2} (market sell)", bid);
                        println!("   Expected loss from spread: ${:.2}", ask - bid);
                        println!();

                        let confirm = prompt("Proceed with test? (y/n): ");
                        if confirm.to_lowercase() != "y" {
                            println!("❌ Cancelled");
                            continue;
                        }

                        // Buy
                        println!("\n⏳ Step 1: Buying...");
                        match client.buy(&token_id, ask, 1.0).await {
                            Ok(_) => {
                                println!("✅ Buy complete!");

                                // Small delay
                                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                                // Sell
                                println!("\n⏳ Step 2: Selling...");
                                match client.sell(&token_id, bid, 1.0).await {
                                    Ok(_) => {
                                        println!("✅ Sell complete!");
                                        println!("\n🎉 Test complete! You have successfully:");
                                        println!("   ✓ Connected to Polymarket API");
                                        println!("   ✓ Placed a buy order");
                                        println!("   ✓ Placed a sell order");
                                        println!("   You're ready for real trading!");
                                    }
                                    Err(e) => println!("❌ Sell failed: {}", e),
                                }
                            }
                            Err(e) => println!("❌ Buy failed: {}", e),
                        }
                    }
                    _ => {
                        println!("❌ Could not get orderbook prices. Market may be inactive.");
                    }
                }
            }

            "0" => {
                println!("\n👋 Goodbye!");
                break;
            }

            _ => println!("❌ Invalid choice"),
        }
    }

    Ok(())
}
