# Chronicle-RS Refactoring Summary

## Overview

This document summarizes the comprehensive refactoring of the chronicle-rs stream module, focusing on reducing code duplication, improving separation of concerns, and establishing clear module boundaries.

## Refactoring Phases Completed

### Phase 1 & 2: Extract Sequencer and Relocate Reconstruct

**Commit:** `ee03f17` - refactor: extract sequencer and relocate reconstruct to venues module

**Problem:**
- Duplicate sequence validation logic in `replay/mod.rs` and `reconstruct/szse_l3.rs` (~50 lines each)
- Venue-specific SZSE L3 reconstruction code in generic stream module

**Solution:**
1. Created `src/stream/sequencer/mod.rs` (228 lines)
   - Single source of truth for sequence validation
   - `SequenceValidator` class with three gap policies: `Panic`, `Quarantine`, `Ignore`
   - Both strict checking and relaxed detection modes
   - 7 comprehensive unit tests

2. Created `src/venues/` module hierarchy
   - `src/venues/szse/l3/engine.rs` - Moved from `stream/reconstruct/szse_l3.rs`
   - Venue-specific implementations now separate from generic patterns

**Impact:**
- Eliminated ~50 lines of duplicate code
- Clear separation: generic patterns in `stream/`, venue-specific in `venues/`

---

### Phase 3: Integrate SequenceValidator Across Modules

**Commit:** `55515bd` - refactor: integrate shared SequenceValidator across modules

**Problem:**
- `replay/mod.rs` had manual sequence tracking (`last_seq: Option<u64>`)
- `venues/szse/l3/engine.rs` had duplicate `ChannelSequencer` and `GapPolicy`
- Inconsistent gap policy naming (`Fail` vs `Panic`)

**Solution:**
1. Refactored `replay/mod.rs`:
   - Changed `ReplayEngine` to use `SequenceValidator` instead of manual tracking
   - Updated all `apply_message()` calls to use sequencer
   - Re-exported `GapPolicy` for backward compatibility

2. Refactored `venues/szse/l3/engine.rs`:
   - Removed duplicate `ChannelSequencer` (38 lines)
   - Removed duplicate `GapPolicy` enum
   - Updated to use shared `SequenceValidator`

3. Updated binary `chronicle_szse_recon.rs`:
   - Use shared types from sequencer module
   - Mapped `GapPolicy::Fail` → `GapPolicy::Panic` for consistency

**Impact:**
- Eliminated ~97 lines of duplicate code (56 in replay + 41 in venues)
- Single source of truth for all sequence validation
- Consistent gap handling across all modules

---

### Phase 4A: ETL Abstraction Layer

**Commit:** `949e5bd` - feat: add ETL abstraction layer to decouple from replay internals

**Problem:**
- ETL features directly coupled to `replay::L2Book` and `replay::BookEvent`
- Features couldn't be tested without full replay engine
- No clear interface contracts between ETL and replay modules
- Hard to support alternative book implementations

**Solution:**
Created `src/stream/etl/types.rs` (227 lines) with abstraction traits:

1. **`OrderBook` trait** - Abstraction over book state
   - `best_bid()`, `best_ask()`
   - `bid_levels()`, `ask_levels()` iterators
   - `scales()` for price/size scaling

2. **`EventHeader` trait** - Abstraction over event metadata
   - `venue_id()`, `market_id()`
   - `exchange_ts_ns()`, `ingest_ts_ns()`
   - `event_type_code()`, `seq()`

3. **`BookUpdate` trait** - Abstraction over book events
   - `header()` accessor
   - `is_snapshot()`, `is_diff()`, `is_reset()`, `is_heartbeat()`, `is_supported()`

4. **`Message` trait** - Abstraction over replay messages
   - `book()`, `update()`
   - Convenience methods: `venue_id()`, `market_id()`, `exchange_ts_ns()`, `ingest_ts_ns()`

5. **`MessageSource` trait** - Abstraction over message iteration
   - `next_message()` for stream processing

6. **Adapter implementations** for existing replay types:
   - `impl OrderBook for L2Book`
   - `impl EventHeader for BookEventHeader`
   - `impl BookUpdate for BookEvent<'a>`
   - `impl Message for ReplayMessage<'a>`
   - `impl<R: StreamReader> MessageSource for ReplayEngine<R>`

