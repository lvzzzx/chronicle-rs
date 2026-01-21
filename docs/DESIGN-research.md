# Research & Replay Architecture (The Silas Standard)

## 1. Philosophy: The Log IS the Database

In the `chronicle-rs` ecosystem, we reject the traditional separation between "Live Trading" and "Historical Research".
- **Live:** The Feed Handler writes binary events to memory-mapped segments.
- **Research:** The Replay Engine maps those *exact same segments* to reconstruct the past.

**The raw log is the source of truth.** Derived stores are optional accelerators for analytics.
Why?
1.  **Fidelity:** Databases destroy wire-level nuances (sequence gaps, packet fragmentation, exact hardware timestamps).
2.  **Latency:** ETL processes introduce delays and translation errors.
3.  **Redundancy:** Storing the data twice (Log + DB) is wasteful.

**Pragmatic note:** Some teams keep columnar/TSDB mirrors for fast slicing or training. That is acceptable as long as the raw log remains the canonical source and every derived dataset is reproducible from it.

## 2. Architecture: Deterministic Event Sourcing

The Replay Engine is a **state machine** where $State_{t+1} = f(State_t, Event_t)$.

### 2.1 Components

1.  **Source (Chronicle Reader):**
    -   Iterates over persisted `.q` files.
    -   Mode: `StartMode::Earliest` (for full rebuild) or `StartMode::ResumeSnapshot`.
    -   Access: Zero-Copy (casts raw bytes to Event structs).

2.  **State (The L3 Book):**
    -   An in-memory reconstruction of the Limit Order Book.
    -   Tracks every single order (L3), not just price levels (L2).

3.  **Clock (Time Hijacking):**
    -   The Replay Engine **ignores the system clock**.
    -   Time is purely a function of `message.timestamp_ns` (Ingest Time) and `event.exchange_ts` (Matching Engine Time).
    -   Strategies act on "Event Time", ensuring backtests are reproducible regardless of hardware speed.

### 2.2 The Replay Loop (Pseudocode)

```rust
// The loop must be tight and allocation-free.
while let Some(msg) = reader.next() {
    // 1. Integrity Check (The "Gap" Obsession)
    if msg.seq != last_seq + 1 {
        panic!("SEQUENCE GAP: {} -> {}. Market state invalid.", last_seq, msg.seq);
    }
    
    // 2. State Update
    let event = msg.as_event(); // Zero-copy cast
    book.apply(event);
    
    // 3. Strategy Hook
    strategy.on_event(&book, event);
    
    last_seq = msg.seq;
}
```

## 3. L3 Order Book Reconstruction

To match the matching engine, we must track individual orders.

### 3.1 Data Structures

```rust
struct L3Book {
    // O(1) access to modify/cancel specific orders
    orders: HashMap<u128, OrderNode>, // Key: OrderID

    // Sorted access for "Best Bid/Ask" and impact cost calculation
    // Using BTreeMap for simplicity, or a custom Red-Black tree for perf
    bids: BTreeMap<u64, Level>, // Key: Price (descending)
    asks: BTreeMap<u64, Level>, // Key: Price (ascending)
}

struct OrderNode {
    price: u64,
    size: u64,
    side: Side,
    // Pointers for intrusive linked list within the Level?
}
```

### 3.2 Event Logic

*   **ADD:**
    1.  Create `OrderNode`.
    2.  Insert into `orders` HashMap.
    3.  Find/Create `Level` in `bids`/`asks`.
    4.  Add size to `Level`.

*   **CANCEL (Partial or Full):**
    1.  Lookup `OrderNode` in `orders`.
    2.  Decrement size in `Level`.
    3.  If size == 0, remove `OrderNode` and cleanup `Level`.

*   **EXECUTE:**
    1.  Treat as Partial Cancel (reduce size).
    2.  Record "Trade" feature for the strategy (volume, aggressor side).

*   **REPLACE (Modify):**
    1.  Atomic `Cancel(Old)` + `Add(New)`.
    2.  **Crucial:** This resets queue priority. The simulation must reflect this loss of priority.

### 3.3 Venue Capability Matrix (L2 vs L3)

Not every venue can support true L3 reconstruction. This design assumes:
- **L3 mode:** order-level updates and stable OrderID semantics exist.
- **L2 mode:** only price-level diffs are available; order-level state is not reconstructable.

**Rule:** L3 is opt-in per venue/stream. If the feed does not provide full order lifecycle semantics, the engine must run in L2 mode.

### 3.4 Binance L2 Diff Depth Protocol (Concrete Invariant)

