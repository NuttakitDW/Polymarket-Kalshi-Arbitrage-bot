//! Simple script to show order book prices and sizes from Polymarket.
//!
//! Usage: cargo run --bin show_orderbook

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use std::collections::HashMap;
use std::time::Duration;

const POLYMARKET_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
const GAMMA_API: &str = "https://gamma-api.polymarket.com";

#[derive(Deserialize, Debug)]
struct BookSnapshot {
    asset_id: String,
    #[serde(default)]
    asks: Vec<PriceLevel>,
}

#[derive(Deserialize, Debug)]
struct PriceLevel {
    price: String,
    size: String,
}

#[derive(Serialize)]
struct SubscribeCmd {
    assets_ids: Vec<String>,
    #[serde(rename = "type")]
    sub_type: &'static str,
}

#[derive(Deserialize, Debug)]
struct GammaEvent {
    title: String,
    markets: Vec<GammaMarket>,
}

#[derive(Deserialize, Debug)]
struct GammaMarket {
    question: String,
    #[serde(rename = "clobTokenIds")]
    clob_token_ids: Option<String>,
}

/// Fetch active markets from Gamma API
async fn fetch_markets() -> Result<Vec<(String, String, String, String)>> {
    let client = reqwest::Client::new();

    // Get active events
    let url = format!("{}/events?active=true&closed=false&limit=20", GAMMA_API);
    let resp: Vec<GammaEvent> = client.get(&url).send().await?.json().await?;

    let mut markets = Vec::new();

    for event in resp {
        for market in event.markets {
            if let Some(token_ids) = market.clob_token_ids {
                // Parse "[\"token1\",\"token2\"]" format
                let tokens: Vec<String> = serde_json::from_str(&token_ids).unwrap_or_default();
                if tokens.len() == 2 {
                    markets.push((
                        tokens[0].clone(),
                        tokens[1].clone(),
                        market.question.clone(),
                        event.title.clone(),
                    ));
                }
            }
        }
    }

    Ok(markets)
}

fn print_market_line(
    label: &str,
    yes_price: f64,
    yes_size: f64,
    no_price: f64,
    no_size: f64,
) {
    let total = yes_price + no_price;
    let profit = 1.0 - total;
    let profit_pct = profit * 100.0;

    let profit_indicator = if profit > 0.005 {
        "🟢"
    } else if profit > 0.0 {
        "🟡"
    } else {
        "🔴"
    };

    println!(
        "{} {:<45} {:>6.1}¢ {:>8.1}$ {:>6.1}¢ {:>8.1}$ {:>6.1}¢ {:>+6.2}%",
        profit_indicator,
        &label[..label.len().min(45)],
        yes_price * 100.0,
        yes_size,
        no_price * 100.0,
        no_size,
        total * 100.0,
        profit_pct
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Polymarket Order Book Viewer ===\n");

    // Fetch markets
    println!("Fetching active markets from Gamma API...");
    let markets = fetch_markets().await?;
    println!("Found {} markets with token pairs\n", markets.len());

    if markets.is_empty() {
        println!("No markets found!");
        return Ok(());
    }

    // Take first 10 markets for demo
    let markets: Vec<_> = markets.into_iter().take(10).collect();

    // Build token -> market info map
    let mut token_to_market: HashMap<String, (String, bool)> = HashMap::new(); // token -> (label, is_yes)
    let mut market_yes_token: HashMap<String, String> = HashMap::new(); // label -> yes_token
    let mut market_no_token: HashMap<String, String> = HashMap::new(); // label -> no_token
    let mut all_tokens = Vec::new();

    for (yes_token, no_token, question, _event) in &markets {
        let label = if question.len() > 45 {
            format!("{}...", &question[..45])
        } else {
            question.clone()
        };

        token_to_market.insert(yes_token.clone(), (label.clone(), true));
        token_to_market.insert(no_token.clone(), (label.clone(), false));
        market_yes_token.insert(label.clone(), yes_token.clone());
        market_no_token.insert(label, no_token.clone());
        all_tokens.push(yes_token.clone());
        all_tokens.push(no_token.clone());
    }

    // Connect to WebSocket
    println!("Connecting to Polymarket WebSocket...");
    let (ws_stream, _) = connect_async(POLYMARKET_WS_URL).await?;
    let (mut write, mut read) = ws_stream.split();
    println!("Connected!\n");

    // Subscribe to tokens
    let subscribe_msg = SubscribeCmd {
        assets_ids: all_tokens,
        sub_type: "market",
    };
    let subscribe_json = serde_json::to_string(&subscribe_msg)?;
    write.send(Message::Text(subscribe_json)).await?;
    println!("Subscribed to {} tokens. Waiting for order book data...\n", markets.len() * 2);
    println!("{:-<105}", "");
    println!(
        "   {:<45} {:>8} {:>10} {:>8} {:>10} {:>8} {:>7}",
        "MARKET", "YES_ASK", "YES_SIZE", "NO_ASK", "NO_SIZE", "TOTAL", "PROFIT"
    );
    println!("{:-<105}", "");

    // Track order books: token -> (best_ask_price, best_ask_size)
    let mut order_books: HashMap<String, (f64, f64)> = HashMap::new();

    // Set timeout for demo
    let timeout = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(timeout);

    loop {
        tokio::select! {
            _ = &mut timeout => {
                println!("\n{:-<105}", "");
                println!("Timeout (60s). Exiting.");
                break;
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Try to parse as array of book snapshots (initial message format)
                        if let Ok(books) = serde_json::from_str::<Vec<BookSnapshot>>(&text) {
                            for book in books {
                                if let Some(best) = get_best_ask(&book) {
                                    order_books.insert(book.asset_id.clone(), best);
                                    try_print_market(&book.asset_id, &order_books, &token_to_market, &market_yes_token, &market_no_token);
                                }
                            }
                            continue;
                        }

                        // Try to parse as single book snapshot
                        if let Ok(book) = serde_json::from_str::<BookSnapshot>(&text) {
                            if let Some(best) = get_best_ask(&book) {
                                order_books.insert(book.asset_id.clone(), best);
                                try_print_market(&book.asset_id, &order_books, &token_to_market, &market_yes_token, &market_no_token);
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        println!("WebSocket closed");
                        break;
                    }
                    Some(Err(e)) => {
                        println!("Error: {}", e);
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

fn get_best_ask(book: &BookSnapshot) -> Option<(f64, f64)> {
    book.asks
        .iter()
        .filter_map(|l| {
            let price: f64 = l.price.parse().ok()?;
            let size: f64 = l.size.parse().ok()?;
            if price > 0.0 {
                Some((price, size))
            } else {
                None
            }
        })
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
}

fn try_print_market(
    asset_id: &str,
    order_books: &HashMap<String, (f64, f64)>,
    token_to_market: &HashMap<String, (String, bool)>,
    market_yes_token: &HashMap<String, String>,
    market_no_token: &HashMap<String, String>,
) {
    // Get market label for this token
    let Some((label, _is_yes)) = token_to_market.get(asset_id) else {
        return;
    };

    // Get both tokens for this market
    let Some(yes_token) = market_yes_token.get(label) else {
        return;
    };
    let Some(no_token) = market_no_token.get(label) else {
        return;
    };

    // Check if we have data for both sides
    let Some(&(yes_price, yes_size)) = order_books.get(yes_token) else {
        return;
    };
    let Some(&(no_price, no_size)) = order_books.get(no_token) else {
        return;
    };

    print_market_line(label, yes_price, yes_size, no_price, no_size);
}