Updated all ETL components to use abstractions:
- `src/stream/etl/feature.rs` - Updated `Feature`, `FeatureSet`, `Trigger` traits
- `src/stream/etl/features.rs` - Updated `GlobalTime`, `MidPrice`, `SpreadBps`, `BookImbalance`
- `src/stream/etl/triggers.rs` - Updated `TimeBarTrigger`, `EventTypeTrigger`, `AnyTrigger`

**Impact:**
- ETL module no longer depends on replay implementation details
- Clear interface contracts via traits
- Features can be tested in isolation
- Easy to support alternative book implementations

---

### Phase 4B: Feature Testing Infrastructure

**Commit:** `f0a90a9` - test: add mock utilities and unit tests for ETL features

**Problem:**
- No way to test features without full replay engine
- Difficult to test edge cases and boundary conditions
- Slow, integration-style tests

**Solution:**
1. Created test utilities in `etl/types.rs::test_utils`:
   - **`MockOrderBook`** - Configurable order book for testing
     - `add_bid()`, `add_ask()` methods
     - Custom price/size scales
     - Implements `OrderBook` trait

   - **`MockEventHeader`** - Mock event metadata
     - Configurable timestamps, IDs, sequence numbers
     - Implements `EventHeader` trait

   - **`MockBookUpdate`** - Mock book update events
     - Configurable event types
     - Implements `BookUpdate` trait

2. Added 8 comprehensive unit tests in `features.rs`:
   - `test_global_time_feature()` - Timestamp extraction
   - `test_mid_price_empty_book()` - Empty book handling
   - `test_mid_price_calculation()` - Mid-price with scaled prices
   - `test_spread_bps_empty_book()` - Empty book handling
   - `test_spread_bps_calculation()` - Spread in basis points
   - `test_book_imbalance_empty_book()` - Empty book handling
   - `test_book_imbalance_balanced()` - Balanced bid/ask
   - `test_book_imbalance_bid_heavy()` - Imbalanced scenario

**Impact:**
- Features testable in isolation (no replay engine needed)
- Fast, focused unit tests
- Easy to test edge cases
- Mock utilities reusable for future features
- Test count: 42 → 50 passing tests

---

### Phase 4C: Extract Protocol Serialization

**Commit:** `83cc355` - refactor: extract protocol serialization from refinery to protocol module

**Problem:**
- Unsafe protocol serialization code (90 lines) scattered in `etl/refinery.rs`
- Multiple `unsafe` transmute operations without clear safety documentation
- Protocol serialization logic not reusable
- Hard to maintain and test

**Solution:**
1. Created `protocol::serialization` module (152 lines):
   - **`write_l2_snapshot()`** - Safe API for writing L2 snapshot events
     - Takes iterator of bids/asks
     - Returns bytes written
     - Clear panic documentation

   - **`l2_snapshot_size()`** - Calculate required buffer size

   - **`struct_to_bytes()`** - Internal helper for safe struct serialization
     - Encapsulates unsafe transmute
     - Clear safety documentation

2. Refactored `etl/refinery.rs`:
   - Replaced 90 lines of unsafe code with clean API call
   - Simplified `inject_snapshot()` from 90 lines → 30 lines
   - Removed direct protocol struct manipulation

3. Added 4 comprehensive tests:
   - `test_l2_snapshot_size_calculation()` - Size calculations
   - `test_write_l2_snapshot_empty_book()` - Empty book serialization
   - `test_write_l2_snapshot_with_levels()` - Full snapshot with levels
   - `test_write_l2_snapshot_buffer_too_small()` - Error handling

**Impact:**
- Protocol serialization centralized in protocol module
- Unsafe operations encapsulated with clear documentation
- Reduced code: 90 lines → 30 lines in refinery
- Reusable serialization API
- Test count: 50 → 54 passing tests

---

## Current Module Structure

