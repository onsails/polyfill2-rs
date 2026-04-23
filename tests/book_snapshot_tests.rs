//! Regression tests for issue #6 — WS `book` snapshot semantics + millis timestamp.
//!
//! See `docs/superpowers/specs/2026-04-23-ws-book-snapshot-fix-design.md`.

use std::str::FromStr;

use chrono::Datelike;
use polyfill2::types::{BookUpdate, OrderSummary};
use polyfill2::OrderBookImpl;
use rust_decimal::Decimal;

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn level(price: &str, size: &str) -> OrderSummary {
    OrderSummary {
        price: dec(price),
        size: dec(size),
    }
}

fn book_update(
    asset_id: &str,
    timestamp: u64,
    bids: Vec<OrderSummary>,
    asks: Vec<OrderSummary>,
) -> BookUpdate {
    BookUpdate {
        asset_id: asset_id.to_string(),
        market: "0xabc".to_string(),
        timestamp,
        bids,
        asks,
        hash: None,
    }
}

const ASSET: &str = "test_asset_id";

/// Bug B regression: a new `book` snapshot must remove levels from prior snapshots
/// that are not present in the new message.
#[test]
#[ignore = "unignored in Task 4 (bug B fix)"]
fn snapshot_clears_stale_levels() {
    let mut book = OrderBookImpl::new(ASSET.to_string(), 100);

    // S1: bids at 0.74 and 0.75; asks at 0.76 and 0.77.
    book.apply_book_update(&book_update(
        ASSET,
        1_000_000_000_001,
        vec![level("0.74", "100"), level("0.75", "200")],
        vec![level("0.76", "50"), level("0.77", "30")],
    ))
    .unwrap();

    // S2: only 0.80 bid and 0.81 ask — S1's levels must disappear.
    book.apply_book_update(&book_update(
        ASSET,
        1_000_000_000_002,
        vec![level("0.80", "150")],
        vec![level("0.81", "25")],
    ))
    .unwrap();

    let snap = book.snapshot();
    assert_eq!(snap.bids.len(), 1, "stale bids leaked: {:?}", snap.bids);
    assert_eq!(snap.asks.len(), 1, "stale asks leaked: {:?}", snap.asks);
    assert_eq!(snap.bids[0].price, dec("0.80"));
    assert_eq!(snap.asks[0].price, dec("0.81"));
}

/// Bug A regression: a 13-digit millisecond timestamp must parse to the current century,
/// not to year ~57,716 (which is what happens when millis are interpreted as seconds).
#[test]
fn snapshot_timestamp_parses_as_millis() {
    let mut book = OrderBookImpl::new(ASSET.to_string(), 100);

    // 2025-09-15T03:21:32.351Z in milliseconds.
    let ts_millis: u64 = 1_757_908_892_351;

    book.apply_book_update(&book_update(
        ASSET,
        ts_millis,
        vec![level("0.75", "100")],
        vec![level("0.76", "100")],
    ))
    .unwrap();

    let snap = book.snapshot();
    let year = snap.timestamp.year();
    assert!(
        (2020..2100).contains(&year),
        "timestamp parsed as seconds instead of millis: got year {year}",
    );
}

/// A snapshot carrying zero-sized wire levels must not place them in the book.
#[test]
fn snapshot_drops_zero_sized_levels() {
    let mut book = OrderBookImpl::new(ASSET.to_string(), 100);

    book.apply_book_update(&book_update(
        ASSET,
        1_000_000_000_001,
        vec![level("0.74", "0"), level("0.75", "100")],
        vec![level("0.76", "50"), level("0.77", "0")],
    ))
    .unwrap();

    let snap = book.snapshot();
    assert!(
        snap.bids.iter().all(|l| l.price != dec("0.74")),
        "zero-sized bid survived: {:?}",
        snap.bids,
    );
    assert!(
        snap.asks.iter().all(|l| l.price != dec("0.77")),
        "zero-sized ask survived: {:?}",
        snap.asks,
    );
    assert_eq!(snap.bids.len(), 1);
    assert_eq!(snap.asks.len(), 1);
}

