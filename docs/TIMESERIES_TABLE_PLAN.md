# TimeSeriesTable Implementation Plan

**Version:** 1.0
**Date:** 2026-01-29
**Target:** Chronicle-RS Time-Series Storage Layer
**Estimated Effort:** ~2500 lines core code, 3-4 weeks

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Core Concepts](#core-concepts)
3. [Directory Structure](#directory-structure)
4. [API Specifications](#api-specifications)
5. [Implementation Phases](#implementation-phases)
6. [File Structure](#file-structure)
7. [Detailed Component Design](#detailed-component-design)
8. [Error Handling](#error-handling)
9. [Testing Strategy](#testing-strategy)
10. [Examples](#examples)
11. [Performance Considerations](#performance-considerations)
12. [Future Extensions](#future-extensions)

---

## Architecture Overview

### System Layers

```
┌─────────────────────────────────────────────────┐
│  Application Layer (User Code)                  │
├─────────────────────────────────────────────────┤
│  Table API                                      │
│  ├─ Table (create/open)                        │
│  ├─ PartitionWriter (single partition)         │
│  ├─ RollingWriter (auto partition)             │
│  └─ TableReader (multi-partition query)        │
├─────────────────────────────────────────────────┤
│  Partition Management                           │
│  ├─ PartitionScheme (define keys)              │
│  ├─ PartitionValues (key=value pairs)          │
│  └─ PartitionRoller (auto-detection)           │
├─────────────────────────────────────────────────┤
│  TimeSeriesLog (Existing - Phase 8)            │
│  ├─ TimeSeriesWriter                           │
│  └─ TimeSeriesReader                           │
├─────────────────────────────────────────────────┤
│  Segment Primitives (Existing - Phase 1-3)     │
│  ├─ SegmentWriter                              │
│  ├─ SegmentCursor                              │
│  └─ SegmentStore                               │
└─────────────────────────────────────────────────┘
```

### Data Flow

**Write Path:**
```
User Message
    ↓
RollingWriter.append(ts, payload)
    ↓
PartitionRoller.partition_for(ts, payload)
    ↓ (if partition changed)
RollingWriter.roll_to_partition(new_partition)
    ↓
TimeSeriesWriter.append(ts, payload)
    ↓
SegmentWriter (existing)
    ↓
Disk: partition_dir/000000000.q
```

**Read Path:**
```
User Query
    ↓
TableReader.query(filter)
    ↓
Discover matching partitions
    ↓
Open TimeSeriesReader per partition
    ↓
Merge by timestamp (min-heap)
    ↓
Return messages in order
```

---

## Core Concepts

### 1. Table
- **Self-contained directory** with metadata (Delta Lake style)
- Defines partition scheme
- No parent catalog needed
- One table = one logical time-series collection

### 2. Partition
- **Physical storage unit** = one TimeSeriesLog
- Identified by key-value pairs (e.g., channel=101, date=2026-01-29)
- Directory path: `{table_root}/{key1}={value1}/{key2}={value2}/`
- Contains segments: `000000000.q`, `000000000.idx`, etc.

### 3. Partition Scheme
- Defines partition keys for a table
- Keys can be: String, Int, Date, Hour, Custom
- Immutable after table creation (schema evolution is future work)

### 4. Rolling Writer
- **Automatically switches partitions** based on message content
- Primary use case: date-based rolling for daily partitions
- User writes continuously, writer handles partition boundaries

---

## Directory Structure

### On-Disk Layout

```
l3_szse/                                    ← Table root
├── _table/                                 ← Metadata directory
│   ├── metadata.json                       ← Table metadata
│   └── .version                            ← Version tracking
│
├── channel=101/                            ← Partition dimension 1
│   ├── date=2026-01-29/                    ← Partition dimension 2
│   │   ├── 000000000.q                     ← TimeSeriesLog segment
│   │   ├── 000000000.idx                   ← Seek index
│   │   ├── 000000001.q
│   │   ├── 000000001.idx
│   │   └── .partition_info                 ← Partition metadata (optional)
│   │
│   ├── date=2026-01-30/
│   │   ├── 000000000.q
│   │   └── 000000000.idx
│   │
│   └── date=2026-01-31/
│       ├── 000000000.q
│       └── 000000000.idx
│
├── channel=102/
│   ├── date=2026-01-29/
│   │   ├── 000000000.q
│   │   └── 000000000.idx
│   └── date=2026-01-30/
│       ├── 000000000.q
│       └── 000000000.idx
│
└── channel=103/
    └── date=2026-01-29/
        ├── 000000000.q
        └── 000000000.idx
```

### Metadata Format

**`_table/metadata.json`:**
```json
{
  "version": 1,
  "created_at": 1706486400000000000,
  "updated_at": 1706572800000000000,
  "scheme": {
    "keys": [
      {
        "type": "String",
        "name": "channel"
      },
      {
        "type": "Date",
        "name": "date",
        "format": "%Y-%m-%d",
        "timezone": "Asia/Shanghai"
      }
    ]
  },
  "properties": {
    "description": "L3 SZSE market data",
    "source": "szse_feed",
    "owner": "trading_team"
  },
  "statistics": {
    "partition_count": 150,
    "total_messages_estimate": 1500000000,
    "size_bytes_estimate": 150000000000
  }
}
```

**`{partition}/.partition_info` (optional):**
```json
{
  "created_at": 1706486400000000000,
  "first_timestamp_ns": 1706486400000000000,
  "last_timestamp_ns": 1706572799999999999,
  "message_count": 10000000,
  "size_bytes": 1000000000
}
```

---

## API Specifications

### Module Structure

```rust
pub mod timeseries {
    mod table;           // Core Table type
    mod partition;       // Partition types
    mod rolling;         // RollingWriter
    mod rollers;         // Built-in rollers
    mod table_reader;    // Multi-partition reading
    mod metadata;        // Serialization

    pub use table::{Table, TableConfig};
    pub use partition::{
        PartitionScheme, PartitionKey, PartitionValues,
        PartitionInfo, PartitionFilter
    };
    pub use rolling::{RollingWriter, RollingStats};
    pub use rollers::{
        PartitionRoller, DateRoller, HourRoller, MinuteRoller,
        CustomRoller, CompositeRoller, PartitionExtractor
    };
    pub use table_reader::{TableReader, TableMessage, MergeStrategy};
}
```

### 1. Table

```rust
/// Self-contained partitioned time-series table
pub struct Table {
    root: PathBuf,
    metadata: TableMetadata,
    scheme: PartitionScheme,
    config: TableConfig,
}

pub struct TableConfig {
    /// Default segment size for partitions
    pub segment_size: usize,

    /// Default index stride for partitions
    pub index_stride: u32,

    /// Validate partition order (prevent backwards rolling)
    pub validate_partition_order: bool,

    /// Allow concurrent writes to same partition
    pub allow_concurrent_writes: bool,
}

impl Table {
    /// Create new table with partition scheme
    pub fn create(path: impl AsRef<Path>, scheme: PartitionScheme) -> Result<Self>

    /// Create with custom config
    pub fn create_with_config(
        path: impl AsRef<Path>,
        scheme: PartitionScheme,
        config: TableConfig,
    ) -> Result<Self>

    /// Open existing table
    pub fn open(path: impl AsRef<Path>) -> Result<Self>

    /// Get writer for specific partition
    pub fn writer(&self, partition: PartitionValues) -> Result<PartitionWriter>

    /// Create rolling writer (auto partition management)
    pub fn rolling_writer(
        &self,
        roller: impl PartitionRoller + 'static
    ) -> Result<RollingWriter>

    /// Convenient date-based rolling writer
    pub fn rolling_writer_by_date(
        &self,
        date_key: &str,
        static_values: PartitionValues,
    ) -> Result<RollingWriter>

    /// Read specific partition
    pub fn reader(&self, partition: PartitionValues) -> Result<TimeSeriesReader>

    /// Query multiple partitions with filter
    pub fn query(&self, filter: PartitionFilter) -> Result<TableReader>

    /// List all partitions
    pub fn partitions(&self) -> Result<Vec<PartitionInfo>>

    /// List partitions matching filter
    pub fn partitions_filtered(&self, filter: PartitionFilter) -> Result<Vec<PartitionInfo>>

    /// Get table metadata
    pub fn metadata(&self) -> &TableMetadata

    /// Get partition scheme
    pub fn scheme(&self) -> &PartitionScheme

    /// Get table root path
    pub fn root(&self) -> &Path
}

// Allow cheap cloning (Arc internally)
impl Clone for Table
```

### 2. PartitionScheme

```rust
/// Defines partition keys for a table
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionScheme {
    keys: Vec<PartitionKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PartitionKey {
    /// String partition key (e.g., symbol, channel, venue)
    String {
        name: String,
    },

    /// Integer partition key (e.g., user_id, channel_id)
    Int {
        name: String,
    },

    /// Date partition key (extracted from timestamp)
    Date {
        name: String,
        #[serde(default)]
        format: String,  // Default: "%Y-%m-%d"
        #[serde(default)]
        timezone: Option<String>,  // "UTC", "Asia/Shanghai", etc.
    },

    /// Hour partition key (extracted from timestamp)
    Hour {
        name: String,
        #[serde(default)]
        timezone: Option<String>,
    },

    /// Minute partition key (for very high frequency)
    Minute {
        name: String,
        #[serde(default)]
        timezone: Option<String>,
    },
}

impl PartitionScheme {
    /// Create empty scheme
    pub fn new() -> Self

    /// Add string partition key
    pub fn add_string(mut self, name: &str) -> Self

    /// Add integer partition key
    pub fn add_int(mut self, name: &str) -> Self

    /// Add date partition key
    pub fn add_date(mut self, name: &str) -> Self

    /// Add date with timezone
    pub fn add_date_with_tz(mut self, name: &str, timezone: &str) -> Self

    /// Add hour partition key
    pub fn add_hour(mut self, name: &str) -> Self

    /// Add minute partition key
    pub fn add_minute(mut self, name: &str) -> Self

    /// Get partition keys
    pub fn keys(&self) -> &[PartitionKey]

    /// Validate partition values against scheme
    pub fn validate(&self, values: &PartitionValues) -> Result<()>

    /// Get partition directory path
    pub fn partition_path(&self, root: &Path, values: &PartitionValues) -> PathBuf
}
```

### 3. PartitionValues

```rust
/// Key-value pairs identifying a partition
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PartitionValues {
    values: BTreeMap<String, String>,  // BTreeMap for deterministic order
}

impl PartitionValues {
    /// Create empty values
    pub fn new() -> Self

    /// Set partition value
    pub fn set(mut self, key: &str, value: &str) -> Self

    /// Get partition value
    pub fn get(&self, key: &str) -> Option<&str>

    /// Get all values
    pub fn values(&self) -> &BTreeMap<String, String>

    /// Convert to directory path following Hive partitioning
    /// Returns: "key1=value1/key2=value2/..."
    pub fn to_path(&self, scheme: &PartitionScheme) -> PathBuf

    /// Parse from directory path
    /// Input: "channel=101/date=2026-01-29"
    pub fn from_path(path: &Path) -> Result<Self>
}
```

### 4. PartitionWriter

```rust
/// Writer for a single partition (thin wrapper around TimeSeriesWriter)
pub struct PartitionWriter {
    partition: PartitionValues,
    writer: TimeSeriesWriter,
    table_root: PathBuf,
}

impl PartitionWriter {
    /// Append message to this partition
    pub fn append(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()>

    /// Flush pending writes
    pub fn flush(&mut self) -> Result<()>

    /// Finish and close partition
    pub fn finish(self) -> Result<PartitionStats>

    /// Get partition values
    pub fn partition(&self) -> &PartitionValues

    /// Get current sequence number
    pub fn seq(&self) -> u64

    /// Get number of segments written
    pub fn segments_written(&self) -> u64
}

pub struct PartitionStats {
    pub partition: PartitionValues,
    pub messages_written: u64,
    pub segments_written: u64,
    pub bytes_written: u64,
}
```

### 5. RollingWriter

```rust
/// Writer that automatically rolls to new partitions
pub struct RollingWriter {
    table: Table,
    roller: Box<dyn PartitionRoller>,
    current_partition: Option<PartitionValues>,
    current_writer: Option<TimeSeriesWriter>,
    stats: RollingStats,
    on_roll_callback: Option<Box<dyn Fn(&PartitionValues, &RollingStats) + Send>>,
}

impl RollingWriter {
    /// Append message - auto-rolls if partition changes
    pub fn append(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()>

    /// Flush current partition
    pub fn flush(&mut self) -> Result<()>

    /// Finish writing and close current partition
    pub fn finish(self) -> Result<RollingStats>

    /// Set callback for partition roll events
    pub fn on_partition_roll<F>(mut self, callback: F) -> Self
    where
        F: Fn(&PartitionValues, &RollingStats) + Send + 'static

    /// Get current partition
    pub fn current_partition(&self) -> Option<&PartitionValues>

    /// Get current statistics
    pub fn stats(&self) -> &RollingStats
}

#[derive(Debug, Clone, Default)]
pub struct RollingStats {
    pub messages_written: u64,
    pub partitions_created: usize,
    pub partition_rolls: usize,
    pub bytes_written: u64,
}
```

### 6. PartitionRoller

```rust
/// Strategy for determining partition from message
pub trait PartitionRoller: Send {
    /// Extract partition values from message
    fn partition_for(&self, timestamp_ns: u64, payload: &[u8]) -> Result<PartitionValues>;
}

/// Roll by date extracted from timestamp
pub struct DateRoller {
    scheme: PartitionScheme,
    date_key: String,
    timezone: Timezone,
    static_values: PartitionValues,
}

impl DateRoller {
    pub fn new(scheme: PartitionScheme, date_key: &str) -> Self
    pub fn timezone(mut self, tz: Timezone) -> Self
    pub fn with_static(mut self, key: &str, value: &str) -> Self
    pub fn with_static_values(mut self, values: PartitionValues) -> Self
}

/// Roll by hour
pub struct HourRoller {
    scheme: PartitionScheme,
    date_key: String,
    hour_key: String,
    timezone: Timezone,
    static_values: PartitionValues,
}

impl HourRoller {
    pub fn new(scheme: PartitionScheme, date_key: &str, hour_key: &str) -> Self
    pub fn timezone(mut self, tz: Timezone) -> Self
    pub fn with_static_values(mut self, values: PartitionValues) -> Self
}

/// Roll by minute
pub struct MinuteRoller {
    scheme: PartitionScheme,
    date_key: String,
    hour_key: String,
    minute_key: String,
    timezone: Timezone,
    static_values: PartitionValues,
}

/// Custom partition roller with user-provided extractor
pub struct CustomRoller {
    scheme: PartitionScheme,
    extractor: Box<dyn Fn(u64, &[u8]) -> Result<PartitionValues> + Send>,
}

impl CustomRoller {
    pub fn new<F>(scheme: PartitionScheme, extractor: F) -> Self
    where
        F: Fn(u64, &[u8]) -> Result<PartitionValues> + Send + 'static
}

/// Timezone for date/hour/minute extraction
#[derive(Debug, Clone, Copy)]
pub enum Timezone {
    UTC,
    AsiaShanghai,    // UTC+8
    AsiaTokyo,       // UTC+9
    AmericaNewYork,  // UTC-5/-4 (DST)
    Custom(i32),     // Custom offset in seconds
}
```

### 7. TableReader

```rust
/// Reader for querying multiple partitions
pub struct TableReader {
    table: Table,
    partitions: Vec<PartitionInfo>,
    readers: Vec<(PartitionValues, TimeSeriesReader)>,
    merge_strategy: MergeStrategy,
    heap: Option<BinaryHeap<PeekedMessage>>,
}

pub enum MergeStrategy {
    /// Strictly timestamp-ordered (uses min-heap)
    TimestampOrdered,

    /// Sequential partition reading (faster, but not globally ordered)
    PartitionSequential,
}

impl TableReader {
    /// Read next message
    pub fn next(&mut self) -> Result<Option<TableMessage<'_>>>

    /// Get list of partitions being read
    pub fn partitions(&self) -> &[PartitionInfo]

    /// Get merge strategy
    pub fn strategy(&self) -> MergeStrategy
}

pub struct TableMessage<'a> {
    pub partition: &'a PartitionValues,
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub payload: &'a [u8],
}
```

### 8. PartitionFilter & PartitionInfo

```rust
/// Filter for selecting partitions
#[derive(Debug, Clone, Default)]
pub struct PartitionFilter {
    filters: HashMap<String, PartitionValueFilter>,
}

pub enum PartitionValueFilter {
    /// Exact match
    Exact(String),

    /// Match any of these values
    In(Vec<String>),

    /// Range for dates/numbers
    Range { start: Option<String>, end: Option<String> },

    /// Regex pattern
    Pattern(String),
}

impl PartitionFilter {
    pub fn new() -> Self

    /// Add exact match filter
    pub fn exact(mut self, key: &str, value: &str) -> Self

    /// Add "in" filter
    pub fn in_values(mut self, key: &str, values: Vec<String>) -> Self

    /// Add range filter (inclusive start, exclusive end)
    pub fn range(mut self, key: &str, start: Option<&str>, end: Option<&str>) -> Self

    /// Check if partition matches filter
    pub fn matches(&self, partition: &PartitionValues) -> bool
}

/// Information about a partition
#[derive(Debug, Clone)]
pub struct PartitionInfo {
    pub values: PartitionValues,
    pub path: PathBuf,
    pub created_at: Option<u64>,
    pub message_count: Option<u64>,
    pub size_bytes: Option<u64>,
    pub first_timestamp_ns: Option<u64>,
    pub last_timestamp_ns: Option<u64>,
}
```

---

## Implementation Phases

### Phase 1: Core Table Infrastructure (Week 1)

**Goal:** Basic table create/open with partition scheme

**Files:**
- `src/core/timeseries/table.rs` (~400 lines)
- `src/core/timeseries/partition.rs` (~300 lines)
- `src/core/timeseries/metadata.rs` (~200 lines)
- `src/core/timeseries/mod.rs` (exports)

**Components:**
- Table struct with create/open
- PartitionScheme and PartitionValues
- PartitionWriter (thin wrapper)
- TableMetadata serialization

**Tests:**
```rust
#[test]
fn test_table_create_with_scheme()
#[test]
fn test_table_open_existing()
#[test]
fn test_partition_scheme_builder()
#[test]
fn test_partition_values_to_path()
#[test]
fn test_partition_writer_basic()
#[test]
fn test_metadata_serialization()
```

**Deliverable:**
```rust
// User can create table and write to specific partitions
let scheme = PartitionScheme::new()
    .add_string("channel")
    .add_date("date");

let table = Table::create("./l3_szse", scheme)?;

let partition = PartitionValues::new()
    .set("channel", "101")
    .set("date", "2026-01-29");

let mut writer = table.writer(partition)?;
writer.append(0x01, ts, payload)?;
writer.finish()?;
```

---

### Phase 2: Rolling Writer (Week 2)

**Goal:** Auto partition management with date/hour rolling

**Files:**
- `src/core/timeseries/rolling.rs` (~300 lines)
- `src/core/timeseries/rollers.rs` (~400 lines)

**Components:**
- RollingWriter struct
- RollingStats tracking
- PartitionRoller trait
- DateRoller, HourRoller, MinuteRoller
- CustomRoller
- Timezone support

**Tests:**
```rust
#[test]
fn test_rolling_writer_single_partition()
#[test]
fn test_rolling_writer_multi_day()
#[test]
fn test_date_roller_midnight_boundary()
#[test]
fn test_hour_roller_hour_boundary()
#[test]
fn test_timezone_utc()
#[test]
fn test_timezone_shanghai()
#[test]
fn test_custom_roller()
#[test]
fn test_rolling_stats()
```

**Deliverable:**
```rust
// User can write continuously across partition boundaries
let table = Table::open("./l3_szse")?;

let static_partition = PartitionValues::new()
    .set("channel", "101");

let mut writer = table.rolling_writer_by_date("date", static_partition)?;

for msg in multi_day_stream {
    writer.append(0x01, msg.timestamp_ns, &msg.payload)?;
    // Automatically rolls to new partition at midnight
}

let stats = writer.finish()?;
```

---

### Phase 3: Multi-Partition Reading (Week 3)

**Goal:** Query and merge multiple partitions

**Files:**
- `src/core/timeseries/table_reader.rs` (~400 lines)

**Components:**
- TableReader struct
- MergeStrategy enum
- TableMessage struct
- PartitionFilter implementation
- Min-heap based merging
- Sequential reading strategy

**Tests:**
```rust
#[test]
fn test_table_reader_single_partition()
#[test]
fn test_table_reader_multi_partition_merge()
#[test]
fn test_timestamp_ordering()
#[test]
fn test_partition_filter_exact()
#[test]
fn test_partition_filter_range()
#[test]
fn test_partition_sequential_strategy()
```

**Deliverable:**
```rust
// User can query across multiple partitions
let table = Table::open("./l3_szse")?;

// Query all channels for a date range
let filter = PartitionFilter::new()
    .range("date", Some("2026-01-29"), Some("2026-02-01"));

let mut reader = table.query(filter)?;

while let Some(msg) = reader.next()? {
    println!("partition: {:?}, ts: {}", msg.partition, msg.timestamp_ns);
}
```

---

### Phase 4: Polish & Optimization (Week 4)

**Goal:** Examples, documentation, optimizations

**Files:**
- `examples/table_basic.rs` (~150 lines)
- `examples/table_rolling.rs` (~200 lines)
- `examples/table_query.rs` (~150 lines)
- `tests/table_integration.rs` (~300 lines)
- `benches/table_write.rs` (~150 lines)

**Tasks:**
1. Comprehensive examples
2. Integration tests
3. Performance benchmarks
4. Documentation (rustdoc)
5. Error message improvements
6. Concurrent write detection
7. Partition statistics

**Tests:**
```rust
// Integration tests
#[test]
fn test_end_to_end_l3_scenario()
#[test]
fn test_parallel_channel_writers()
#[test]
fn test_multi_day_query()
#[test]
fn test_concurrent_write_detection()

// Benchmarks
fn bench_rolling_writer_10m_messages(c: &mut Criterion)
fn bench_multi_partition_query(c: &mut Criterion)
```

---

## File Structure

```
src/core/timeseries/
├── mod.rs                    (~100 lines - module exports)
├── table.rs                  (~400 lines - Table type)
├── partition.rs              (~300 lines - partition types)
├── metadata.rs               (~200 lines - serialization)
├── rolling.rs                (~300 lines - RollingWriter)
├── rollers.rs                (~400 lines - built-in rollers)
└── table_reader.rs           (~400 lines - multi-partition reading)

Total core: ~2100 lines

examples/
├── timeseries_demo.rs        (existing)
├── table_basic.rs            (~150 lines - basic usage)
├── table_rolling.rs          (~200 lines - rolling writer)
└── table_query.rs            (~150 lines - querying)

tests/
└── table_integration.rs      (~300 lines)

benches/
└── table_benchmarks.rs       (~150 lines)

Total with examples/tests: ~3050 lines
```

---

## Detailed Component Design

### Table Implementation

```rust
// src/core/timeseries/table.rs

use std::path::{Path, PathBuf};
use std::sync::Arc;
use serde::{Deserialize, Serialize};

pub struct Table {
    inner: Arc<TableInner>,
}

struct TableInner {
    root: PathBuf,
    metadata: TableMetadata,
    scheme: PartitionScheme,
    config: TableConfig,
}

impl Table {
    pub fn create(path: impl AsRef<Path>, scheme: PartitionScheme) -> Result<Self> {
        // 1. Create directories
        // 2. Create metadata
        // 3. Save metadata to _table/metadata.json
        // 4. Return Table instance
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        // 1. Validate directory exists
        // 2. Load metadata from _table/metadata.json
        // 3. Return Table instance
    }

    pub fn writer(&self, partition: PartitionValues) -> Result<PartitionWriter> {
        // 1. Validate partition values match scheme
        // 2. Get partition directory path
        // 3. Create partition directory if needed
        // 4. Check for concurrent writes (lock file)
        // 5. Create TimeSeriesWriter
        // 6. Wrap in PartitionWriter
    }

    pub fn rolling_writer(
        &self,
        roller: impl PartitionRoller + 'static,
    ) -> Result<RollingWriter> {
        // Delegate to RollingWriter::new
    }

    pub fn reader(&self, partition: PartitionValues) -> Result<TimeSeriesReader> {
        // 1. Validate partition
        // 2. Get partition path
        // 3. Open TimeSeriesReader
    }

    pub fn partitions(&self) -> Result<Vec<PartitionInfo>> {
        // Recursively discover all partitions
        self.discover_partitions(&self.root, PartitionValues::new(), 0)
    }

    fn discover_partitions(
        &self,
        dir: &Path,
        current_values: PartitionValues,
        depth: usize,
    ) -> Result<Vec<PartitionInfo>> {
        // Recursive partition discovery
        // Parse "key=value" directory names
        // Build PartitionInfo for each leaf
    }
}

impl Clone for Table {
    // Cheap clone using Arc
}
```

### RollingWriter Implementation

```rust
// src/core/timeseries/rolling.rs

pub struct RollingWriter {
    table: Table,
    roller: Box<dyn PartitionRoller>,
    current_partition: Option<PartitionValues>,
    current_writer: Option<TimeSeriesWriter>,
    stats: RollingStats,
    on_roll_callback: Option<Box<dyn Fn(&PartitionValues, &RollingStats) + Send>>,
}

impl RollingWriter {
    pub fn append(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()> {
        // 1. Determine target partition
        let target = self.roller.partition_for(timestamp_ns, payload)?;

        // 2. Check if roll needed
        let need_roll = match &self.current_partition {
            None => true,
            Some(current) => current != &target,
        };

        // 3. Roll if needed
        if need_roll {
            self.roll_to_partition(target)?;
        }

        // 4. Write to current partition
        self.current_writer.as_mut().unwrap()
            .append(type_id, timestamp_ns, payload)?;

        // 5. Update stats
        self.stats.messages_written += 1;

        Ok(())
    }

    fn roll_to_partition(&mut self, new_partition: PartitionValues) -> Result<()> {
        // 1. Finish current writer
        // 2. Call on_roll_callback
        // 3. Validate new partition
        // 4. Create partition directory
        // 5. Check concurrent writes
        // 6. Create new TimeSeriesWriter
        // 7. Update state and stats
    }
}
```

### DateRoller Implementation

```rust
// src/core/timeseries/rollers.rs

pub struct DateRoller {
    scheme: PartitionScheme,
    date_key: String,
    timezone: Timezone,
    static_values: PartitionValues,
}

impl PartitionRoller for DateRoller {
    fn partition_for(&self, timestamp_ns: u64, _payload: &[u8]) -> Result<PartitionValues> {
        // 1. Convert timestamp to date with timezone
        let date = timestamp_to_date(timestamp_ns, self.timezone);

        // 2. Merge with static values
        let mut partition = self.static_values.clone();
        partition = partition.set(&self.date_key, &date.format("%Y-%m-%d").to_string());

        Ok(partition)
    }
}

pub fn timestamp_to_date(timestamp_ns: u64, timezone: Timezone) -> NaiveDate {
    // Apply timezone offset
    let adjusted_ns = match timezone {
        Timezone::UTC => timestamp_ns,
        Timezone::AsiaShanghai => timestamp_ns + (8 * 3600 * 1_000_000_000),
        // ... other timezones
    };

    // Convert to date
    let secs = (adjusted_ns / 1_000_000_000) as i64;
    NaiveDateTime::from_timestamp_opt(secs, 0).unwrap().date()
}
```

### TableReader Implementation

```rust
// src/core/timeseries/table_reader.rs

use std::collections::BinaryHeap;

pub struct TableReader {
    table: Table,
    partitions: Vec<PartitionInfo>,
    readers: Vec<(PartitionValues, TimeSeriesReader)>,
    merge_strategy: MergeStrategy,
    heap: Option<BinaryHeap<PeekedMessage>>,
}

impl TableReader {
    pub fn new(
        table: Table,
        partitions: Vec<PartitionInfo>,
        strategy: MergeStrategy,
    ) -> Result<Self> {
        // 1. Open readers for all partitions
        // 2. Initialize heap if timestamp-ordered
        // 3. Peek all readers
    }

    pub fn next(&mut self) -> Result<Option<TableMessage<'_>>> {
        match self.merge_strategy {
            MergeStrategy::TimestampOrdered => self.next_timestamp_ordered(),
            MergeStrategy::PartitionSequential => self.next_sequential(),
        }
    }

    fn next_timestamp_ordered(&mut self) -> Result<Option<TableMessage<'_>>> {
        // 1. Pop from min-heap
        // 2. Read message from corresponding reader
        // 3. Peek next from that reader and push to heap
        // 4. Return message
    }
}

struct PeekedMessage {
    partition_idx: usize,
    timestamp_ns: u64,
    seq: u64,
    // ... other fields
}

impl Ord for PeekedMessage {
    // Min-heap by timestamp (reverse order)
}
```

---

## Error Handling

```rust
// src/core/error.rs (additions)

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // Existing errors...

    /// Table not found at path
    #[error("table not found: {0}")]
    TableNotFound(String),

    /// Invalid table structure
    #[error("invalid table: {0}")]
    InvalidTable(&'static str),

    /// Partition not found
    #[error("partition not found: {0}")]
    PartitionNotFound(String),

    /// Invalid partition values
    #[error("invalid partition values: {0}")]
    InvalidPartition(String),

    /// Concurrent write detected
    #[error("concurrent write to partition detected")]
    ConcurrentWriteDetected,

    /// Backward partition roll attempted
    #[error("cannot roll backwards: current={0:?}, attempted={1:?}")]
    BackwardRoll(PartitionValues, PartitionValues),

    /// Partition scheme mismatch
    #[error("partition scheme mismatch")]
    SchemeMismatch,
}
```

---

## Testing Strategy

### Unit Tests (Per Component)

**table.rs:**
```rust
#[test]
fn test_table_create()
#[test]
fn test_table_open()
#[test]
fn test_partition_writer()
#[test]
fn test_partition_validation()
#[test]
fn test_concurrent_write_detection()
```

**partition.rs:**
```rust
#[test]
fn test_partition_scheme_builder()
#[test]
fn test_partition_values_to_path()
#[test]
fn test_partition_values_from_path()
#[test]
fn test_partition_filter_exact()
#[test]
fn test_partition_filter_range()
```

**rolling.rs:**
```rust
#[test]
fn test_rolling_writer_single_partition()
#[test]
fn test_rolling_writer_multi_partition()
#[test]
fn test_rolling_stats()
#[test]
fn test_on_partition_roll_callback()
```

**rollers.rs:**
```rust
#[test]
fn test_date_roller_utc()
#[test]
fn test_date_roller_shanghai()
#[test]
fn test_date_roller_midnight_boundary()
#[test]
fn test_hour_roller()
#[test]
fn test_custom_roller()
```

**table_reader.rs:**
```rust
#[test]
fn test_table_reader_single_partition()
#[test]
fn test_table_reader_merge_ordering()
#[test]
fn test_table_reader_sequential()
```

### Integration Tests

```rust
// tests/table_integration.rs

#[test]
fn test_end_to_end_l3_scenario() {
    // 1. Create table
    // 2. Write across multiple channels and days
    // 3. Query by channel
    // 4. Query by date range
    // 5. Verify message ordering
}

#[test]
fn test_parallel_channel_writers() {
    // 1. Spawn 10 threads, each writing to different channel
    // 2. Verify no conflicts
    // 3. Verify all partitions created correctly
}

#[test]
fn test_rolling_across_midnight() {
    // 1. Write messages spanning midnight
    // 2. Verify partition roll happened at correct time
    // 3. Verify no messages lost
}

#[test]
fn test_multi_day_query() {
    // 1. Write 3 days of data
    // 2. Query 2-day range
    // 3. Verify correct partitions selected
    // 4. Verify message ordering
}
```

### Benchmarks

```rust
// benches/table_benchmarks.rs

fn bench_rolling_writer_1m_messages(c: &mut Criterion) {
    // Measure throughput of rolling writer
    // Expected: ~1M msg/sec
}

fn bench_partition_switch_overhead(c: &mut Criterion) {
    // Measure cost of rolling to new partition
    // Expected: <1ms
}

fn bench_multi_partition_query_10_partitions(c: &mut Criterion) {
    // Measure merged read performance
    // Expected: >500K msg/sec
}
```

---

## Examples

### Example 1: Basic Table Usage

```rust
// examples/table_basic.rs

use chronicle::core::{Table, PartitionScheme, PartitionValues};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create table
    let scheme = PartitionScheme::new()
        .add_string("channel")
        .add_date("date");

    let table = Table::create("./my_table", scheme)?;

    // Write to specific partition
    let partition = PartitionValues::new()
        .set("channel", "101")
        .set("date", "2026-01-29");

    let mut writer = table.writer(partition)?;

    for i in 0..1000 {
        let ts = 1706486400000000000 + (i * 1000000000);
        let data = format!("message_{}", i);
        writer.append(0x01, ts, data.as_bytes())?;
    }

    writer.finish()?;

    // Read partition
    let partition = PartitionValues::new()
        .set("channel", "101")
        .set("date", "2026-01-29");

    let mut reader = table.reader(partition)?;

    let mut count = 0;
    while let Some(msg) = reader.next()? {
        count += 1;
    }

    println!("Read {} messages", count);

    Ok(())
}
```

### Example 2: Rolling Writer

```rust
// examples/table_rolling.rs

use chronicle::core::{Table, PartitionScheme, PartitionValues};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let table = Table::open("./my_table")?;

    // Create rolling writer for channel 101
    let static_partition = PartitionValues::new()
        .set("channel", "101");

    let mut writer = table.rolling_writer_by_date("date", static_partition)?
        .on_partition_roll(|partition, stats| {
            println!("Rolled to {:?}, wrote {} messages so far",
                partition,
                stats.messages_written
            );
        });

    // Write messages spanning 3 days
    let base_ts = 1706486400000000000;  // 2026-01-29 00:00:00 UTC

    for day in 0..3 {
        for hour in 0..24 {
            for msg in 0..1000 {
                let offset = (day * 86400 + hour * 3600 + msg) * 1_000_000_000;
                let ts = base_ts + offset;
                let data = format!("day={} hour={} msg={}", day, hour, msg);
                writer.append(0x01, ts, data.as_bytes())?;
            }
        }
    }

    let stats = writer.finish()?;

    println!("Wrote {} messages across {} partitions",
        stats.messages_written,
        stats.partitions_created
    );
    println!("Performed {} partition rolls", stats.partition_rolls);

    Ok(())
}
```

### Example 3: Multi-Partition Query

```rust
// examples/table_query.rs

use chronicle::core::{Table, PartitionFilter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let table = Table::open("./my_table")?;

    // List all partitions
    let partitions = table.partitions()?;
    println!("Found {} partitions:", partitions.len());
    for p in &partitions {
        println!("  {:?}", p.values);
    }

    // Query specific channel, date range
    let filter = PartitionFilter::new()
        .exact("channel", "101")
        .range("date", Some("2026-01-29"), Some("2026-01-31"));

    let mut reader = table.query(filter)?;

    let mut count = 0;
    let mut first_ts = None;
    let mut last_ts = None;

    while let Some(msg) = reader.next()? {
        if first_ts.is_none() {
            first_ts = Some(msg.timestamp_ns);
        }
        last_ts = Some(msg.timestamp_ns);
        count += 1;
    }

    println!("Read {} messages", count);
    println!("Time range: {} - {}",
        first_ts.unwrap(),
        last_ts.unwrap()
    );

    Ok(())
}
```

---

## Performance Considerations

### Write Performance

**Target:** >1M msg/sec per partition

**Optimizations:**
- Direct delegation to TimeSeriesWriter (no overhead)
- Partition rolling: <1ms overhead
- Lock file check: cached in memory

### Read Performance

**Single partition:** Same as TimeSeriesReader (~2M msg/sec)

**Multi-partition merge:**
- Target: >500K msg/sec with timestamp ordering
- Min-heap overhead: O(log P) per message, P = partition count
- Sequential mode: ~2M msg/sec (no merging)

### Memory

**Per RollingWriter:**
- One TimeSeriesWriter: ~8KB
- Partition tracking: <1KB
- Total: ~10KB

**Per TableReader (merged):**
- P TimeSeriesReaders: P * 8KB
- Min-heap: P * 32 bytes
- Payload buffers: P * 8KB
- Total: ~16KB per partition

---

## Future Extensions

### Phase 5+: Advanced Features

1. **Partition Pruning with Statistics**
   - Store min/max timestamp per partition
   - Skip partitions outside query range
   - Faster query planning

2. **Compaction**
   - Merge small partitions
   - Rewrite with better compression
   - Background compaction thread

3. **Schema Evolution**
   - Add/remove partition keys
   - Migrate data to new scheme
   - Version tracking

4. **Metadata Store**
   - Centralized partition metadata
   - Query optimization
   - Statistics aggregation

5. **Export Integration**
   - Export partitions to Parquet
   - Partition-level exports
   - Parallel export workers

---

## Summary

### Deliverables

- ✅ Self-contained Table abstraction (Delta Lake style)
- ✅ Flexible partition schemes (String, Int, Date, Hour, Minute)
- ✅ Single partition writers (explicit control)
- ✅ Rolling writers (automatic partition management)
- ✅ Multi-partition querying with timestamp merge
- ✅ Comprehensive examples and tests
- ✅ Performance: >1M msg/sec write, >500K msg/sec merged read

### Code Metrics

**Core Implementation:** ~2100 lines
- table.rs: 400 lines
- partition.rs: 300 lines
- metadata.rs: 200 lines
- rolling.rs: 300 lines
- rollers.rs: 400 lines
- table_reader.rs: 400 lines
- mod.rs: 100 lines

**Examples & Tests:** ~800 lines
- examples: 500 lines
- tests: 300 lines

**Total:** ~3000 lines

### Timeline

**Week 1:** Phase 1 - Core Table Infrastructure
**Week 2:** Phase 2 - Rolling Writer
**Week 3:** Phase 3 - Multi-Partition Reading
**Week 4:** Phase 4 - Polish & Optimization

**Total:** 3-4 weeks for full implementation

---

## Questions & Clarifications

### Open Questions for Implementation

1. **Partition key extraction**: For L3 data, how is payload structured?
   - Fixed format: "CHANNEL,data..."
   - Protocol buffer with channel field
   - Custom binary format

2. **Date extraction**: Use message timestamp or date field in payload?

3. **Writer LRU**: Max open writers per RollingWriter? (Default 16?)

4. **Timezone**: UTC or Asia/Shanghai for date partitions?

5. **Metadata versioning**: Support schema evolution in first version?

---

**Document Status:** Complete
**Next Action:** Begin Phase 1 implementation after approval