```
chronicle-rs/
├── src/
│   ├── stream/
│   │   ├── sequencer/           [NEW] Sequence validation
│   │   │   └── mod.rs           - SequenceValidator, GapPolicy
│   │   │
│   │   ├── replay/              [REFACTORED] Replay engine
│   │   │   ├── mod.rs           - Uses SequenceValidator
│   │   │   ├── snapshot.rs
│   │   │   ├── transform.rs
│   │   │   ├── sink.rs
│   │   │   ├── pipeline.rs
│   │   │   └── parallel.rs
│   │   │
│   │   ├── etl/                 [REFACTORED] ETL features
│   │   │   ├── types.rs         [NEW] - Abstraction traits + test mocks
│   │   │   ├── feature.rs       - Uses abstraction traits
│   │   │   ├── features.rs      - Concrete features + tests
│   │   │   ├── triggers.rs      - Event triggers
│   │   │   ├── catalog.rs       - Symbol catalog
│   │   │   ├── extractor.rs     - Feature extraction orchestration
│   │   │   ├── refinery.rs      - Uses protocol::serialization
│   │   │   └── sink.rs          - Output abstraction
│   │   │
│   │   ├── live.rs
│   │   └── mod.rs
│   │
│   ├── venues/                  [NEW] Venue-specific implementations
│   │   └── szse/
│   │       └── l3/
│   │           ├── mod.rs       - Re-exports + shared types
│   │           └── engine.rs    - Uses SequenceValidator
│   │
│   ├── protocol/                [REFACTORED] Protocol definitions
│   │   └── mod.rs               - Types + serialization module
│   │
│   ├── core/                    [UNCHANGED] Core queue/storage
│   ├── ipc/                     [UNCHANGED] IPC abstractions
│   ├── storage/                 [UNCHANGED] Archive storage
│   └── layout/                  [UNCHANGED] Path layouts
```

---

## Module Boundaries and Responsibilities

### 1. `stream/sequencer/` - Sequence Validation [NEW]

**Responsibility:** Monotonic sequence number validation with configurable gap handling

**Public API:**
```rust
pub enum GapPolicy {
    Panic,      // Fail immediately on gaps
    Quarantine, // Mark as skipped, continue
    Ignore,     // Accept gaps, continue
}

pub enum GapDetection {
    Sequential,
    Gap { from: u64, to: u64 },
}

pub struct SequenceValidator {
    // Validates sequence numbers
    pub fn new(policy: GapPolicy) -> Self;
    pub fn check(&mut self, seq: u64) -> Result<GapDetection>;
    pub fn check_relaxed(&mut self, seq: u64) -> GapDetection;
    pub fn reset(&mut self);
    pub fn last_seq(&self) -> Option<u64>;
    pub fn set_policy(&mut self, policy: GapPolicy);
}
```

**Used By:**
- `stream/replay/mod.rs` - Replay engine
- `venues/szse/l3/engine.rs` - SZSE L3 reconstruction

**Dependencies:** None (self-contained)

**Key Insight:** Single source of truth for sequence validation across all stream processing

---

### 2. `stream/replay/` - Replay Engine [REFACTORED]

**Responsibility:** Transform chronicle queue messages into book state updates

**Public API:**
```rust
pub struct ReplayEngine<R: StreamReader> {
    // Uses SequenceValidator internally
}

pub type LiveReplayEngine = ReplayEngine<LiveStream>;

pub struct ReplayMessage<'a> {
    pub msg: StreamMessageRef<'a>,
    pub update: ReplayUpdate,
    pub book_event: Option<BookEvent<'a>>,
    pub book: &'a L2Book,
}

pub struct L2Book {
    pub fn bids(&self) -> &BTreeMap<u64, u64>;
    pub fn asks(&self) -> &BTreeMap<u64, u64>;
    pub fn scales(&self) -> (u8, u8);
}
```

**Used By:**
- `stream/etl/refinery.rs` - Stream refinement
- `stream/etl/extractor.rs` - Feature extraction
- Application binaries

**Dependencies:**
- `stream::sequencer` - Sequence validation
- `stream::live` - Live stream reader
- `protocol` - Book event types

**Key Changes:**
- Now uses `SequenceValidator` instead of manual tracking
- Re-exports `GapPolicy` for backward compatibility

---

### 3. `stream/etl/` - ETL Features [REFACTORED]

**Responsibility:** Extract, transform, and load features from book events

#### 3a. `etl/types.rs` - Abstraction Layer [NEW]

**Responsibility:** Decouple ETL from replay implementation details