Binance L2 diff streams require a strict snapshot+diff protocol:
1. Subscribe to the diff stream and start buffering all events.
2. Fetch a REST snapshot (`lastUpdateId`).
3. Discard buffered events where `u <= lastUpdateId`.
4. The first applied event must satisfy: `U <= lastUpdateId + 1` AND `u >= lastUpdateId + 1`.
5. Thereafter, enforce continuity: `new_event.U == prev_event.u + 1`.

**Failure handling:** If any continuity check fails, the local book is invalid.
Policy options:
- panic (deterministic research),
- quarantine (mark window and skip),
- resync (drop state, refetch snapshot, reapply buffered diffs).

## 4. Message Protocol for Order Book Replay (Concrete Draft)

To make replay deterministic and lossless, the incremental order book protocol must be explicit, versioned, and venue-aware.

### 4.1 Record Header (Required for Every Event)

```rust
#[repr(C)]
pub struct BookEventHeader {
    pub schema_version: u16,
    pub record_len: u16,       // bytes including header + payload
    pub endianness: u8,        // 0 = LE, 1 = BE
    pub venue_id: u16,         // stable numeric ID
    pub market_id: u32,        // stable numeric ID for symbol/venue
    pub stream_id: u32,        // optional: per-connection stream
    pub ingest_ts_ns: u64,     // monotonic ingest time
    pub exchange_ts_ns: u64,   // exchange event time (if known)
    pub seq: u64,              // chronicle ingest sequence
    pub native_seq: u64,       // venue native sequence or last update ID
    pub event_type: u8,        // Snapshot/Diff/Reset/Heartbeat
    pub book_mode: u8,         // 0 = L2, 1 = L3
    pub flags: u16,            // future use, alignment
}
```

**Notes:**
- `seq` is the strict replay ordering key when populated; if set to 0, use `MessageHeader.seq` as canonical.
- `native_seq` preserves venue ordering semantics for validation.
- All fixed-point numbers below must include an implied scale (per stream or per event).

### 4.2 L2 Diff Payload (Price-Level Updates)

```rust
#[repr(C)]
pub struct L2Diff {
    pub update_id_first: u64,  // venue-native U (if applicable)
    pub update_id_last: u64,   // venue-native u (if applicable)
    pub update_id_prev: u64,   // venue-native pu (if applicable)
    pub price_scale: u8,       // 10^-scale
    pub size_scale: u8,        // 10^-scale
    pub flags: u16,            // e.g., ABSOLUTE vs DELTA
    pub bid_count: u16,
    pub ask_count: u16,
    // followed by bid_count + ask_count records
}

#[repr(C)]
pub struct PriceLevelUpdate {
    pub price: u64,            // fixed-point
    pub size: u64,             // fixed-point
}
```

**Semantics:**
- If `flags` contains `ABSOLUTE`, `size` replaces the level.
- If `flags` contains `DELTA`, `size` is added (can be negative if signed, or use separate flag).
- `bid_count` and `ask_count` define the number of updates that follow (bids first, then asks).

### 4.3 Snapshot Payload (Optional Accelerator)

Snapshots are optional and only used for warm starts. They must declare the same scales and event metadata.

```rust
#[repr(C)]
pub struct L2Snapshot {
    pub price_scale: u8,
    pub size_scale: u8,
    pub bid_count: u32,
    pub ask_count: u32,
    // followed by bid_count + ask_count PriceLevelUpdate entries
}
```

### 4.4 L3 Diff Payload (Order-Level Updates, Venue-Dependent)

```rust
#[repr(C)]
pub struct L3Diff {
    pub price_scale: u8,
    pub size_scale: u8,
    pub action: u8,            // Add/Cancel/Execute/Replace
    pub side: u8,              // Bid/Ask
    pub _pad: u8,
    pub order_id: u128,        // venue order ID
    pub price: u64,
    pub size: u64,
}
```

**Rule:** L3 is only valid if the venue provides order lifecycle semantics. Otherwise run in L2 mode.

### 4.5 Fixed Binary Layout Example (Wire-Compat, Top-Down)

The layout below is a concrete example to make wire compatibility unambiguous. All integers are little-endian. Offsets are byte offsets from the start of each structure. Alignment uses `#[repr(C)]` rules; trailing padding is explicit.

#### 4.5.1 Persistent Queue Layout (Top-Level)

```
<queue_root>/
  <bus>/<stream>/
    segment_000000.q   // fixed-size segment file
    segment_000001.q
    ...
    segment_000000.idx // optional sparse time->seq index
    ...
```

#### 4.5.2 Segment File Layout

```
segment_N.q
  [0x0000..0x007F]  SegmentHeader (128 bytes)
  [0x0080..end]     Record(s) appended sequentially
```

SegmentHeader (128 bytes, little-endian):

