# Stream Module Analysis

## Executive Summary

After the refactoring that created the **ipc** (hot path) and **table** (cold path) modules, the **stream** module has become **misaligned** with the new architecture. It contains a mix of:
- Outdated abstraction layers
- Business logic (order book replay)
- Duplicate functionality
- Broken code

**Recommendation**: Refactor or remove, depending on your application needs.

---

## Current Architecture Overview

```text
┌─────────────────────────────────────────────────────────────┐
│                     Applications/Binaries                    │
│              (feeds, strategies, ETL pipelines)              │
├─────────────────────────────────────────────────────────────┤
│  Domain Layer                                                │
│  - trading/: Trading-specific logic                          │
│  - venues/: Venue-specific adapters (SZSE, etc.)            │
│  - protocol/: Market data schemas                            │
├─────────────────────────────────────────────────────────────┤
│  Infrastructure Layer (POST-REFACTOR)                        │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐      │
│  │   ipc/       │  │   table/     │  │  lifecycle/  │      │
│  │  Hot path    │  │  Cold path   │  │  Tiering     │      │
│  │  Real-time   │  │  Batch       │  │  Compress    │      │
│  └──────────────┘  └──────────────┘  └──────────────┘      │
├─────────────────────────────────────────────────────────────┤
│  core/: Low-level primitives                                 │
│  (Queue, Segments, Readers, Writers, Compression)           │
└─────────────────────────────────────────────────────────────┘
```

---

## Stream Module Contents (Current State)

### 1. **Abstraction Layer** (Questionable Value)

**Files**: `mod.rs`, `live.rs`, `archive.rs`

```rust
// Defines traits to unify live and archive reading
pub trait StreamReader {
    fn next(&mut self) -> Result<Option<StreamMessageRef>>;
    fn commit(&mut self) -> Result<()>;
    fn seek_timestamp(&mut self, ts: u64) -> Result<bool>;
}

pub struct LiveStream { reader: QueueReader }  // Thin wrapper
pub struct ArchiveStream { /* BROKEN - TODO */ }
```

**Issues**:
- `LiveStream` is just a thin wrapper around `core::QueueReader` with no added value
- `ArchiveStream` is broken/stubbed out (depends on deleted `storage` module)
- The `StreamReader` trait tries to unify hot/cold paths, but they have fundamentally different characteristics:
  - **Hot path (IPC)**: Low latency, blocking, wait strategies, live data
  - **Cold path (Table)**: Batch queries, partition filters, timestamp ranges, historical data
- This abstraction predates your ipc/table split and is now obsolete

### 2. **Duplicate Functionality**

**File**: `merge.rs`

```rust
pub struct FanInReader {
    readers: Vec<QueueReader>,
    // Timestamp-ordered merge of multiple queues
}
```

**Issue**: This is **identical functionality** to `ipc::fanin::FanInReader`! You have two implementations of the same thing:
- `stream::merge::FanInReader`
- `ipc::fanin::FanInReader`

One should be removed.

### 3. **Business Logic** (Domain-Specific)

**Directory**: `replay/`

Contains:
- `L2Book`: Order book state machine (apply snapshots/diffs)
- `ReplayEngine`: Generic replay system with gap detection
- `snapshot::SnapshotWriter`: Periodic snapshot generation
- `pipeline::ReplayPipeline`: Transform/sink architecture
- `parallel::MultiSymbolReplay`: Multi-threaded replay

**Value**: This is **useful business logic** for market data applications, but it's **domain-specific** (order book mechanics), not general infrastructure.

### 4. **ETL Pipeline** (Application-Level)

**Directory**: `etl/`

Contains:
- Feature extraction from order book events
- Parquet sink for analytics
- Symbol catalog
- Time-based triggers

**Value**: Useful for analytics pipelines, but this is **application code**, not infrastructure.

### 5. **Reusable Utility**

**Directory**: `sequencer/`

```rust
pub struct SequenceValidator {
    // Detects gaps and out-of-order messages
}
```

**Value**: Generally useful for any message stream. Could be moved to `core/` or kept if stream module is retained.

---

## Alignment Issues with New Architecture

### Problem 1: Abstraction Impedance Mismatch

The `StreamReader` trait assumes a unified model:
```rust
trait StreamReader {
    fn next(&mut self) -> Result<Option<StreamMessageRef>>;
    fn commit(&mut self) -> Result<()>;
}
```

But your new architecture has **two different models**:

**IPC Model** (Hot Path):
```rust
let mut subscriber = Subscriber::open(path, "reader")?;
while let Some(msg) = subscriber.recv()? {
    process(msg);
    subscriber.commit()?;  // Live cursor tracking
}
```

**Table Model** (Cold Path):
```rust
let table = Table::open(path)?;
let filter = PartitionFilter::Range {
    key: "date",
    start: "2026-01-01",
    end: "2026-01-31"
};
let mut reader = table.query(filter)?;  // Batch query, no commit
while let Some(msg) = reader.next()? {
    process(msg);
}
```

