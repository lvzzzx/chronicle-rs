# Chronicle Refactoring Summary

## Overview

Completed bottom-up refactoring to create a clean, layered architecture:
- **Core**: Low-level queue primitives
- **IPC**: Hot path communication patterns
- **Table**: Cold path time-series storage
- **Lifecycle**: Storage management (compression, tiering)
- **Stream**: Application-level utilities (order book replay, ETL)

---

## Phase 1: Module Cleanup (Completed)

### Removed Modules

#### `src/ingest/` (Full Removal)
**Reason**: Replaced by new `table` module architecture
**Deleted**:
- `ingest/feed/binance.rs`: Binance WebSocket feed
- `ingest/import/tardis_csv.rs`: Tardis CSV importer
- `ingest/import/szse_l3_csv.rs`: SZSE L3 importer
- `ingest/market.rs`: Market ID utilities
- 3 binary files: `chronicle_feed_binance`, `chronicle_csv_import`, `chronicle_szse_import`

#### `src/storage/` (Full Removal - Previous)
**Reason**: Merged into unified `table` and `lifecycle` modules
**Deleted**: All storage-related code migrated to new architecture

#### `src/layout/` (Full Removal)
**Reason**: Path management incorporated into `ipc` and `table` modules
**Deleted**: `IpcLayout`, `StreamsLayout`, `ArchiveLayout`, `RawArchiveLayout`

#### `src/stream/` (Partial Cleanup)
**Phase 1 Cleanup Completed**:
- ❌ Deleted `stream/archive.rs`: Broken stub (depends on deleted storage module)
- ❌ Deleted `stream/live.rs`: Useless wrapper around `QueueReader`
- ❌ Deleted `stream/merge.rs`: Duplicate of `ipc::fanin::FanInReader`
- ✅ Moved `stream/sequencer/` → `core/sequencer/`: Gap detection is core utility

**Kept (Application Utilities)**:
- ✅ `stream/replay/`: Order book replay engine (L2Book state machine)
- ✅ `stream/etl/`: ETL feature extraction and Parquet sinks

---

## Current Architecture

```
src/
├── core/               # Low-level primitives
│   ├── queue/         # SPSC queue with segments
│   ├── log/           # Append-only log for batch processing
│   ├── compression/   # Unified zstd compression layer
│   └── sequencer/     # Gap detection (moved from stream)
│
├── ipc/                # Hot path - Real-time IPC patterns
│   ├── pubsub/        # Pub/Sub (broadcast)
│   ├── bidirectional/ # Bidirectional channel (SPSC duplex)
│   └── fanin/         # Fan-in merge (many-to-one)
│
├── table/              # Cold path - Time-series storage
│   ├── log/           # TimeSeriesLog (timestamp-indexed)
│   ├── table/         # Partitioned table with rolling
│   ├── rollers/       # Partition rollers (date, hour, minute)
│   └── table_reader/  # Multi-partition query reader
│
├── lifecycle/          # Storage lifecycle management
│   ├── manager/       # StorageLifecycleManager
│   ├── compressor/    # Segment compression
│   ├── policy/        # Lifecycle policies
│   └── access_tracker/# Access tracking for tiering
│
├── stream/             # Application utilities (domain-specific)
│   ├── replay/        # Order book replay engine
│   └── etl/           # ETL feature extraction
│
├── protocol/           # Market data schemas
├── venues/             # Venue-specific adapters
├── trading/            # Trading domain logic
├── bus/                # High-level bus abstraction (uses ipc + table)
└── cli/                # CLI tools
```

---

## Key Design Decisions

### 1. Hot Path vs Cold Path Separation

**Hot Path (IPC)**:
- Low latency, real-time streaming
- Writer lock, reader cursors, wait strategies
- No partition filtering, linear replay
- Used for: Live market data, order routing, strategy signals

**Cold Path (Table)**:
- Batch queries, historical data
- Partition-based filtering, timestamp ranges
- No writer lock (offline writes via lifecycle)
- Used for: Backtesting, analytics, data archival

**Rationale**: These have fundamentally different use cases and shouldn't share abstractions.

### 2. Stream Module Positioning

**Decision**: Keep as application-level utilities, not infrastructure.

**Rationale**:
- Order book replay (`L2Book`) is domain-specific market data logic
- ETL feature extraction is application-level analytics
- Not general-purpose infrastructure like IPC/Table

**Alternative considered**: Remove entirely, but used by:
- `bin/chronicle_etl.rs`
- `bin/snapshotter.rs`
- `venues/szse/l3/`

### 3. Sequencer to Core

