[![Crates.io](https://img.shields.io/crates/v/polyfill2.svg)](https://crates.io/crates/polyfill2)
[![Documentation](https://docs.rs/polyfill2/badge.svg)](https://docs.rs/polyfill2)
[![CI](https://github.com/onsails/polyfill2-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/onsails/polyfill2-rs/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

A high-performance Rust client for Polymarket's **CLOB V2** API, with latency-optimized data structures and zero-allocation hot paths. This is a V2 migration fork of [`polyfill-rs`](https://github.com/floor-licker/polyfill-rs) (Julius Tranquilli), updated in April 2026 for the CLOB V2 cutover (new EIP-712 Order schema, new contract addresses, new endpoint URLs, new WebSocket message shapes).

`polyfill-rs` was originally created as a performance-focused Rust alternative to `polymarket-rs-client`, aiming to beat the benchmarks quoted in that project's README while maintaining zero-allocation hot paths. This fork carries those properties forward onto V2.

**On zero-alloc**: in this project, "zero-alloc" means zero allocations in the per-message handling loop after init/warm-up — i.e. **the hot path never touches the heap**, even though cold paths do. Order book paths that introduce new allocations by design:
- First time seeing a token/book (HashMap insert + key clone): `src/book.rs:~788`
- New price levels (BTreeMap node growth): `src/book.rs:~409`


## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
polyfill2 = "0.1"
```

```rust
use polyfill2::{ClobClient, Side, OrderType};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClobClient::new("https://clob-v2.polymarket.com");
    let markets = client.get_sampling_markets(None).await?;
    println!("Found {} markets", markets.data.len());
    Ok(())
}
```

## Performance Comparison

> **Note:** the numbers below were measured on upstream `polyfill-rs` against CLOB V1 (`clob.polymarket.com`) prior to the V2 migration. Computational paths (book updates, spread/mid, WS decode) are unchanged and should still hold; the networked end-to-end number has not been re-measured against V2 — [#1](https://github.com/onsails/polyfill2-rs/issues/1) tracks re-measurement.

**Real-World API Performance (with network I/O)**

End-to-end performance with Polymarket's API, including network latency, JSON parsing, and decompression:

| Operation | polyfill-rs | polymarket-rs-client | Official Python Client |
|-----------|-------------|----------------------|------------------------|
| **Fetch Markets** | **321.6 ms ± 92.9 ms** | 409.3 ms ± 137.6 ms | 1.366 s ± 0.048 s |


**Performance vs polymarket-rs-client:**
- **21.4% faster** 
- **32.5% more consistent** 
- **4.2x faster** than Official Python Client

**Benchmark Methodology:** All benchmarks run side-by-side on the same machine, same network, same time using 20 iterations, 100ms delay between requests, /simplified-markets endpoint. Best performance achieved with connection keep-alive enabled. See `examples/side_by_side_benchmark.rs` in commit `a63a170`: https://github.com/floor-licker/polyfill-rs/blob/a63a170/examples/side_by_side_benchmark.rs for the complete benchmark implementation.

**Computational Performance (pure CPU, no I/O)**

| Operation | Performance | Notes |
|-----------|-------------|-------|
| **Order Book Updates (1000 ops)** | 159.6 µs ± 32 µs | 6,260 updates/sec, zero-allocation |
| **Spread/Mid Calculations** | 70 ns ± 77 ns | 14.3M ops/sec, optimized BTreeMap |
| **JSON Parsing (480KB)** | ~2.3 ms | SIMD-accelerated parsing (1.77x faster than serde_json) |
| **WS `book` hot path (decode + apply)** | ~0.28 µs / 2.01 µs / 7.70 µs | 1 / 16 / 64 levels-per-side, ~3.7–4.0x faster vs serde decode+apply (see `benches/ws_hot_path.rs`) |

Run the WS hot-path benchmark locally with `cargo bench --bench ws_hot_path`.

**Key Performance Optimizations:**

The 21.4% performance improvement comes from SIMD-accelerated JSON parsing (1.77x faster than serde_json), HTTP/2 tuning with 512KB stream windows optimized for 469KB payloads, integrated DNS caching, connection keep-alive, and buffer pooling to reduce allocation overhead.

### Memory Architecture

Pre-allocated pools eliminate allocation latency spikes. Configurable book depth limiting prevents memory bloat. Hot data structures group frequently-accessed fields for cache line efficiency.

### Architectural Principles

Price data converts to fixed-point at ingress boundaries while maintaining tick-aligned precision. The critical path uses integer arithmetic with branchless operations. Data converts back to IEEE 754 at egress for API compatibility. This enables deterministic execution with predictable instruction counts.

### Measured Network Improvements

| Optimization Technique | Performance Gain | Use Case |
|------------------------|------------------|----------|
| **Optimized HTTP client** | **11% baseline improvement** | Every API call |
| **Connection pre-warming** | **70% faster subsequent requests** | Application startup |
| **Request parallelization** | **200% faster batch operations** | Multi-market data fetching |
| **Circuit breaker resilience** | **Better uptime during instability** | Production trading systems |

## Credits

`polyfill-rs` was originally authored by Julius Tranquilli ([floor-licker/polyfill-rs](https://github.com/floor-licker/polyfill-rs)). This V2 fork (`polyfill2`) is maintained by [onsails](https://github.com/onsails).
