# Storage Unification Refactoring Plan

**Version:** 1.0
**Date:** 2026-01-29
**Status:** Planning
**Estimated Effort:** 4-5 weeks

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Current State Analysis](#current-state-analysis)
3. [Problem Statement](#problem-statement)
4. [Target Architecture](#target-architecture)
5. [Migration Phases](#migration-phases)
6. [Detailed File Changes](#detailed-file-changes)
7. [Backward Compatibility](#backward-compatibility)
8. [Testing Strategy](#testing-strategy)
9. [Rollback Plan](#rollback-plan)
10. [Success Criteria](#success-criteria)

---

## Executive Summary

### Goals

1. **Eliminate duplication** between `src/storage/` (IPC) and `src/core/timeseries/` (time-series)
2. **Unified compression** strategy with flexible policies
3. **Transparent hot/cold storage** with seekable zstd compression
4. **Clean architecture** with clear separation of concerns
5. **Backward compatibility** for existing IPC users

### Key Changes

- Rename `src/core/timeseries/` → `src/table/`
- Create `src/core/compression/` for unified compression layer
- Create `src/lifecycle/` for storage lifecycle management
- Refactor `src/storage/` → `src/ipc/` as thin wrapper over `Table`
- Add `CompressionPolicy` to `TableConfig`

### Timeline

- **Week 1:** Phase 1 - Compression Layer
- **Week 2:** Phase 2 - Rename & Reorganize
- **Week 3:** Phase 3 - Lifecycle Manager
- **Week 4:** Phase 4 - IPC Wrapper
- **Week 5:** Phase 5 - Testing & Documentation

---

## Current State Analysis

### Directory Structure (Before)

```
src/
├── core/
│   ├── segment/               # ✅ Segment primitives (shared)
│   │   ├── mod.rs
│   │   ├── writer.rs
│   │   ├── cursor.rs
│   │   └── store.rs
│   │
│   └── timeseries/            # 🔵 Time-series implementation
│       ├── log.rs             # TimeSeriesWriter/Reader
│       ├── table.rs           # Table
│       ├── partition.rs       # PartitionScheme
│       ├── rolling.rs         # RollingWriter
│       ├── rollers.rs         # Partition rollers
│       └── table_reader.rs    # Multi-partition reader
│
├── storage/                   # 🔴 IPC-specific (DUPLICATED)
│   ├── archive_writer.rs      # ❌ Duplicates TimeSeriesWriter
│   ├── tier.rs                # Compression tier management
│   ├── zstd.rs                # Zstd compression
│   ├── zstd_seek.rs           # Seekable zstd
│   ├── meta.rs                # Metadata
│   ├── raw_archiver.rs        # Raw archive format
│   └── access/
│       ├── reader.rs          # StorageReader
│       └── resolver.rs        # StorageResolver
│
└── layout/                    # ✅ Directory layout (shared)
    └── ...
```

### Duplication Analysis

| Functionality | storage/ | timeseries/ | Problem |
|---------------|----------|-------------|---------|
| Segment writing | `ArchiveWriter` | `TimeSeriesWriter` | Two implementations |
| Directory layout | `ArchiveLayout` (fixed) | `PartitionScheme` (flexible) | Different paradigms |
| Compression | `tier.rs` (hardcoded) | None | Not available for timeseries |
| Reading | `StorageReader` | `TimeSeriesReader` | Separate code paths |
| Rolling | Built into `ArchiveWriter` | `RollingWriter` | Duplicated logic |

### Code Metrics (Before)

```
storage/
  archive_writer.rs       224 lines
  tier.rs                 230 lines
  zstd.rs                 ~200 lines (estimated)
  zstd_seek.rs            ~300 lines (estimated)
  access/reader.rs        ~400 lines (estimated)
  Total: ~1400 lines

timeseries/
  log.rs                  ~700 lines
  table.rs                ~500 lines
  partition.rs            ~400 lines
  rolling.rs              ~300 lines
  rollers.rs              ~400 lines
  table_reader.rs         ~450 lines
  Total: ~2750 lines

Combined: ~4150 lines (with duplication)
```

---

## Problem Statement

### Issues with Current Design

1. **Duplication:**
   - `ArchiveWriter` and `TimeSeriesWriter` both write segments
   - Compression logic only in `storage/`, not available for time-series
   - Two different directory layout paradigms

2. **Inflexibility:**
   - IPC compression is hardcoded (age-based only)
   - Time-series has no compression support
   - Cannot share compression between use cases

3. **Complexity:**
   - Users must choose between two APIs
   - Maintenance burden of parallel implementations
   - Difficult to add features (benefit both paths)

4. **Performance:**
   - IPC compresses even hot data (wasteful for actively read streams)
   - No access tracking (compress data that's still being read)

---

## Target Architecture

### Directory Structure (After)

```
src/
├── core/
│   ├── segment/               # ✅ Segment primitives (unchanged)
│   │   ├── mod.rs
│   │   ├── writer.rs
│   │   ├── cursor.rs
│   │   └── store.rs
│   │
│   └── compression/           # 🆕 Unified compression layer
│       ├── mod.rs
│       ├── zstd.rs            # Moved from storage/
│       ├── zstd_seek.rs       # Moved from storage/
│       └── unified_reader.rs  # NEW: Reads .q or .q.zst transparently
│
├── table/                     # 🔄 Renamed from timeseries/
│   ├── mod.rs
│   ├── table.rs               # Enhanced with compression config
│   ├── config.rs              # NEW: TableConfig with CompressionPolicy
│   ├── metadata.rs
│   │
│   ├── partition/
│   │   ├── mod.rs
│   │   ├── scheme.rs          # PartitionScheme
│   │   ├── values.rs          # PartitionValues
│   │   ├── filter.rs          # PartitionFilter
│   │   └── info.rs            # PartitionInfo
│   │
│   ├── writer/
│   │   ├── mod.rs
│   │   ├── log_writer.rs      # TimeSeriesWriter
│   │   ├── partition_writer.rs
│   │   └── rolling_writer.rs
│   │
│   ├── reader/
│   │   ├── mod.rs
│   │   ├── log_reader.rs      # Uses UnifiedSegmentReader
│   │   └── table_reader.rs
│   │
│   └── roller/
│       ├── mod.rs
│       ├── trait.rs
│       ├── date_roller.rs
│       ├── hour_roller.rs
│       └── custom_roller.rs
│
├── lifecycle/                 # 🆕 Storage lifecycle management
│   ├── mod.rs
│   ├── policy.rs              # CompressionPolicy
│   ├── manager.rs             # StorageLifecycleManager
│   ├── access_tracker.rs      # Track segment reads
│   ├── compressor.rs          # Compression operations
│   └── stats.rs               # LifecycleStats
│
├── ipc/                       # 🔄 Refactored from storage/
│   ├── mod.rs
│   ├── archive.rs             # Archive (wrapper over Table)
│   └── layout.rs              # ArchiveLayout (compatibility)
│
└── lib.rs                     # Updated exports
```

### Key Abstractions

#### 1. UnifiedSegmentReader (NEW)

```rust
pub struct UnifiedSegmentReader {
    inner: ReaderInner,
}

enum ReaderInner {
    Hot(SegmentCursor),         // .q file (mmap)
    Cold(ZstdSeekableReader),   // .q.zst file (seekable)
}

impl UnifiedSegmentReader {
    /// Opens .q if exists, falls back to .q.zst
    pub fn open(dir: &Path, segment_id: u64) -> Result<Self>;

    /// Transparent read regardless of compression
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize>;
}
```

#### 2. CompressionPolicy (NEW)

```rust
pub enum CompressionPolicy {
    /// Never compress (always hot)
    Never,

    /// Compress after N days of no access
    /// Best for IPC: compress old/idle data only
    IdleAfter {
        days: u32,
        track_reads: bool,
    },

    /// Compress after age, regardless of reads
    /// Best for time-series: compress yesterday's data
    AgeAfter {
        days: u32,
    },

    /// Compress immediately after seal
    /// Best for archival use cases
    Immediate,

    /// Compress when size > threshold
    SizeThreshold {
        bytes: u64,
    },

    /// Custom user-defined policy
    Custom(Box<dyn Fn(&SegmentInfo) -> bool + Send + Sync>),
}
```

#### 3. TableConfig (ENHANCED)

```rust
pub struct TableConfig {
    // Existing fields
    pub segment_size: usize,
    pub index_stride: u32,
    pub validate_partition_order: bool,

    // NEW: Compression configuration
    pub compression_policy: CompressionPolicy,
    pub compression_level: i32,           // zstd level (1-22)
    pub compression_block_size: usize,    // seek block size
    pub track_access: bool,               // Enable access tracking
}

impl Default for TableConfig {
    fn default() -> Self {
        Self {
            segment_size: 64 * 1024 * 1024,
            index_stride: 100,
            validate_partition_order: true,

            // Default: compress after 7 days of no reads
            compression_policy: CompressionPolicy::IdleAfter {
                days: 7,
                track_reads: true,
            },
            compression_level: 3,
            compression_block_size: 1024 * 1024,
            track_access: true,
        }
    }
}
```

#### 4. StorageLifecycleManager (NEW)

```rust
pub struct StorageLifecycleManager {
    root: PathBuf,
    config: LifecycleConfig,
    access_tracker: AccessTracker,
}

pub struct LifecycleConfig {
    pub policy: CompressionPolicy,
    pub compression_level: i32,
    pub block_size: usize,
    pub parallel_workers: usize,
}

impl StorageLifecycleManager {
    pub fn new(root: PathBuf, config: LifecycleConfig) -> Self;

    /// Scan and compress eligible segments
    pub fn run_once(&self) -> Result<LifecycleStats>;

    /// Compress specific partition
    pub fn compress_partition(&self, partition_path: &Path) -> Result<()>;

    /// Decompress back to hot (if needed)
    pub fn decompress_segment(&self, segment_id: u64) -> Result<()>;
}
```

#### 5. Archive (REFACTORED)

```rust
/// IPC Archive - wrapper over Table for backward compatibility
pub struct Archive {
    table: Table,
    partition: PartitionValues,
    writer: Option<PartitionWriter>,
}

impl Archive {
    pub fn new(
        root: impl Into<PathBuf>,
        venue: &str,
        symbol: &str,
        date: &str,
        stream: &str,
    ) -> Result<Self> {
        // Internally creates Table with fixed scheme
        let scheme = PartitionScheme::new()
            .add_string("venue")
            .add_string("symbol")
            .add_date("date")
            .add_string("stream");

        let table = Table::open_or_create(
            root,
            scheme,
            TableConfig {
                compression_policy: CompressionPolicy::IdleAfter {
                    days: 7,
                    track_reads: true,
                },
                ..Default::default()
            },
        )?;

        // ... rest of implementation
    }

    pub fn append(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()>;
    pub fn finish(self) -> Result<()>;
}
```

---

## Migration Phases

### Phase 1: Compression Layer (Week 1)

**Goal:** Extract and unify compression code

#### Tasks

1. **Create `src/core/compression/` module**
   - Create directory structure
   - Add `mod.rs` with exports

2. **Move compression files**
   - `storage/zstd.rs` → `core/compression/zstd.rs`
   - `storage/zstd_seek.rs` → `core/compression/zstd_seek.rs`
   - Update imports across codebase

3. **Implement `UnifiedSegmentReader`**
   - Create `core/compression/unified_reader.rs`
   - Implement transparent .q/.q.zst reading
   - Add tests for both paths

4. **Update `TimeSeriesReader`**
   - Replace `SegmentCursor` with `UnifiedSegmentReader`
   - Add support for reading compressed segments

#### Files Changed

```
NEW:
  src/core/compression/mod.rs
  src/core/compression/unified_reader.rs

MOVED:
  src/storage/zstd.rs → src/core/compression/zstd.rs
  src/storage/zstd_seek.rs → src/core/compression/zstd_seek.rs

MODIFIED:
  src/core/timeseries/log.rs (use UnifiedSegmentReader)
  src/core/mod.rs (add compression module)
```

#### Tests

```rust
#[test]
fn test_unified_reader_hot_segment()
#[test]
fn test_unified_reader_cold_segment()
#[test]
fn test_unified_reader_fallback()
#[test]
fn test_timeseries_reader_with_compressed()
```

#### Deliverable

- Compression code centralized in `core/compression/`
- TimeSeriesReader can read compressed segments
- All existing tests pass

---

### Phase 2: Rename & Reorganize (Week 2)

**Goal:** Rename timeseries → table, add compression config

#### Tasks

1. **Rename module**
   - `src/core/timeseries/` → `src/table/`
   - Update all imports across codebase

2. **Reorganize into submodules**
   - `partition.rs` → `partition/` (split into scheme, values, filter, info)
   - `rolling.rs` + `rollers.rs` → `roller/` (split by roller type)
   - `log.rs` → `writer/log_writer.rs` + `reader/log_reader.rs`
   - `table.rs` → `table.rs` (stays top-level)
   - `table_reader.rs` → `reader/table_reader.rs`

3. **Create `TableConfig`**
   - Extract config from `Table` into `config.rs`
   - Add compression-related fields
   - Add `Default` implementation

4. **Update `Table`**
   - Accept `TableConfig` in constructor
   - Store config in metadata
   - Pass config to writers

#### Files Changed

```
RENAMED MODULE:
  src/core/timeseries/ → src/table/

NEW:
  src/table/config.rs
  src/table/partition/scheme.rs
  src/table/partition/values.rs
  src/table/partition/filter.rs
  src/table/partition/info.rs
  src/table/writer/mod.rs
  src/table/writer/log_writer.rs
  src/table/writer/partition_writer.rs
  src/table/writer/rolling_writer.rs
  src/table/reader/mod.rs
  src/table/reader/log_reader.rs
  src/table/reader/table_reader.rs
  src/table/roller/mod.rs
  src/table/roller/trait.rs
  src/table/roller/date_roller.rs
  src/table/roller/hour_roller.rs
  src/table/roller/custom_roller.rs

MODIFIED:
  src/table/table.rs (use TableConfig)
  src/table/metadata.rs (store config)
  src/lib.rs (update exports)
```

#### Tests

```rust
#[test]
fn test_table_config_default()
#[test]
fn test_table_config_serialization()
#[test]
fn test_table_with_custom_config()
#[test]
fn test_config_persistence()
```

#### Deliverable

- Clean module structure under `table/`
- `TableConfig` with compression fields
- All existing tests pass with new imports

---

### Phase 3: Lifecycle Manager (Week 3)

**Goal:** Implement storage lifecycle management

#### Tasks

1. **Create `src/lifecycle/` module**
   - Create directory structure
   - Add `mod.rs` with exports

2. **Implement `CompressionPolicy`**
   - Create `lifecycle/policy.rs`
   - Define all policy variants
   - Add validation logic

3. **Implement `AccessTracker`**
   - Create `lifecycle/access_tracker.rs`
   - Track segment access times
   - Persist to `.access_log` files

4. **Implement `StorageLifecycleManager`**
   - Create `lifecycle/manager.rs`
   - Scan segments
   - Apply compression policies
   - Compress eligible segments

5. **Implement compression operations**
   - Create `lifecycle/compressor.rs`
   - Atomic compression (tmp files)
   - Verification after compression
   - Cleanup on success

6. **Add stats tracking**
   - Create `lifecycle/stats.rs`
   - Track compression operations
   - Report space savings

#### Files Changed

```
NEW:
  src/lifecycle/mod.rs
  src/lifecycle/policy.rs
  src/lifecycle/manager.rs
  src/lifecycle/access_tracker.rs
  src/lifecycle/compressor.rs
  src/lifecycle/stats.rs

MODIFIED:
  src/lib.rs (export lifecycle module)
  src/table/config.rs (use CompressionPolicy)
```

#### Tests

```rust
// Policy tests
#[test]
fn test_policy_never()
#[test]
fn test_policy_idle_after()
#[test]
fn test_policy_age_after()
#[test]
fn test_policy_immediate()
#[test]
fn test_policy_custom()

// Manager tests
#[test]
fn test_manager_scan_segments()
#[test]
fn test_manager_compress_eligible()
#[test]
fn test_manager_skip_ineligible()
#[test]
fn test_manager_parallel_compression()

// Access tracking tests
#[test]
fn test_access_tracker_record()
#[test]
fn test_access_tracker_persist()
#[test]
fn test_access_tracker_load()

// Integration tests
#[test]
fn test_end_to_end_compression()
#[test]
fn test_read_after_compression()
#[test]
fn test_compression_idempotent()
```

#### Deliverable

- Fully functional `StorageLifecycleManager`
- Multiple compression policies supported
- Access tracking works
- Compression preserves data integrity

---

### Phase 4: IPC Wrapper (Week 4)

**Goal:** Refactor IPC to use Table internally

#### Tasks

1. **Create `src/ipc/` module**
   - Create directory structure
   - Add `mod.rs` with exports

2. **Implement `Archive` wrapper**
   - Create `ipc/archive.rs`
   - Implement as wrapper over `Table`
   - Fixed partition scheme (venue/symbol/date/stream)
   - Backward-compatible API

3. **Move `ArchiveLayout`**
   - `layout/mod.rs` → `ipc/layout.rs`
   - Keep for compatibility

4. **Deprecate old storage module**
   - Mark `storage/archive_writer.rs` as deprecated
   - Add migration guide in docs
   - Keep temporarily for backward compatibility

5. **Update `StorageReader`**
   - Refactor to use `Table::reader`
   - Support reading old archives

#### Files Changed

```
NEW:
  src/ipc/mod.rs
  src/ipc/archive.rs

MOVED:
  src/layout/mod.rs → src/ipc/layout.rs (copy, mark original deprecated)

DEPRECATED (but kept):
  src/storage/archive_writer.rs
  src/storage/tier.rs

MODIFIED:
  src/storage/access/reader.rs (use Table internally)
  src/lib.rs (export ipc module, deprecate storage)
```

#### Tests

```rust
#[test]
fn test_archive_backward_compatible()
#[test]
fn test_archive_creates_correct_partition()
#[test]
fn test_archive_append()
#[test]
fn test_archive_with_compression()
#[test]
fn test_read_old_archive_format()
```

#### Deliverable

- IPC `Archive` API works using `Table` internally
- Backward compatible with existing code
- Old archives readable
- Migration path documented

---

### Phase 5: Testing & Documentation (Week 5)

**Goal:** Comprehensive testing and documentation

#### Tasks

1. **Integration tests**
   - Create `tests/storage_integration.rs`
   - Test all compression policies
   - Test IPC compatibility
   - Test time-series use cases

2. **Examples**
   - Create `examples/compression_policies.rs`
   - Create `examples/lifecycle_manager.rs`
   - Create `examples/ipc_migration.rs`
   - Update existing examples

3. **Benchmarks**
   - Create `benches/compression_overhead.rs`
   - Measure compression impact on write/read
   - Compare hot vs cold read performance

4. **Documentation**
   - Write migration guide
   - Update README
   - Add rustdoc comments
   - Create architecture diagram

5. **Cleanup**
   - Remove deprecated code (after migration period)
   - Final code review
   - Update CHANGELOG

#### Files Changed

```
NEW:
  tests/storage_integration.rs
  examples/compression_policies.rs
  examples/lifecycle_manager.rs
  examples/ipc_migration.rs
  benches/compression_overhead.rs
  docs/MIGRATION_GUIDE.md
  docs/ARCHITECTURE.md

MODIFIED:
  README.md
  CHANGELOG.md
  All source files (add rustdoc)

REMOVED (after migration period):
  src/storage/archive_writer.rs
  src/storage/tier.rs
```

#### Deliverable

- Comprehensive test coverage
- Performance benchmarks
- Migration guide
- Architecture documentation

---

## Detailed File Changes

### Phase 1 File Structure

```
Before:
src/
├── core/
│   └── timeseries/
│       └── log.rs (uses SegmentCursor)
└── storage/
    ├── zstd.rs
    └── zstd_seek.rs

After Phase 1:
src/
├── core/
│   ├── compression/
│   │   ├── mod.rs
│   │   ├── zstd.rs (MOVED)
│   │   ├── zstd_seek.rs (MOVED)
│   │   └── unified_reader.rs (NEW)
│   └── timeseries/
│       └── log.rs (uses UnifiedSegmentReader)
└── storage/
    └── (zstd files removed)
```

### Phase 2 File Structure

```
Before Phase 2:
src/core/timeseries/
├── log.rs
├── table.rs
├── partition.rs
├── rolling.rs
├── rollers.rs
├── table_reader.rs
└── metadata.rs

After Phase 2:
src/table/
├── mod.rs
├── table.rs
├── config.rs (NEW)
├── metadata.rs
├── partition/
│   ├── mod.rs
│   ├── scheme.rs (split from partition.rs)
│   ├── values.rs (split from partition.rs)
│   ├── filter.rs (split from partition.rs)
│   └── info.rs (split from partition.rs)
├── writer/
│   ├── mod.rs
│   ├── log_writer.rs (split from log.rs)
│   ├── partition_writer.rs (extracted)
│   └── rolling_writer.rs (from rolling.rs)
├── reader/
│   ├── mod.rs
│   ├── log_reader.rs (split from log.rs)
│   └── table_reader.rs (from table_reader.rs)
└── roller/
    ├── mod.rs
    ├── trait.rs (extracted)
    ├── date_roller.rs (split from rollers.rs)
    ├── hour_roller.rs (split from rollers.rs)
    └── custom_roller.rs (split from rollers.rs)
```

### Final File Structure (After All Phases)

```
src/
├── core/
│   ├── error.rs
│   ├── header.rs
│   ├── mmap.rs
│   ├── segment/
│   │   ├── mod.rs
│   │   ├── writer.rs
│   │   ├── cursor.rs
│   │   └── store.rs
│   └── compression/
│       ├── mod.rs
│       ├── zstd.rs
│       ├── zstd_seek.rs
│       └── unified_reader.rs
│
├── table/
│   ├── mod.rs
│   ├── table.rs
│   ├── config.rs
│   ├── metadata.rs
│   ├── partition/
│   │   ├── mod.rs
│   │   ├── scheme.rs
│   │   ├── values.rs
│   │   ├── filter.rs
│   │   └── info.rs
│   ├── writer/
│   │   ├── mod.rs
│   │   ├── log_writer.rs
│   │   ├── partition_writer.rs
│   │   └── rolling_writer.rs
│   ├── reader/
│   │   ├── mod.rs
│   │   ├── log_reader.rs
│   │   └── table_reader.rs
│   └── roller/
│       ├── mod.rs
│       ├── trait.rs
│       ├── date_roller.rs
│       ├── hour_roller.rs
│       └── custom_roller.rs
│
├── lifecycle/
│   ├── mod.rs
│   ├── policy.rs
│   ├── manager.rs
│   ├── access_tracker.rs
│   ├── compressor.rs
│   └── stats.rs
│
├── ipc/
│   ├── mod.rs
│   ├── archive.rs
│   └── layout.rs
│
└── lib.rs
```

---

## Backward Compatibility

### API Compatibility Matrix

| Old API | Status | New API | Migration |
|---------|--------|---------|-----------|
| `storage::ArchiveWriter` | Deprecated → Removed | `ipc::Archive` | Drop-in replacement |
| `storage::TierManager` | Deprecated → Removed | `lifecycle::StorageLifecycleManager` | Config change needed |
| `timeseries::Table` | Moved | `table::Table` | Import path change |
| `timeseries::RollingWriter` | Moved | `table::RollingWriter` | Import path change |

### Migration Timeline

1. **Phase 1-4 (Weeks 1-4):**
   - Old APIs marked `#[deprecated]`
   - Deprecation warnings in documentation
   - Both old and new APIs work

2. **Phase 5 (Week 5):**
   - Migration guide published
   - Examples updated
   - Deprecation warnings in compiler

3. **Post-release (1 month):**
   - Remove deprecated code
   - Clean up old tests
   - Archive old examples

### Breaking Changes

```rust
// BREAKING: Module moved
// Before:
use chronicle::core::timeseries::Table;

// After:
use chronicle::table::Table;

// BREAKING: Config structure changed
// Before:
Table::create(path, scheme)?;

// After:
Table::create(path, scheme, TableConfig::default())?;
// Or use builder:
Table::create(path, scheme, TableConfig {
    compression_policy: CompressionPolicy::Never,
    ..Default::default()
})?;

// BREAKING: ArchiveWriter removed
// Before:
use chronicle::storage::ArchiveWriter;
let mut writer = ArchiveWriter::new(root, venue, symbol, date, stream, size)?;

// After:
use chronicle::ipc::Archive;
let mut archive = Archive::new(root, venue, symbol, date, stream)?;
```

---

## Testing Strategy

### Unit Tests (Per Module)

```
core/compression/
  ✓ test_zstd_compress_decompress
  ✓ test_zstd_seek_roundtrip
  ✓ test_unified_reader_hot
  ✓ test_unified_reader_cold
  ✓ test_unified_reader_fallback

table/partition/
  ✓ test_partition_scheme_builder
  ✓ test_partition_values_to_path
  ✓ test_partition_filter_exact
  ✓ test_partition_filter_range

table/writer/
  ✓ test_log_writer_append
  ✓ test_rolling_writer_auto_partition
  ✓ test_partition_writer_wrapper

table/reader/
  ✓ test_log_reader_with_compression
  ✓ test_table_reader_merge
  ✓ test_table_reader_sequential

lifecycle/
  ✓ test_policy_evaluation
  ✓ test_manager_scan
  ✓ test_manager_compress
  ✓ test_access_tracker
  ✓ test_compressor_atomic

ipc/
  ✓ test_archive_backward_compat
  ✓ test_archive_append
  ✓ test_archive_read_old_format
```

### Integration Tests

```rust
// tests/storage_integration.rs

#[test]
fn test_end_to_end_compression_lifecycle() {
    // 1. Create table
    // 2. Write data
    // 3. Run lifecycle manager
    // 4. Verify compression
    // 5. Read compressed data
    // 6. Verify correctness
}

#[test]
fn test_ipc_migration() {
    // 1. Create archive with old format
    // 2. Write data
    // 3. Read using new IPC API
    // 4. Verify data matches
}

#[test]
fn test_multi_policy_table() {
    // 1. Create tables with different policies
    // 2. Write data
    // 3. Run lifecycle manager
    // 4. Verify each policy applied correctly
}

#[test]
fn test_concurrent_read_write_compression() {
    // 1. Spawn writer thread
    // 2. Spawn reader threads
    // 3. Spawn compression thread
    // 4. Verify no corruption
}
```

### Performance Benchmarks

```rust
// benches/compression_overhead.rs

fn bench_write_uncompressed(c: &mut Criterion) {
    // Baseline: write without compression
}

fn bench_write_with_compression(c: &mut Criterion) {
    // Measure overhead of immediate compression
}

fn bench_read_hot_segment(c: &mut Criterion) {
    // Baseline: read uncompressed
}

fn bench_read_cold_segment(c: &mut Criterion) {
    // Measure zstd seek performance
}

fn bench_lifecycle_scan(c: &mut Criterion) {
    // Measure time to scan and identify candidates
}

fn bench_compression_operation(c: &mut Criterion) {
    // Measure time to compress one segment
}
```

### Test Coverage Goals

- **Unit tests:** >90% coverage
- **Integration tests:** All major user workflows
- **Benchmarks:** No regression vs. baseline
- **Compatibility:** 100% backward compatibility during migration

---

## Rollback Plan

### If Issues Found in Phase 1

**Symptoms:**
- `UnifiedSegmentReader` has bugs
- Compressed reads fail

**Rollback:**
1. Revert `log.rs` to use `SegmentCursor` directly
2. Keep `core/compression/` but don't use yet
3. Fix bugs before proceeding

**Risk:** Low - compression is new, no existing users

---

### If Issues Found in Phase 2

**Symptoms:**
- Import paths broken
- Module organization causes issues

**Rollback:**
1. Revert module rename
2. Keep `timeseries/` name
3. Fix organizational issues
4. Retry rename

**Risk:** Medium - affects all users

---

### If Issues Found in Phase 3

**Symptoms:**
- Lifecycle manager corrupts data
- Compression policy bugs

**Rollback:**
1. Keep lifecycle module but don't enable by default
2. Mark as experimental
3. Fix bugs
4. Re-enable after validation

**Risk:** Low - opt-in feature

---

### If Issues Found in Phase 4

**Symptoms:**
- IPC wrapper has regressions
- Old archives unreadable

**Rollback:**
1. Revert IPC changes
2. Keep old `storage/archive_writer.rs`
3. Extend migration period
4. Fix compatibility issues

**Risk:** High - affects existing IPC users

**Mitigation:**
- Extensive testing before release
- Beta period with IPC users
- Keep old code path for 1 release cycle

---

## Success Criteria

### Functional Requirements

- [ ] All existing tests pass
- [ ] New compression features work
- [ ] IPC backward compatibility maintained
- [ ] Time-series can use compression
- [ ] Lifecycle manager compresses correctly
- [ ] Transparent hot/cold reads work

### Performance Requirements

- [ ] Write performance: No regression
- [ ] Hot read performance: No regression
- [ ] Cold read performance: <2x slower than hot
- [ ] Compression ratio: >50% for typical data
- [ ] Lifecycle scan: <1 sec per 1000 segments

### Code Quality Requirements

- [ ] No duplication between IPC and time-series
- [ ] Clean module boundaries
- [ ] Comprehensive documentation
- [ ] >90% test coverage
- [ ] All public APIs documented

### User Experience Requirements

- [ ] Migration guide published
- [ ] Examples updated
- [ ] Deprecation warnings clear
- [ ] Error messages helpful
- [ ] Zero-downtime migration possible

---

## Risk Assessment

### High Risk Items

1. **Phase 4: IPC Backward Compatibility**
   - Risk: Breaking existing IPC users
   - Mitigation: Extensive testing, beta period, keep old code temporarily
   - Rollback: Revert IPC changes, extend migration

2. **Phase 3: Data Corruption in Compression**
   - Risk: Compressed data unreadable or corrupted
   - Mitigation: Atomic operations, verification, extensive testing
   - Rollback: Disable lifecycle manager, fix bugs

### Medium Risk Items

3. **Phase 2: Module Reorganization**
   - Risk: Breaking imports for all users
   - Mitigation: Deprecation warnings, migration guide
   - Rollback: Revert rename, keep old structure

4. **Performance Regression**
   - Risk: Compression overhead slows down writes
   - Mitigation: Benchmarks, make compression opt-in
   - Rollback: Default to `CompressionPolicy::Never`

### Low Risk Items

5. **Phase 1: Compression Layer**
   - Risk: New code has bugs
   - Mitigation: Not used by default, extensive tests
   - Rollback: Don't integrate with readers yet

---

## Dependencies

### External Dependencies

No new external dependencies required. Existing:
- `zstd` crate (already used)
- `serde` for config serialization
- `anyhow` for errors

### Internal Dependencies

- Phase 2 depends on Phase 1 (needs `UnifiedSegmentReader`)
- Phase 3 depends on Phase 2 (needs `TableConfig`)
- Phase 4 depends on Phases 1-3 (needs full stack)
- Phase 5 depends on all (needs complete system)

### Critical Path

```
Phase 1 → Phase 2 → Phase 3 → Phase 4 → Phase 5
(Can't parallelize, must be sequential)
```

---

## Stakeholder Communication

### Before Starting

- [ ] Review plan with team
- [ ] Get approval for breaking changes
- [ ] Notify existing IPC users
- [ ] Set expectations for migration

### During Development

- [ ] Weekly progress updates
- [ ] Demo each phase when complete
- [ ] Request feedback on APIs
- [ ] Share benchmarks

### After Completion

- [ ] Publish migration guide
- [ ] Update documentation
- [ ] Announce new features
- [ ] Provide migration support

---

## Appendix

### A. Compression Policy Examples

```rust
// IPC: Compress old idle data
TableConfig {
    compression_policy: CompressionPolicy::IdleAfter {
        days: 7,
        track_reads: true,
    },
    ..Default::default()
}

// Time-series archival: Compress yesterday
TableConfig {
    compression_policy: CompressionPolicy::AgeAfter {
        days: 1,
    },
    ..Default::default()
}

// Hot analytics: Never compress
TableConfig {
    compression_policy: CompressionPolicy::Never,
    ..Default::default()
}

// Large segments only
TableConfig {
    compression_policy: CompressionPolicy::SizeThreshold {
        bytes: 100 * 1024 * 1024,  // 100MB
    },
    ..Default::default()
}

// Custom: Compress weekends
TableConfig {
    compression_policy: CompressionPolicy::Custom(Box::new(|info| {
        let weekday = info.created_at.weekday();
        weekday == Weekday::Sat || weekday == Weekday::Sun
    })),
    ..Default::default()
}
```

### B. File Size Estimates

```
Phase 1:
  core/compression/unified_reader.rs    ~300 lines
  core/compression/mod.rs               ~50 lines
  Total: ~350 lines new

Phase 2:
  Reorganization only, minimal new code
  table/config.rs                       ~150 lines
  Total: ~150 lines new

Phase 3:
  lifecycle/policy.rs                   ~200 lines
  lifecycle/manager.rs                  ~400 lines
  lifecycle/access_tracker.rs           ~150 lines
  lifecycle/compressor.rs               ~200 lines
  lifecycle/stats.rs                    ~100 lines
  Total: ~1050 lines new

Phase 4:
  ipc/archive.rs                        ~300 lines
  ipc/layout.rs                         ~100 lines
  Total: ~400 lines new

Phase 5:
  Tests, examples, docs                 ~1000 lines

Grand Total New Code: ~2950 lines
Code Removed (duplication): ~1400 lines
Net Change: +1550 lines
```

### C. Timeline Details

```
Week 1 (Phase 1):
  Day 1-2: Create compression module, move files
  Day 3-4: Implement UnifiedSegmentReader
  Day 5: Update TimeSeriesReader, test

Week 2 (Phase 2):
  Day 1-2: Rename and reorganize modules
  Day 3-4: Create TableConfig, update Table
  Day 5: Update all imports, test

Week 3 (Phase 3):
  Day 1-2: Implement CompressionPolicy and AccessTracker
  Day 3-4: Implement StorageLifecycleManager
  Day 5: Integration testing

Week 4 (Phase 4):
  Day 1-2: Implement Archive wrapper
  Day 3-4: Update StorageReader for compatibility
  Day 5: Backward compatibility testing

Week 5 (Phase 5):
  Day 1-2: Integration tests and benchmarks
  Day 3-4: Examples and documentation
  Day 5: Final review and cleanup
```

---

**End of Plan**

**Next Steps:**
1. Review this plan with team
2. Get approval for breaking changes
3. Create GitHub issues for each phase
4. Begin Phase 1 implementation
