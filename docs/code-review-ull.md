# Code Review: Research Architecture & Feed Path

**Date:** 2026-01-21
**Reviewer:** Silas "Wire-Speed" Vance
**Subject:** Latency Analysis of `chronicle-core` Indexing and `chronicle-feed-binance`

## 1. Executive Summary

The implementation of the Research Tier (replay, snapshots, indexing) is structurally sound and adheres to the "Log is Database" philosophy. The `chronicle-protocol` definitions provide the necessary ABI stability.

However, the current implementation introduces **unbounded latencies** in the critical write path due to heap allocations. This violates the `ultra_low_latency` mandate for the core write path.

## 2. Findings

### 2.1 Critical: Dynamic Allocation in `SeekIndexBuilder` (Hot Path)

**Location:** `crates/chronicle/src/core/seek_index.rs`
**Severity:** High (Latency Jitter)

The `SeekIndexBuilder` maintains an in-memory `Vec<SeekIndexEntry>` that grows as records are appended.

```rust
pub(crate) struct SeekIndexBuilder {
    // ...
    entries: Vec<SeekIndexEntry>,
}

impl SeekIndexBuilder {
    pub(crate) fn observe(...) {
        // ...
        if self.record_index == self.next_entry_at {
             // This push() can trigger a reallocation/memcpy of the entire index
             // during a "write" call, causing a latency spike > 100us.
            self.entries.push(...);
        }
    }
}
```

**Impact:**
When the vector capacity is exceeded, the OS allocator must find a new contiguous block and copy the old data. If this happens during a burst of market data, the write latency for that specific tick will spike, potentially causing backpressure to the feed handler.

**Recommendation:**
Since `segment_size` and `entry_stride` are known at creation time, we can calculate a safe upper bound for the number of entries.
-   **Estimate:** `Max Entries = (Segment Size / Min Record Size) / Stride`.
-   **Action:** Pre-allocate the vector with `Vec::with_capacity(max_entries)` in `SeekIndexBuilder::new`.

### 2.2 Major: Intermediate Allocations in Feed Handler

**Location:** `crates/4-app/chronicle-feed-binance/src/binance.rs`
**Severity:** Medium (Throughput/GC pressure)

The Binance feed handler allocates intermediate vectors for Bids and Asks before writing them to the queue.

```rust
struct L2DiffEvent {
    // ...
    bids: Vec<PriceLevelUpdate>, // <--- Heap Allocation
    asks: Vec<PriceLevelUpdate>, // <--- Heap Allocation
}

fn convert_depth_update(...) -> Result<L2DiffEvent> {
    // ...
    let bids = parse_levels(...); // Allocates new Vec
    let asks = parse_levels(...); // Allocates new Vec
    Ok(L2DiffEvent { bids, asks, ... })
}
```

**Impact:**
For a high-throughput feed (e.g., Binance Futures during volatility), this creates massive allocator churn. While Rust does not have a GC, the allocator overhead (`malloc`/`free`) and cache thrashing reduce the maximum sustainable throughput.

**Recommendation:**
1.  **Short Term:** Use a thread-local "scratchpad" struct with reusable `Vec::clear()` + `push()` to avoid allocation per message.
2.  **Long Term:** Implement a "Direct-to-Mmap" parser where we reserve space in the queue first, then parse the JSON directly into the reserved memory.

### 2.3 Minor: Timestamp Precision in Replay

**Location:** `crates/3-engine/chronicle-replay/src/lib.rs`

The replay engine uses `SystemTime` for wall-clock benchmarks. Ensure that in production replay, we strictly use `ingest_ts_ns` from the record header to simulate "time passing," rather than checking the CPU clock.

## 3. Action Items

1.  **[Core]** Refactor `SeekIndexBuilder` to pre-calculate and `reserve()` capacity.
2.  **[Feed]** Refactor `BinanceFeed` to use a reused `L2DiffEvent` buffer instead of allocating new ones per message.
