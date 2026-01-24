# Phase 15: Binance Diff. Depth Stream Support

## Context
We need to support the Binance "Diff. Depth Stream" (`<symbol>@depth@100ms`). To ensure the persisted data is self-contained and replayable (for backtesting), the Feed Adapter must handle the "Buffer -> Snapshot -> Sync" protocol and write **both** the initial `Snapshot` and subsequent `Diffs` to the queue.

## Goals
1.  **Schema Definition:** Define binary formats for `DepthUpdate` and `OrderBookSnapshot`.
2.  **Snapshot Management:** Feed adapter must fetch the initial snapshot via REST and perform gap detection/synchronization.
3.  **Persist Self-Contained Stream:** Ensure the queue contains all necessary data to reconstruct the order book without external dependencies.
4.  **Zero-Copy Writes:** Efficiently write variable-length updates using `append_in_place`.

## Data Structures

### 1. `PriceLevel`
Standard representation of a single order book entry.
```rust
#[repr(C)]
pub struct PriceLevel {
    pub price: f64,
    pub qty: f64,
}
```

### 2. `DepthHeader`
Metadata for updates and snapshots.
```rust
#[repr(C)]
pub struct DepthHeader {
    pub timestamp_ms: u64,
    pub first_update_id: u64,
    pub final_update_id: u64,
    pub symbol_hash: u64,
    pub bid_count: u32,
    pub ask_count: u32,
}
```

### 3. Message Types
- `MarketMessageType::DepthUpdate`: Incremental changes.
- `MarketMessageType::OrderBookSnapshot`: Full state of the book (typically Top 1000).

### 4. Wire Format
Both message types use the same wire layout:
`[DepthHeader] [PriceLevel... (bids)] [PriceLevel... (asks)]`

## Implementation Steps

### Step 1: Dependencies
- Add `reqwest` (blocking or async) to `chronicle-feed-binance` for fetching the snapshot.

### Step 2: Market Module Updates (`crates/4-app/chronicle-feed-binance/src/market.rs`)
- Define `PriceLevel` and `DepthHeader`.
- Add `MarketMessageType::DepthUpdate` and `MarketMessageType::OrderBookSnapshot`.

### Step 3: Snapshot & Sync Logic (`crates/4-app/chronicle-feed-binance/src/binance.rs`)
- **State Machine:**
    - `Connecting`: Establish WS connection.
    - `Buffering`: Accumulate WS events while fetching REST snapshot.
    - `Synchronizing`: Filter buffered events against `snapshot.lastUpdateId`.
    - `Streaming`: Write Snapshot -> Write Buffered Diffs -> Write New Diffs.
- **REST Client:** specific method to `GET /api/v3/depth`.

### Step 4: Writing Logic
- Both Snapshots and Diffs are variable length.
- Calculate size: `sizeof(Header) + (n_bids + n_asks) * sizeof(PriceLevel)`.
- Use `append_in_place` to write directly to the queue.

## Technical Considerations
- **Gap Detection:** The feed must monitor `U` and `u`. If a gap occurs (`next.U != prev.u + 1`), it must trigger a re-sync (Fetch new Snapshot).
- **Blocking vs Async:** Since we are in an async `tokio` loop, we should use `reqwest` async client.
- **Buffering:** Use a `VecDeque` for buffering WS messages during the snapshot fetch phase.

## Verification
- Run feed, wait for sync.
- Inspect queue: Should see one massive `OrderBookSnapshot` followed by small `DepthUpdate` messages.
- Verify `first_update_id` of the first Diff > `final_update_id` of the Snapshot.
