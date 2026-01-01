# Trade API

## Polymarket CLOB API

### Overview

The bot interacts with Polymarket's Central Limit Order Book (CLOB) for order submission and execution.

**API Host**: `https://clob.polymarket.com`
**Chain**: Polygon (Chain ID: 137)
**File**: `src/polymarket_clob.rs`

## Authentication

### EIP-712 Message Signing

Polymarket uses Ethereum-based authentication with EIP-712 typed data signing:

```rust
pub async fn derive_api_key(&self) -> Result<ApiCreds> {
    // 1. Create CLOB auth message (nonce from server)
    let nonce = self.get_nonce().await?;

    // 2. Sign EIP-712 typed data with private key
    let signature = self.wallet.sign_typed_data(&auth_message).await?;

    // 3. Exchange signature for API credentials
    let creds = self.exchange_for_creds(signature).await?;

    // Returns: ApiCreds { api_key, api_secret, api_passphrase }
    Ok(creds)
}
```

### Request Signing

All authenticated requests use HMAC-SHA256:

```rust
fn sign_request(&self, method: &str, path: &str, body: &str, timestamp: i64) -> String {
    let message = format!("{}{}{}{}", timestamp, method, path, body);
    hmac_sha256(api_secret, message).to_base64()
}
```

### Headers

```
POLY-ADDRESS: 0x...           // Wallet address
POLY-SIGNATURE: <signature>   // HMAC signature
POLY-TIMESTAMP: <unix_ms>     // Request timestamp
POLY-API-KEY: <api_key>       // API key
POLY-PASSPHRASE: <passphrase> // API passphrase
```

## Order Types

### Fill-or-Kill (FAK) Orders

The bot exclusively uses FAK orders for guaranteed execution:

```rust
// FAK order: executes immediately at specified price or cancels entirely
pub async fn buy_fak(&self, token_id: &str, price: f64, size: f64) -> Result<OrderResult>
pub async fn sell_fak(&self, token_id: &str, price: f64, size: f64) -> Result<OrderResult>
```

**Benefits**:
- No partial fills (all-or-nothing)
- No slippage risk
- Immediate execution feedback

## Price & Size Conversion

### Price to Basis Points

Prices are converted from decimal (0.0-1.0) to basis points (0-10000):

```rust
pub fn price_to_bps(price: f64) -> u64 {
    // price: 0.48 → 4800 bps
    (price * 10000.0).round() as u64
}
```

### Size to Micro Units

Contract sizes use 6 decimal precision:

```rust
pub fn size_to_micro(size: f64) -> u64 {
    // size: 15.0 → 15_000_000 micro
    (size * 1_000_000.0).round() as u64
}
```

### Order Amount Calculations

**BUY Order** (side=0):

```rust
pub fn get_order_amounts_buy(size_micro: u64, price_bps: u64) -> (u64, u64) {
    // maker_amount = cost in USDC (what you pay)
    // taker_amount = contracts (what you receive)
    let maker = (size_micro * price_bps) / 10000;
    let taker = size_micro;
    (maker, taker)
}

// Example: Buy 15 contracts at $0.48
// size_micro = 15_000_000
// price_bps = 4800
// maker_amount = (15_000_000 * 4800) / 10000 = 7_200_000 (=$7.20)
// taker_amount = 15_000_000 (=15 contracts)
```

**SELL Order** (side=1):

```rust
pub fn get_order_amounts_sell(size_micro: u64, price_bps: u64) -> (u64, u64) {
    // maker_amount = contracts (what you give)
    // taker_amount = USDC proceeds (what you receive)
    let maker = size_micro;
    let taker = (size_micro * price_bps) / 10000;
    (maker, taker)
}
```

## Order Submission

### Create Order Request

```rust
#[derive(Serialize)]
pub struct CreateOrderRequest {
    pub order: SignedOrder,
    pub owner: String,           // Wallet address
    pub order_type: String,      // "FAK"
}

#[derive(Serialize)]
pub struct SignedOrder {
    pub salt: String,
    pub maker: String,           // Wallet address
    pub signer: String,          // Signing address
    pub taker: String,           // 0x000...000 (any taker)
    pub token_id: String,        // Polymarket token address
    pub maker_amount: String,    // Cost/contracts (depends on side)
    pub taker_amount: String,    // Contracts/proceeds (depends on side)
    pub expiration: String,      // Unix timestamp
    pub nonce: String,           // Unique per order
    pub fee_rate_bps: String,    // 0 (no fees on Polymarket)
    pub side: String,            // "0" (buy) or "1" (sell)
    pub signature_type: String,  // "2" (EOA signature)
    pub signature: String,       // EIP-712 signature
}
```

