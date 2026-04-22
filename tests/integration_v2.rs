//! Integration smoke tests against Polymarket CLOB V2 staging.
//!
//! These tests hit the live `clob-v2.polymarket.com` endpoints (or a custom URL
//! via `CLOB_V2_URL`) and are `#[ignore]`d so they never run under a plain
//! `cargo test`.
//!
//! Run with:
//!     cargo test --test integration_v2 -- --ignored --test-threads=1
//!
//! Required env:
//!     POLYMARKET_PRIVATE_KEY — EOA private key hex (0x...)
//! Optional env:
//!     CLOB_V2_URL     — base URL (default: https://clob-v2.polymarket.com)
//!     TEST_TOKEN_ID   — token id used by the endpoints that need one
//!                        (v2_unauth_fee_rate, v2_unauth_order_book).
//!                        If unset, those tests log a SKIP line and return.

use polyfill_rs::ClobClient;
use std::env;
use std::time::Duration;

const DEFAULT_V2_URL: &str = "https://clob-v2.polymarket.com";
const POLYGON_CHAIN_ID: u64 = 137;
const WS_USER_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/user";

fn base_url() -> String {
    env::var("CLOB_V2_URL").unwrap_or_else(|_| DEFAULT_V2_URL.to_string())
}

fn private_key() -> String {
    dotenvy::dotenv().ok();
    env::var("POLYMARKET_PRIVATE_KEY")
        .expect("POLYMARKET_PRIVATE_KEY must be set in .env or environment")
}

fn unauth_client() -> ClobClient {
    ClobClient::new(&base_url())
}

async fn authed_client() -> ClobClient {
    let pk = private_key();
    let mut client = ClobClient::with_l1_headers(&base_url(), &pk, POLYGON_CHAIN_ID);
    let creds = client
        .create_or_derive_api_key(None)
        .await
        .expect("create_or_derive_api_key failed — invalid PK or server rejected");
    client.set_api_creds(creds);
    client
}

// ---------------------------------------------------------------------------
// Unauthenticated endpoints
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn v2_unauth_ok() {
    let client = unauth_client();
    let ok = client.get_ok().await;
    assert!(ok, "V2 /ok returned false — server unhealthy or unreachable");
    println!("V2 /ok OK: {}", ok);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn v2_unauth_server_time() {
    let client = unauth_client();
    let time = client.get_server_time().await.expect("GET /time failed");
    // Sanity: the server clock should be at least 2020-01-01 and before year 2100.
    assert!(
        time > 1_577_836_800 && time < 4_102_444_800,
        "V2 server time {time} is not a sensible Unix timestamp",
    );
    println!("V2 server time: {}", time);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn v2_unauth_sampling_markets() {
    let client = unauth_client();
    let markets = client
        .get_sampling_markets(None)
        .await
        .expect("get_sampling_markets failed");
    println!("V2 sampling_markets returned {} markets", markets.data.len());
    if let Some(m) = markets.data.first() {
        if let Some(token) = m.tokens.first() {
            println!(
                "V2 sample: condition_id={}, token_id={}",
                m.condition_id, token.token_id
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn v2_unauth_fee_rate() {
    let Some(token_id) = env::var("TEST_TOKEN_ID").ok() else {
        eprintln!("SKIP v2_unauth_fee_rate: set TEST_TOKEN_ID env to run");
        return;
    };
    let client = unauth_client();
    let fee = client
        .get_fee_rate_bps(&token_id)
        .await
        .expect("get_fee_rate_bps failed");
    println!("V2 fee rate bps for {}: {}", token_id, fee);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn v2_unauth_order_book() {
    let Some(token_id) = env::var("TEST_TOKEN_ID").ok() else {
        eprintln!("SKIP v2_unauth_order_book: set TEST_TOKEN_ID env to run");
        return;
    };
    let client = unauth_client();
    let book = client
        .get_order_book(&token_id)
        .await
        .expect("get_order_book failed");
    println!(
        "V2 /book for {}: {} bids, {} asks",
        token_id,
        book.bids.len(),
        book.asks.len()
    );
}

// ---------------------------------------------------------------------------
// Authenticated endpoints (require POLYMARKET_PRIVATE_KEY)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn v2_auth_create_or_derive_api_key() {
    let pk = private_key();
    let client = ClobClient::with_l1_headers(&base_url(), &pk, POLYGON_CHAIN_ID);
    let creds = client
        .create_or_derive_api_key(None)
        .await
        .expect("create_or_derive_api_key failed");
    assert!(!creds.api_key.is_empty(), "empty api_key in creds");
    assert!(!creds.secret.is_empty(), "empty secret in creds");
    assert!(!creds.passphrase.is_empty(), "empty passphrase in creds");
    println!(
        "V2 got api key (len={}, secret_len={}, pass_len={})",
        creds.api_key.len(),
        creds.secret.len(),
        creds.passphrase.len(),
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn v2_auth_get_rfq_config() {
    let client = authed_client().await;
    let cfg = client
        .get_rfq_config()
        .await
        .expect("get_rfq_config failed");
    println!(
        "V2 /rfq/config response: {}",
        serde_json::to_string_pretty(&cfg).unwrap_or_default()
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn v2_auth_get_orders() {
    let client = authed_client().await;
    let orders = client
        .get_orders(None, None)
        .await
        .expect("get_orders failed");
    println!("V2 got {} open orders", orders.len());
}

// ---------------------------------------------------------------------------
// WebSocket smoke (user channel, authenticated)
// ---------------------------------------------------------------------------

/// Open a WS user-channel subscription using V2-derived L2 creds and verify
/// the connection does not immediately drop. We may or may not receive a
/// message depending on account activity; the primary assertion is that
/// auth + subscribe succeeds and the stream stays open for a short window.
#[cfg(feature = "stream")]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn v2_ws_user_channel_subscribe() {
    use futures::StreamExt;
    use polyfill_rs::WebSocketStream;

    let pk = private_key();
    let auth_client = ClobClient::with_l1_headers(&base_url(), &pk, POLYGON_CHAIN_ID);
    let api_creds = auth_client
        .create_or_derive_api_key(None)
        .await
        .expect("create_or_derive_api_key failed");

    // Pick an active market (condition_id) to scope the subscription.
    let markets = auth_client
        .get_sampling_markets(None)
        .await
        .expect("get_sampling_markets failed");
    let market_id = markets
        .data
        .iter()
        .find(|m| m.active && !m.closed)
        .map(|m| m.condition_id.clone())
        .expect("no active markets found for WS user-channel subscription");

    let mut ws = WebSocketStream::new(WS_USER_URL).with_auth(api_creds);
    ws.subscribe_user_channel(vec![market_id])
        .await
        .expect("failed to subscribe user channel");

    // Keep the connection open briefly. We tolerate no messages (idle account)
    // but fail on explicit errors / premature close.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), ws.next()).await {
            Ok(Some(Ok(_msg))) => {},
            Ok(Some(Err(e))) => panic!("V2 WS user-channel error: {:?}", e),
            Ok(None) => panic!("V2 WS user-channel stream ended unexpectedly"),
            Err(_) => {
                // Idle tick; keep waiting.
            },
        }
    }
    println!("V2 WS user-channel: stayed open through smoke window");
}