```
Offset  Size  Field
0x0000  8     magic = "CHRSEG1\0"
0x0008  2     segment_version
0x000A  2     header_len = 128
0x000C  4     segment_id
0x0010  8     segment_len_bytes
0x0018  8     create_ts_ns
0x0020  8     writer_id
0x0028  8     first_seq (optional, 0 if unknown until finalized)
0x0030  8     last_seq  (optional, 0 if unknown until finalized)
0x0038  4     flags (future use)
0x003C  4     reserved
0x0040  64    reserved / future expansion
```

#### 4.5.3 Record Layout (Header + Payload)

```
Record
  [0x0000..0x003F]  MessageHeader (64 bytes, from chronicle-core)
  [0x0040..0x0077]  BookEventHeader (56 bytes, includes trailing pad)
  [0x0078..]        Payload (record_len - 56 bytes)
```

MessageHeader (64 bytes, little-endian, from `chronicle-core`):

```
Offset  Size  Field
0x0000  4     commit_len (0 = uncommitted, >0 = payload_len + 1)
0x0004  4     pad0
0x0008  8     seq
0x0010  8     timestamp_ns
0x0018  2     type_id
0x001A  2     flags
0x001C  4     reserved_u32
0x0020  32    pad
```

**Note:** `type_id` should come from a shared enum in `chronicle-protocol` (e.g., `TypeId::BookEvent`) rather than ad-hoc values per writer. Reserve `PAD_TYPE_ID = 0xFFFF` for padding.

BookEventHeader (56 bytes, little-endian, payload prefix):

```
Offset  Size  Field
0x0000  2     schema_version
0x0002  2     record_len
0x0004  1     endianness (0=LE, 1=BE)
0x0005  1     pad0
0x0006  2     venue_id
0x0008  4     market_id
0x000C  4     stream_id
0x0010  8     ingest_ts_ns
0x0018  8     exchange_ts_ns
0x0020  8     seq
0x0028  8     native_seq
0x0030  1     event_type
0x0031  1     book_mode
0x0032  2     flags
0x0034  4     pad1 (align to 8, total header = 56)
```

#### 4.5.4 L2 Diff Payload Layout (Example)

```
L2Diff
  [0x0000..0x001F]  L2Diff header (32 bytes)
  [0x0020..]        PriceLevelUpdate array (bid_count + ask_count entries)
```

L2Diff header (32 bytes):

```
Offset  Size  Field
0x0000  8     update_id_first (U)
0x0008  8     update_id_last  (u)
0x0010  8     update_id_prev  (pu)
0x0018  1     price_scale
0x0019  1     size_scale
0x001A  2     flags (ABSOLUTE/DELTA)
0x001C  2     bid_count
0x001E  2     ask_count
```

PriceLevelUpdate (16 bytes each):

```
Offset  Size  Field
0x0000  8     price (fixed-point)
0x0008  8     size  (fixed-point)
```

**Notes:**
- `price_scale`/`size_scale` are per-event to preserve exact decimal precision.
- `record_len` in `BookEventHeader` must match `56 + payload_len`.
- `commit_len` in `MessageHeader` must match `record_len + 1`.

### 4.6 Ingest Adapter: Tardis CSV incremental_book_L2 (Absolute)

Tardis `incremental_book_L2` rows are price-level updates with absolute sizes. Multiple rows may belong to one exchange message; group by `local_timestamp` and apply the group atomically.

**Field mapping (per row):**
- `exchange` -> `venue_id` (stable mapping table)
- `symbol` -> `market_id` (stable mapping table)
- `timestamp` (microseconds) -> `exchange_ts_ns = timestamp * 1_000`
- `local_timestamp` (microseconds) -> `ingest_ts_ns = local_timestamp * 1_000`
- `is_snapshot` -> `event_type` (Snapshot vs Diff)
- `side` -> bid/ask bucket
- `price`, `amount` -> fixed-point `price`/`size`

**Grouping rule:**
- Group all consecutive rows with the same `local_timestamp` into one Chronicle record.
- Treat each group as one message; apply all levels in the group as a single atomic update.

**Reset rule:**
- If `is_snapshot` becomes true after a run of diffs, emit a `Reset` event (optional) then a `Snapshot` event, or have `Snapshot` imply reset in replay.

**Update semantics:**
- Sizes are **absolute**. `amount = 0` means remove the level.
- No native sequence ID is provided; use `native_seq = 0` or synthesize a per-stream counter.

**Record layout:**
- `MessageHeader.type_id` = `BookEvent`
- Payload = `BookEventHeader` + `L2Diff` (or `L2Snapshot` if `is_snapshot`)

## 5. Snapshots & Warm Starts

Replaying 5 years of ticks to test a strategy on the last 10 minutes is non-viable.