**Decision**: Moved `stream/sequencer/` → `core/sequencer/`

**Rationale**: Gap detection is a general message stream utility, not specific to replay/ETL.

### 4. No Stream Abstractions

**Decision**: Remove `LiveStream`, `ArchiveStream`, `StreamReader` trait

**Rationale**:
- Trying to unify hot/cold paths with one trait obscures their differences
- Users should call `ipc::Subscriber` or `table::TableReader` directly
- Kept minimal adapter (`QueueReaderAdapter`) for replay engine polymorphism

---

## Migration Guide

### If you were using deleted modules:

#### `ingest` module → Use `table` module
```rust
// OLD: ingest module
use chronicle::ingest::import::tardis_csv::import_tardis_incremental_book;

// NEW: table module with rolling writer
use chronicle::table::{Table, DateRoller};
let table = Table::open("./data")?;
let mut writer = table.rolling_writer(DateRoller::new())?;
writer.append(type_id, timestamp_ns, &data)?;
```

#### `stream::LiveStream` → Use `ipc::Subscriber`
```rust
// OLD: stream wrapper
use chronicle::stream::LiveStream;
let mut reader = LiveStream::open(path, "reader")?;

// NEW: direct IPC usage
use chronicle::ipc::Subscriber;
let mut reader = Subscriber::open(path, "reader")?;
```

#### `stream::merge::FanInReader` → Use `ipc::fanin::FanInReader`
```rust
// OLD: stream merge
use chronicle::stream::merge::FanInReader;

// NEW: IPC fanin
use chronicle::ipc::FanInReader;
```

#### `stream::sequencer` → Use `core::sequencer`
```rust
// OLD: stream sequencer
use chronicle::stream::sequencer::{SequenceValidator, GapPolicy};

// NEW: core sequencer
use chronicle::core::sequencer::{SequenceValidator, GapPolicy};
```

---

## Remaining Work (Phase 2 - Optional)

### Decision Point: Stream Module Future

**Option A: Keep as-is** (Current state)
- Stream module contains application utilities
- Clear separation: infrastructure vs application code
- No action needed

**Option B: Rename for clarity**
```bash
mv src/stream/ src/analytics/
# or
mv src/stream/ src/applications/
```
- Makes it explicit this is not infrastructure
- Better aligns with layered architecture

**Option C: Move to separate applications**
```
src/
  core/, ipc/, table/, lifecycle/  # Infrastructure
applications/
  replay/     # Order book replay
  etl/        # ETL pipelines
```
- Cleanest separation
- Requires updating binary dependencies

### Recommendation
**Keep current structure (Option A)** unless you add more application-level utilities, then consider Option B for clarity.

---

## Test Results

All tests passing after refactor:
- **159** unit tests (core, ipc, table, lifecycle)
- **22** integration tests
- **Total: 181 tests ✅**

---

## Benefits Achieved

### 1. Clear Module Boundaries
- Each module has single, well-defined responsibility
- No overlap between hot path (IPC) and cold path (Table)

### 2. Reduced Duplication
- Eliminated duplicate FanInReader implementations
- Moved shared utilities (sequencer) to core

### 3. Better Discoverability
- Users know exactly where to look:
  - Real-time streaming? → `ipc`
  - Historical queries? → `table`
  - Storage management? → `lifecycle`

### 4. Architecture Alignment
- Post-refactor structure matches design intent
- Infrastructure clearly separated from application code

---

## Files Changed Summary

### Commits
1. `c628cc1`: Remove ingest module (11 files, -2575 lines)
2. `d721fa2`: Remove layout module (3 files, -439 lines)
3. `5fa4a5d`: Remove storage binaries/tests (6 files, -624 lines)
4. `f5c7ebb`: Fix compilation errors (7 files, -324 lines)
5. `b9dc89f`: Phase 1 stream cleanup (11 files, +372/-402 lines)

### Total Impact
- **38 files** modified or deleted
- **~4,400 lines** of obsolete code removed
- **Core architecture clarified**
- All tests passing

---

## Documentation Created
- `STREAM_MODULE_ANALYSIS.md`: Detailed stream module review
- `REFACTOR_SUMMARY.md`: This file (overall summary)

---

## Next Steps (If Desired)

1. **Update examples/**: Update any examples using deleted modules
2. **Documentation**: Update high-level docs to reflect new architecture
3. **Benchmarks**: Verify performance characteristics unchanged
4. **Optional**: Consider renaming `stream/` → `analytics/` for clarity

---

**Status**: Phase 1 complete ✅
**Architecture**: Clean and maintainable ✅
**Tests**: All passing ✅
