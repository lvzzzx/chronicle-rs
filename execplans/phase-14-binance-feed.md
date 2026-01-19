# Phase 14: Binance Market Feed Adapter

## Goal
Implement a high-performance, standalone market data feed adapter for Binance Spot markets, capable of writing normalized market data to a Chronicle Queue.

## Architecture

We will introduce a new crate `crates/chronicle-feed-binance` to keep the ecosystem modular.

### components
1.  **Transport**: `tokio` + `tokio-tungstenite` for async WebSocket management.
2.  **Protocol**: Binance WebSocket API (v3).
3.  **Parsing**: `serde_json` for initial parsing (can be optimized to `simd-json` later).
4.  **Storage**: `chronicle-core` QueueWriter for persistence and IPC.

### Data Flow
`Binance WS` -> `WebSocket Stream` -> `Parser` -> `Normalizer` -> `Chronicle Queue (Mmap)`

## Implementation Details

### 1. New Crate
`crates/chronicle-feed-binance`

### 2. Configuration
-   **Exchange**: Binance (URL hardcoded or config).
-   **Symbols**: List of symbols to subscribe (e.g., `btcusdt`, `ethusdt`).
-   **Streams**: `bookTicker` (best bid/ask) or `trade` (ticks). Default to `bookTicker` for latency sensitivity.
-   **Output**: Bus root path or specific queue path.

### 3. Data Model (Wire Format)
To ensure low latency and zero-copy reading for consumers, we should write fixed-size binary structs (SBE-style) or a compact binary format, rather than raw JSON.
However, for Phase 1 MVP, we can write a simple `#[repr(C)]` struct:

```rust
#[repr(C)]
struct BookTicker {
    timestamp: u64, // Exchange timestamp (ms)
    bid_price: f64,
    bid_qty: f64,
    ask_price: f64,
    ask_qty: f64,
    symbol_id: u64, // Hash or ID of the symbol
}
```

*Note: For the MVP, we might stick to a slightly more flexible format or just raw JSON bytes if schema evolution is a concern, but binary is preferred for "Ultra Low Latency".*

### 4. Resilience
-   Automatic reconnection on disconnect.
-   Subscription management (re-subscribing after reconnect).
-   Backpressure handling (though Chronicle handles this naturally via ring buffer).

## Steps

1.  [ ] Create `crates/chronicle-feed-binance` skeleton.
2.  [ ] Define `MarketEvent` binary format.
3.  [ ] Implement `BinanceClient` with `tokio-tungstenite`.
4.  [ ] Implement `ChroniclePublisher` adapter.
5.  [ ] Wire up `main.rs` with CLI args (clap).
6.  [ ] Test with `chronicle-cli monitor` or a simple reader.

## Dependencies
-   `tokio` (full)
-   `tokio-tungstenite` (native-tls or rustls)
-   `serde`, `serde_json`
-   `url`
-   `clap`
-   `chronicle-core`
