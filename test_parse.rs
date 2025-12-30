use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct BookSnapshot {
    pub asset_id: String,
    #[serde(default)]
    pub market: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub hash: Option<String>,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
}

#[derive(Deserialize, Debug)]
pub struct PriceLevel {
    pub price: String,
    pub size: String,
}

fn main() {
    let test_json = r#"[{
        "market": "0xbb3c778a1d6bab110201be110a4ae24c39e0c65f0db19f31f38e394a835a1c7e",
        "asset_id": "37870207272605201136057971663811102809837735102858689692520482020176213703809",
        "timestamp": "1767079588933",
        "hash": "8206a89516e5099f208ff833a258bf5954a14215",
        "bids": [{"price": "0.01", "size": "2608.88"}],
        "asks": [{"price": "0.99", "size": "100"}]
    }]"#;

    match serde_json::from_str::<Vec<BookSnapshot>>(test_json) {
        Ok(books) => {
            println!("✅ SUCCESS! Parsed {} book(s)", books.len());
            for book in &books {
                println!("   Asset: {}", &book.asset_id[..20]);
                println!("   Asks: {} levels", book.asks.len());
                println!("   Best ask: {} @ {}", book.asks[0].price, book.asks[0].size);
            }
        }
        Err(e) => {
            println!("❌ FAILED to parse: {}", e);
        }
    }
}