/// max_depth is enforced: if the snapshot contains more levels than max_depth,
/// only the best levels are retained (highest bids, lowest asks).
#[test]
fn snapshot_enforces_max_depth_keeping_best() {
    let mut book = OrderBookImpl::new(ASSET.to_string(), 3);

    // 5 bids ascending (0.70..0.74); 5 asks ascending (0.80..0.84).
    // Best bid = 0.74 (highest). Best ask = 0.80 (lowest).
    let bids: Vec<_> = (0..5).map(|i| level(&format!("0.7{i}"), "100")).collect();
    let asks: Vec<_> = (0..5).map(|i| level(&format!("0.8{i}"), "100")).collect();

    book.apply_book_update(&book_update(ASSET, 1_000_000_000_001, bids, asks))
        .unwrap();

    let snap = book.snapshot();
    assert_eq!(snap.bids.len(), 3, "bids exceed max_depth");
    assert_eq!(snap.asks.len(), 3, "asks exceed max_depth");

    let bid_prices: Vec<_> = snap.bids.iter().map(|l| l.price).collect();
    let ask_prices: Vec<_> = snap.asks.iter().map(|l| l.price).collect();
    assert!(
        bid_prices.contains(&dec("0.74")),
        "best bid dropped: {bid_prices:?}",
    );
    assert!(
        ask_prices.contains(&dec("0.80")),
        "best ask dropped: {ask_prices:?}",
    );
}

/// A snapshot whose timestamp is <= the book's current sequence is discarded
/// without mutating the book.
#[test]
fn snapshot_ignored_when_timestamp_le_sequence() {
    let mut book = OrderBookImpl::new(ASSET.to_string(), 100);

    book.apply_book_update(&book_update(
        ASSET,
        10,
        vec![level("0.75", "100")],
        vec![level("0.76", "100")],
    ))
    .unwrap();

    // Stale snapshot: timestamp (5) < current sequence (10).
    book.apply_book_update(&book_update(
        ASSET,
        5,
        vec![level("0.99", "999")],
        vec![level("0.01", "999")],
    ))
    .unwrap();

    let snap = book.snapshot();
    assert_eq!(snap.bids.len(), 1);
    assert_eq!(snap.bids[0].price, dec("0.75"));
    assert_eq!(snap.asks.len(), 1);
    assert_eq!(snap.asks[0].price, dec("0.76"));
}

/// Docs-example input shows ascending-price on both sides. In debug builds we
/// assert this and panic on violation to catch a server-side contract change early.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "ascending")]
#[ignore = "unignored in Task 4 (debug_assert added)"]
fn snapshot_panics_on_descending_bids_in_debug() {
    let mut book = OrderBookImpl::new(ASSET.to_string(), 100);
    book.apply_book_update(&book_update(
        ASSET,
        1,
        vec![level("0.75", "100"), level("0.74", "100")], // DESCENDING — violation
        vec![level("0.76", "100")],
    ))
    .unwrap();
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "ascending")]
#[ignore = "unignored in Task 4 (debug_assert added)"]
fn snapshot_panics_on_descending_asks_in_debug() {
    let mut book = OrderBookImpl::new(ASSET.to_string(), 100);
    book.apply_book_update(&book_update(
        ASSET,
        1,
        vec![level("0.75", "100")],
        vec![level("0.77", "100"), level("0.76", "100")], // DESCENDING — violation
    ))
    .unwrap();
}

/// Alternating snapshots S1 -> S2 -> S1 must produce the expected book each time,
/// with no state leakage between snapshots.
#[test]
#[ignore = "unignored in Task 4 (bug B fix)"]
fn snapshot_alternating_s1_s2_s1_has_no_leakage() {
    let mut book = OrderBookImpl::new(ASSET.to_string(), 100);

    let s1_bids = vec![level("0.74", "100"), level("0.75", "200")];
    let s1_asks = vec![level("0.76", "50"), level("0.77", "30")];
    let s2_bids = vec![level("0.60", "500")];
    let s2_asks = vec![level("0.90", "10")];

    book.apply_book_update(&book_update(ASSET, 1, s1_bids.clone(), s1_asks.clone()))
        .unwrap();
    let snap1 = book.snapshot();
    assert_eq!(snap1.bids.len(), 2);
    assert_eq!(snap1.asks.len(), 2);

    book.apply_book_update(&book_update(ASSET, 2, s2_bids.clone(), s2_asks.clone()))
        .unwrap();
    let snap2 = book.snapshot();
    assert_eq!(snap2.bids.len(), 1);
    assert_eq!(snap2.bids[0].price, dec("0.60"));
    assert_eq!(snap2.asks.len(), 1);
    assert_eq!(snap2.asks[0].price, dec("0.90"));

    book.apply_book_update(&book_update(ASSET, 3, s1_bids.clone(), s1_asks.clone()))
        .unwrap();
    let snap3 = book.snapshot();
    assert_eq!(snap3.bids.len(), 2);
    assert_eq!(snap3.asks.len(), 2);
    assert!(
        snap3.bids.iter().all(|l| l.price != dec("0.60")),
        "S2 bid leaked into S3: {:?}",
        snap3.bids,
    );
    assert!(
        snap3.asks.iter().all(|l| l.price != dec("0.90")),
        "S2 ask leaked into S3: {:?}",
        snap3.asks,
    );
}