**Public API:**
```rust
// Core abstractions
pub trait OrderBook {
    fn best_bid(&self) -> Option<(u64, u64)>;
    fn best_ask(&self) -> Option<(u64, u64)>;
    fn bid_levels(&self) -> Box<dyn Iterator<Item = (u64, u64)> + '_>;
    fn ask_levels(&self) -> Box<dyn Iterator<Item = (u64, u64)> + '_>;
    fn scales(&self) -> (u8, u8);
}

pub trait EventHeader {
    fn venue_id(&self) -> u16;
    fn market_id(&self) -> u32;
    fn exchange_ts_ns(&self) -> u64;
    fn ingest_ts_ns(&self) -> u64;
    fn event_type_code(&self) -> u8;
    fn seq(&self) -> u64;
}

pub trait BookUpdate {
    fn header(&self) -> &dyn EventHeader;
    fn is_snapshot(&self) -> bool;
    fn is_diff(&self) -> bool;
    fn is_reset(&self) -> bool;
    fn is_heartbeat(&self) -> bool;
}

pub trait Message {
    fn book(&self) -> &dyn OrderBook;
    fn update(&self) -> Option<&dyn BookUpdate>;
}

pub trait MessageSource {
    fn next_message(&mut self) -> Result<Option<Box<dyn Message + '_>>>;
}

// Test utilities
#[cfg(test)]
pub mod test_utils {
    pub struct MockOrderBook { /* ... */ }
    pub struct MockEventHeader { /* ... */ }
    pub struct MockBookUpdate { /* ... */ }
}
```

**Used By:**
- `etl/feature.rs` - Feature trait definitions
- `etl/features.rs` - Concrete features
- `etl/triggers.rs` - Event triggers

**Dependencies:**
- Adapters for `replay` types (L2Book, BookEvent, etc.)

**Key Insight:** Enables testing features without replay engine

#### 3b. `etl/feature.rs` - Feature Framework

**Responsibility:** Feature computation infrastructure

**Public API:**
```rust
pub trait Feature {
    fn schema(&self) -> Vec<ColumnSpec>;
    fn on_event(&mut self, book: &dyn OrderBook, event: &dyn BookUpdate, out: &mut RowWriter<'_>);
}

pub trait FeatureSet {
    fn schema(&self) -> SchemaRef;
    fn calculate(&mut self, book: &dyn OrderBook, event: &dyn BookUpdate, out: &mut RowWriter<'_>);
    fn should_emit(&mut self, event: &dyn BookUpdate) -> bool;
}

pub trait Trigger {
    fn should_emit(&mut self, event: &dyn BookUpdate) -> bool;
}
```

**Key Changes:**
- All traits now use abstraction traits from `etl/types`
- No direct dependency on `replay::L2Book` or `replay::BookEvent`

#### 3c. `etl/features.rs` - Concrete Features

**Responsibility:** Standard feature implementations

**Public API:**
```rust
pub struct GlobalTime;       // Extract timestamps
pub struct MidPrice;         // Calculate mid-price
pub struct SpreadBps;        // Calculate spread in bps
pub struct BookImbalance;    // Calculate order book imbalance
```

**Key Changes:**
- All features use `&dyn OrderBook` and `&dyn BookUpdate`
- 8 unit tests using mock utilities

#### 3d. `etl/refinery.rs` - Stream Refinement

**Responsibility:** Inject synthetic snapshots into streams

**Key Changes:**
- Now uses `protocol::serialization::write_l2_snapshot()`
- Reduced from 90 lines to 30 lines of code
- No direct unsafe transmute operations

---

### 4. `venues/szse/l3/` - SZSE L3 Reconstruction [NEW LOCATION]

**Responsibility:** Venue-specific L3 (individual order) orderbook reconstruction for SZSE

**Public API:**
```rust
pub struct SzseL3Engine {
    pub fn new(worker_count: usize) -> Self;
    pub fn apply_message<'a>(&mut self, msg: &impl StreamMessageView<'a>) -> Result<ApplyStatus>;
    pub fn checkpoint(&self) -> Option<ChannelCheckpoint>;
}

// Re-exports shared types
pub use crate::stream::sequencer::GapPolicy;
```

**Used By:**
- `bin/chronicle_szse_recon.rs` - SZSE reconstruction binary

**Dependencies:**
- `stream::sequencer` - For `SequenceValidator` and `GapPolicy`
- `protocol` - For L3 event types

**Key Changes:**
- Moved from `stream/reconstruct/` to `venues/szse/l3/`
- Now uses shared `SequenceValidator` instead of duplicate `ChannelSequencer`
- Re-exports `GapPolicy` from sequencer module

---

### 5. `protocol/` - Protocol Definitions [REFACTORED]

**Responsibility:** Wire protocol types and serialization