### Submit Order

```rust
pub async fn submit_order(&self, order: CreateOrderRequest) -> Result<OrderResult> {
    let response = self.client
        .post(&format!("{}/order", self.host))
        .headers(self.auth_headers()?)
        .json(&order)
        .send()
        .await?;

    // Returns order ID and fill details
    Ok(response.json::<OrderResult>().await?)
}
```

### Order Result

```rust
pub struct OrderResult {
    pub order_id: String,
    pub status: String,        // "FILLED", "CANCELLED", etc.
    pub filled_size: f64,      // Actual contracts filled
    pub avg_fill_price: f64,   // Execution price
    pub fees: f64,             // Always 0 on Polymarket
}
```

## WebSocket Price Feed

### Connection

**URL**: `wss://ws-subscriptions-clob.polymarket.com/ws/market`

```rust
pub async fn connect_websocket(&self, token_ids: &[String]) -> Result<WebSocket> {
    let ws = connect_async(WS_URL).await?;

    // Subscribe to markets
    let subscribe_msg = json!({
        "type": "subscribe",
        "channel": "book",
        "markets": token_ids
    });

    ws.send(subscribe_msg).await?;
    Ok(ws)
}
```

### Message Types

**Book Snapshot** (full orderbook):

```json
{
  "asset_id": "0x1234...abcd",
  "market": "market-slug",
  "asks": [
    {"price": "0.48", "size": "1000"},
    {"price": "0.49", "size": "500"}
  ],
  "bids": [
    {"price": "0.47", "size": "800"},
    {"price": "0.46", "size": "1200"}
  ],
  "timestamp": 1704067200000
}
```

**Price Change Event**:

```json
{
  "event_type": "price_change",
  "price_changes": [
    {
      "asset_id": "0x1234...abcd",
      "best_ask": "0.48",
      "best_bid": "0.47",
      "size": "100"
    }
  ]
}
```

**Last Trade Price** (ignored for arbitrage):

```json
{
  "event_type": "last_trade_price",
  "asset_id": "0x1234...abcd",
  "price": "0.475"
}
```

## API Endpoints Summary

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/auth/nonce` | GET | Get nonce for auth |
| `/auth/derive-api-key` | POST | Exchange signature for API keys |
| `/order` | POST | Submit new order |
| `/orders/{id}` | GET | Get order status |
| `/orders/{id}` | DELETE | Cancel order |
| `/positions` | GET | Get current positions |

## Fee Structure

**Polymarket Trading Fees**: **0%** (Zero fees)

This is a significant advantage for arbitrage strategies - the only cost is Polygon gas for on-chain settlement (typically < $0.01).

## Rate Limits

- **API Requests**: 10 requests/second per IP
- **WebSocket Messages**: No documented limit (handles 1000s/sec)
- **Order Submission**: 5 orders/second per account

The bot uses `governor` crate for rate limiting:

```rust
let limiter = RateLimiter::direct(Quota::per_second(10));
```

## Error Handling

### Common Error Codes

| Code | Meaning | Action |
|------|---------|--------|
| 400 | Bad request | Check order parameters |
| 401 | Unauthorized | Re-derive API credentials |
| 429 | Rate limited | Backoff and retry |
| 500 | Server error | Retry with exponential backoff |

### Retry Logic

```rust
const MAX_RETRIES: u32 = 3;
const RETRY_DELAY_MS: u64 = 1000;

async fn submit_with_retry(&self, order: CreateOrderRequest) -> Result<OrderResult> {
    for attempt in 0..MAX_RETRIES {
        match self.submit_order(order.clone()).await {
            Ok(result) => return Ok(result),
            Err(e) if is_retryable(&e) => {
                tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * 2_u64.pow(attempt))).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err("Max retries exceeded")
}
```