### 5.1 Periodic Snapshots
The system supports "Checkpoints":
-   **Frequency:** e.g., Every midnight UTC or every 10GB of data.
-   **Format:** A serialized dump of the `L3Book` struct (using SBE or raw binary).
-   **Naming:** `snapshot_<seq_num>.bin`.

**Snapshot metadata (required):**
- `seq_num` at which the snapshot is valid (next replay starts at `seq_num + 1`).
- `schema_version` and `endianness`.
- `exchange_ts_range` and `ingest_ts_range`.
- Optional `book_hash` for deterministic verification.

### 5.2 Checkpoint Binary Layout (Wire-Compat)

Snapshots are simple, versioned blobs with a fixed header followed by the serialized book.
All integers are little-endian.

```
snapshot_<seq>.bin
  [0x0000..0x007F]  SnapshotHeader (128 bytes)
  [0x0080..]        SnapshotPayload (book state bytes)
```

SnapshotHeader (128 bytes):

```
Offset  Size  Field
0x0000  8     magic = "CHRCKPT1"
0x0008  2     schema_version
0x000A  2     header_len = 128
0x000C  2     endianness (0=LE, 1=BE)
0x000E  2     book_mode (0=L2, 1=L3)
0x0010  4     venue_id
0x0014  4     market_id
0x0018  8     seq_num (snapshot valid at this seq)
0x0020  8     ingest_ts_ns_start
0x0028  8     ingest_ts_ns_end
0x0030  8     exchange_ts_ns_start
0x0038  8     exchange_ts_ns_end
0x0040  16    book_hash (optional, zero if unused)
0x0050  8     payload_len
0x0058  8     flags (future use)
0x0060  32    reserved / future expansion
```

**Notes:**
- `payload_len` must match the exact serialized book size.
- `book_hash` should be a stable hash of the in-memory book to support determinism checks.
- For L2 snapshots, the payload should be a compact list of price levels (bids then asks).
- For L3 snapshots, include full order table plus aggregated levels if you need fast top-of-book access.

### 4.2 Resume Protocol
1.  Researcher requests: "Replay from 2023-10-27 09:30:00".
2.  Engine finds latest snapshot *before* that time: `snapshot_10500000.bin`.
3.  Load Snapshot into `L3Book` -> State is now at `Seq 10,500,000`.
4.  Open Chronicle Queue.
5.  Seek/Scan to `Seq 10,500,001`.
6.  Replay fast-forward until `09:30:00`.
7.  Begin Strategy simulation.

## 6. Validation Standards

A replay is only valid if it proves its own integrity.

1.  **Sequence Continuity:**
    -   Must track `last_seq`. Panic on gaps.
    -   Reason: A missing packet means we missed an order. Our book is now crossed or wrong.

2.  **Timestamp Monotonicity:**
    -   `timestamp_ns` (Ingest) should generally increase.
    -   If `exchange_ts` drifts significantly backwards, flag "Clock Skew" or "Reordering".

3.  **State Hashing (Determinism):**
    -   At the end of the day, compute `hash(book)`.
    -   This hash must match the `hash` generated by the production system (if it dumps state) or previous runs.

## 7. Robustness Guardrails

### 7.1 Record Compatibility

Zero-copy parsing only works if the record layout is stable and explicit:
- `#[repr(C)]` for all on-disk structs.
- A versioned record or segment header that includes:
  - `schema_version`, `record_len`, `endianness`, `writer_id`, `segment_id`.
- Optional checksum (CRC32/xxhash) per record or per block for corruption detection.

### 7.2 Gap and Reorder Policy

Sequence gaps are fatal for correctness, but different workloads need different behavior.
- **panic:** fail fast for deterministic research.
- **quarantine:** mark a bad window, continue outside it.
- **resync:** attempt to re-snapshot and resume if the feed supports it.

Make this a configurable policy instead of hard-coded panic.

### 7.3 Indexing for Seek

To avoid O(n) scans on multi-year logs, add a sparse index per segment:
- Map `timestamp_ns -> seq` every N records.
- Store in a sidecar file (e.g., `segment_000123.idx`).
- Replay seeks become `O(log n)` to find the nearest block, then fast-forward.

## 8. Python Integration (The "Analyst" Interface)

While the Core Engine is Rust, researchers live in Python/Jupyter.

**PyO3 Bindings:**
-   Wrap `ReplayEngine` as a Python Class.
-   Expose `next_tick()` iterator.
-   Convert Rust `L3Book` to `numpy` arrays or `pandas` DataFrame on demand (expensive, use sparingly).

```python
# Intended usage
engine = chronicle.ReplayEngine("/data/chronicle/binance_spot", symbol="BTCUSDT")
engine.seek_time("2024-01-20T10:00:00")

for event in engine:
    features = engine.book.get_features() # [spread, imbalance, vwap]
    alpha = my_model.predict(features)
    # ...
```