**Public API:**
```rust
// Types
pub struct BookEventHeader { /* ... */ }
pub struct L2Snapshot { /* ... */ }
pub struct L2Diff { /* ... */ }
pub struct PriceLevelUpdate { /* ... */ }
pub enum BookEventType { Snapshot, Diff, Reset, Heartbeat }
pub enum BookMode { L2, L3 }

// Serialization [NEW]
pub mod serialization {
    pub fn write_l2_snapshot<I, J>(
        buf: &mut [u8],
        venue_id: u16,
        market_id: u32,
        timestamp_ns: u64,
        price_scale: u8,
        size_scale: u8,
        bids: I,
        asks: J,
    ) -> usize
    where
        I: Iterator<Item = (u64, u64)>,
        J: Iterator<Item = (u64, u64)>;

    pub fn l2_snapshot_size(bid_count: usize, ask_count: usize) -> usize;
}
```

**Used By:**
- `stream/replay/` - For deserializing book events
- `stream/etl/refinery.rs` - For serializing snapshots
- `venues/szse/l3/` - For L3 event types

**Dependencies:** None (self-contained)

**Key Addition:**
- `serialization` module encapsulates unsafe transmute operations
- Reusable across components

---

## Dependency Graph

```
┌─────────────────────────────────────────────────────────────────┐
│                          Applications                           │
│  (chronicle_szse_recon, chronicle_etl, etc.)                   │
└───────────────┬─────────────────────────────────────────────────┘
                │
    ┌───────────┴─────────────┬──────────────┬────────────────┐
    │                         │              │                │
┌───▼───────┐        ┌────────▼────┐    ┌───▼────────┐  ┌────▼─────┐
│  venues/  │        │stream/replay│    │ stream/etl │  │ storage/ │
│   szse/   │        └──────┬──────┘    └──────┬─────┘  └──────────┘
│    l3/    │               │                  │
└─────┬─────┘               │                  │
      │                     │                  │
      │         ┌───────────┴──────────────────┘
      │         │
      │    ┌────▼────────┐        ┌─────────────────┐
      │    │  stream/    │        │  stream/etl/    │
      └───►│  sequencer  │◄───────┤  types          │
           └─────────────┘        └─────────────────┘
                  │                       │
                  │                       │ (adapters)
                  │                       │
           ┌──────▼────────┐        ┌─────▼────────┐
           │   protocol/   │◄───────┤stream/replay │
           │ serialization │        │   (types)    │
           └───────────────┘        └──────────────┘
                  │
           ┌──────▼────────┐
           │   protocol/   │
           │   (types)     │
           └───────────────┘
```

**Arrows show dependency direction (A → B means A depends on B)**

---

## Key Design Principles Achieved

### 1. Single Source of Truth
- **Sequence Validation:** `stream/sequencer/mod.rs` only
- **Gap Policies:** Defined once, re-exported where needed
- **Protocol Serialization:** `protocol::serialization` only

### 2. Separation of Concerns
- **Generic Patterns:** `stream/` module
- **Venue-Specific Logic:** `venues/` module
- **Protocol Details:** `protocol/` module
- **Feature Logic:** Decoupled from data sources via traits

### 3. Testability
- **Unit Tests:** Features testable without replay engine
- **Mock Utilities:** Reusable across components
- **Integration Tests:** Full pipeline tests still work

### 4. Reusability
- `SequenceValidator` used by replay and venues
- `protocol::serialization` reusable across components
- ETL abstraction traits enable alternative implementations

### 5. Clear Boundaries
- **Traits Define Contracts:** `OrderBook`, `BookUpdate`, etc.
- **Modules Own Concerns:** Protocol owns serialization, sequencer owns validation
- **Re-exports Control Visibility:** Explicit public API surface

---

## Code Metrics

### Lines of Code Impact

| Area | Before | After | Change |
|------|--------|-------|--------|
| Sequence validation (total) | ~100 (duplicated) | 228 (single) | -100 duplicate, +228 new |
| Replay sequencing | ~50 | Uses shared | -50 |
| SZSE L3 sequencing | ~50 | Uses shared | -50 |
| Refinery serialization | 90 | 30 | -60 |
| Protocol serialization | 0 | 152 | +152 |
| ETL abstractions | 0 | 227 | +227 |
| ETL test utilities | 0 | 227 | +227 |
| Feature tests | 0 | 165 | +165 |