These have **fundamentally different semantics**. Forcing them into one trait doesn't add value—it obscures the differences.

### Problem 2: `archive.rs` is Broken

```rust
//! **TODO**: This module needs to be migrated to use `table::TableReader`
// COMMENTED OUT - Pending migration to table::TableReader
```

This needs either:
- Migration to use `table::TableReader`
- Deletion if not needed

### Problem 3: Unclear Module Boundary

What is "stream"? The current module mixes:
- Infrastructure (abstractions, merging)
- Business logic (order book replay)
- Applications (ETL pipelines)

This violates separation of concerns.

---

## Recommendation: Three Options

### Option 1: **Remove Stream Module** (Cleanest)

**If you don't need order book replay or ETL**, remove the entire module:

```
✗ src/stream/mod.rs        → DELETE
✗ src/stream/live.rs        → DELETE (users call QueueReader directly)
✗ src/stream/archive.rs     → DELETE (use table::TableReader)
✗ src/stream/merge.rs       → DELETE (use ipc::fanin)
✗ src/stream/replay/        → DELETE or move to applications/
✗ src/stream/etl/           → DELETE or move to applications/
✓ src/stream/sequencer/     → Move to core/sequencer
```

**Migration path**:
- Replace `LiveStream` with `ipc::Subscriber` or `core::QueueReader`
- Replace `ArchiveStream` with `table::TableReader`
- Replace `stream::merge::FanInReader` with `ipc::fanin::FanInReader`
- Move replay/ETL to separate applications if needed

### Option 2: **Refactor as Application Layer** (If You Need Replay/ETL)

Keep stream module but reposition it as **application-level utilities**, not infrastructure:

```
src/
  core/           ← Infrastructure: Queue primitives
  ipc/            ← Infrastructure: IPC patterns
  table/          ← Infrastructure: Time-series storage
  lifecycle/      ← Infrastructure: Storage lifecycle
  stream/         ← APPLICATION LAYER
    replay/       ← Order book replay engine
    etl/          ← ETL feature extraction
    sequencer/    ← Gap detection (could move to core)
```

**Changes**:
1. Remove abstraction layer (`StreamReader`, `LiveStream`, `ArchiveStream`)
2. Remove duplicate `merge.rs` (use `ipc::fanin`)
3. Keep `replay/` and `etl/` as **domain-specific application utilities**
4. Update documentation to clarify this is **not** infrastructure

Rename module to `src/applications/` or `src/analytics/` to make it clear.

### Option 3: **Hybrid - Keep Minimal Stream Layer** (Middle Ground)

If you want to keep some abstraction for polymorphism:

```rust
// Minimal trait for "readable message streams"
pub trait MessageStream {
    fn next(&mut self) -> Result<Option<MessageView>>;
}

// Adapters for different sources
impl MessageStream for QueueReader { ... }
impl MessageStream for TableReader { ... }

// Business logic that works with any source
pub struct ReplayEngine<S: MessageStream> {
    source: S,
    book: L2Book,
}
```

But remove the redundant wrappers and broken code.

---

## Recommended Action Plan

### Phase 1: Cleanup (Immediate)
1. **Delete** `src/stream/archive.rs` (broken stub)
2. **Delete** `src/stream/merge.rs` (duplicate of `ipc::fanin`)
3. **Move** `src/stream/sequencer/` → `src/core/sequencer/`
4. Update imports in tests/binaries

### Phase 2: Decision (Strategic)
Decide based on your application needs:

**If you actively use order book replay/ETL**:
- Keep `src/stream/replay/` and `src/stream/etl/`
- Remove `LiveStream`/`ArchiveStream` wrappers
- Document stream module as "application utilities" not infrastructure
- Consider renaming to `src/analytics/` or similar

**If you don't need replay/ETL**:
- Remove entire `src/stream/` module
- Migrate any external users to `ipc::Subscriber` or `table::TableReader`

### Phase 3: Documentation
Update architecture docs to clarify:
- `core/`: Primitives
- `ipc/`: Hot path patterns
- `table/`: Cold path storage
- `stream/` or `analytics/`: Application-level domain logic (if kept)

---

## Current Usage Analysis

From code search, stream module is used by:
- `src/bin/chronicle_etl.rs`: Uses `stream::etl`
- `src/bin/snapshotter.rs`: Uses `stream::replay`
- `src/venues/szse/l3/`: Uses `stream::sequencer`
- Tests: `warm_start.rs` uses `stream::replay`

**Impact**: If you remove stream module, these binaries/tests need updates.

---

## My Strong Recommendation

**Phase 1 immediately**, then:

- If ETL/replay are **core to your use case**: Go with **Option 2** (refactor as application layer)
- If ETL/replay are **not essential**: Go with **Option 1** (remove entirely)

The current state is confusing and misaligned with your new architecture. The abstraction layer (`StreamReader`, `LiveStream`) adds no value post-refactor.