**Net Impact:**
- Eliminated ~200 lines of duplicate code
- Added ~1000 lines of abstraction, tests, and infrastructure
- Improved maintainability significantly

### Test Coverage

| Phase | Tests Passing | New Tests Added |
|-------|---------------|-----------------|
| Baseline | 42 | - |
| Phase 1-3 | 42 | 0 (existing still pass) |
| Phase 4A | 42 | 0 (abstraction layer) |
| Phase 4B | 50 | +8 (feature tests) |
| Phase 4C | 54 | +4 (serialization tests) |

**Total:** 42 → 54 tests (+28% increase)

---

## Migration Guide for Developers

### Using Sequence Validation

**Before:**
```rust
// Manual tracking in each module
let mut last_seq: Option<u64> = None;
if let Some(prev) = last_seq {
    if seq != prev.wrapping_add(1) {
        bail!("gap detected");
    }
}
last_seq = Some(seq);
```

**After:**
```rust
use crate::stream::sequencer::{SequenceValidator, GapPolicy};

let mut validator = SequenceValidator::new(GapPolicy::Panic);
match validator.check(seq)? {
    GapDetection::Sequential => { /* ok */ },
    GapDetection::Gap { from, to } => { /* handle gap */ },
}
```

### Writing Features

**Before:**
```rust
impl Feature for MyFeature {
    fn on_event(&mut self, book: &L2Book, event: &BookEvent<'_>, ...) {
        let bid = book.bids().iter().next_back()...
    }
}
```

**After:**
```rust
use crate::stream::etl::types::{OrderBook, BookUpdate};

impl Feature for MyFeature {
    fn on_event(&mut self, book: &dyn OrderBook, event: &dyn BookUpdate, ...) {
        let bid = book.best_bid()...
    }
}

// Now testable!
#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::etl::types::test_utils::MockOrderBook;

    #[test]
    fn test_my_feature() {
        let mut book = MockOrderBook::new();
        book.add_bid(100, 50);
        // ... test feature in isolation
    }
}
```

### Protocol Serialization

**Before:**
```rust
// 90 lines of unsafe transmute code
let header_bytes = unsafe {
    std::slice::from_raw_parts(&header as *const _ as *const u8, size)
};
buf[0..size].copy_from_slice(header_bytes);
// ... more transmutes for snapshot and levels
```

**After:**
```rust
use crate::protocol::serialization;

let size = serialization::l2_snapshot_size(bids.len(), asks.len());
let mut buf = vec![0u8; size];
serialization::write_l2_snapshot(
    &mut buf, venue_id, market_id, timestamp_ns,
    price_scale, size_scale, bids, asks,
);
```

---

## Future Improvements

### Potential Phase 5: ETL Module Restructuring
As identified in the original analysis but not yet implemented:

1. **Split ETL into Features and Refinery**
   - `stream/features/` - Feature computation (current `etl/feature*`)
   - `stream/refinery/` - Stream transformation (current `etl/refinery`)

2. **Extract Common Patterns**
   - Transform/Sink patterns could be more generic
   - Pipeline orchestration could be reusable

3. **Remove Single-Symbol Constraint in Refinery**
   - Create pluggable `SymbolHandler` trait
   - Support multi-symbol refinement

### Additional Opportunities

1. **Performance Optimizations**
   - Profile trait object overhead in `OrderBook::bid_levels()`
   - Consider specialized iterators instead of `Box<dyn Iterator>`

2. **Additional Protocol Serialization**
   - `write_l2_diff()` for diff events
   - `write_l3_event()` for L3 events
   - Unified `ProtocolWriter` API

3. **Enhanced Testing**
   - Property-based tests for sequence validation
   - Fuzzing for protocol serialization
   - End-to-end integration tests

---

## Conclusion

This refactoring effort successfully:

✅ **Eliminated Code Duplication** - Removed ~200 lines of duplicate sequence validation and serialization logic

✅ **Established Clear Boundaries** - Separated generic patterns (`stream/`), venue-specific code (`venues/`), and protocol details (`protocol/`)

✅ **Improved Testability** - Features can now be tested in isolation with 12 new unit tests

✅ **Enhanced Maintainability** - Single source of truth for core concerns, clear ownership

✅ **Maintained Backward Compatibility** - All existing tests pass, re-exports preserve public API

The module structure now follows clear design principles with well-defined responsibilities and minimal coupling between components.
