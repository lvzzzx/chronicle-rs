# Compression Guide

Chronicle-RS now includes a comprehensive compression system for automatic hot/cold storage tiering.

## Overview

The compression system provides:
- **Flexible policies**: Compress based on age, idle time, size, or immediately
- **Transparent access**: Read from .q (hot) or .q.zst (cold) seamlessly
- **Atomic operations**: Safe compression with verification
- **Access tracking**: Compress only truly idle data

## Quick Start

### 1. Create a Table with Compression

```rust
use chronicle::table::{Table, PartitionScheme, TableConfig, CompressionPolicy};

let scheme = PartitionScheme::new()
    .add_string("symbol")
    .add_date("date");

let config = TableConfig {
    compression_policy: CompressionPolicy::AgeAfter { days: 1 },
    compression_level: 3,
    ..Default::default()
};

let table = Table::create("./market_data", scheme, config)?;
```

### 2. Run Lifecycle Management

```rust
use chronicle::lifecycle::{StorageLifecycleManager, LifecycleConfig};

let lifecycle_config = LifecycleConfig {
    policy: CompressionPolicy::IdleAfter { days: 7, track_reads: true },
    ..Default::default()
};

let mut manager = StorageLifecycleManager::new("./market_data", lifecycle_config)?;
let stats = manager.run_once()?;

println!("Compressed {} segments, saved {} bytes", 
    stats.compressed_count, stats.bytes_saved);
```

## Compression Policies

### Never
Never compress (always keep hot).

```rust
CompressionPolicy::Never
```

**Use case:** Hot analytics, real-time trading data

### Immediate
Compress immediately after segment is sealed.

```rust
CompressionPolicy::Immediate
```

**Use case:** Archival storage, historical datasets

### AgeAfter
Compress after N days since creation.

```rust
CompressionPolicy::AgeAfter { days: 1 }
```

**Use case:** Time-series data (compress yesterday's data)

### IdleAfter
Compress after N days of no reads.

```rust
CompressionPolicy::IdleAfter { 
    days: 7, 
    track_reads: true 
}
```

**Use case:** IPC, frequently queried data

### SizeThreshold
Compress when segment size exceeds threshold.

```rust
CompressionPolicy::SizeThreshold { 
    bytes: 100 * 1024 * 1024  // 100MB
}
```

**Use case:** Large segments, storage-constrained systems

## Storage Format

### Hot Storage (.q)
- Uncompressed segments
- Memory-mapped for zero-copy reads
- Fast random access
- Used for active/recent data

### Cold Storage (.q.zst)
- Seekable zstd compression
- Block-based decompression
- ~50% space savings typical
- Automatic via UnifiedSegmentReader

### File Layout

```
partition_dir/
├─ 000000000.q              ← HOT: fast access
├─ 000000000.idx            ← Seek index (always exists)
├─ 000000001.q.zst          ← COLD: compressed
├─ 000000001.idx            ← Same index format
├─ 000000002.q              ← HOT
└─ .access_log              ← Access tracking (optional)
```

## Configuration

### TableConfig

```rust
TableConfig {
    segment_size: 64 * 1024 * 1024,     // 64MB
    index_stride: 100,                   // Index every 100 records
    compression_policy: CompressionPolicy::IdleAfter {
        days: 7,
        track_reads: true,
    },
    compression_level: 3,                // Zstd level (1-22)
    compression_block_size: 1024 * 1024, // 1MB blocks
    track_access: true,                  // Enable access tracking
}
```

### LifecycleConfig

```rust
LifecycleConfig {
    policy: CompressionPolicy::default(),
    compression_level: 3,          // 1=fast, 22=max compression
    block_size: 1024 * 1024,       // Seek block size
    parallel_workers: 4,           // Future: parallel compression
}
```

## Access Tracking

Enable access tracking to compress only idle data:

```rust
let config = TableConfig {
    track_access: true,
    compression_policy: CompressionPolicy::IdleAfter {
        days: 7,
        track_reads: true,  // Use access tracking
    },
    ..Default::default()
};
```

Access times are persisted to `.access_log` files:
```
segment_id,unix_timestamp
0,1706486400
1,1706486500
```

## Performance

### Write Performance
- **Hot storage**: Same as before (~1M msg/sec)
- **Immediate compression**: ~500K msg/sec (compression overhead)
- **Deferred compression**: No impact on writes

### Read Performance
- **Hot storage**: ~2M msg/sec (memory-mapped)
- **Cold storage**: ~500K msg/sec (decompression overhead)
- **Transparent**: Same API for both

### Space Savings
- **Typical**: 50-70% reduction
- **Text data**: 70-90% reduction
- **Binary data**: 30-50% reduction

## Migration Guide

### From ArchiveWriter

**Before:**
```rust
use chronicle::storage::ArchiveWriter;

let writer = ArchiveWriter::new(root, venue, symbol, date, stream, size)?;
```

**After:**
```rust
use chronicle::table::{Table, PartitionScheme, TableConfig};

let scheme = PartitionScheme::new()
    .add_string("venue")
    .add_string("symbol")
    .add_date("date")
    .add_string("stream");

let table = Table::create(root, scheme, TableConfig::default())?;
let partition = PartitionValues::new()
    .set("venue", venue)
    .set("symbol", symbol)
    .set("date", date)
    .set("stream", stream);

let writer = table.writer(partition)?;
```

### From TierManager

**Before:**
```rust
use chronicle::storage::{TierConfig, TierManager};

let config = TierConfig::new(root);
let manager = TierManager::new(config);
manager.run_once()?;
```

**After:**
```rust
use chronicle::lifecycle::{StorageLifecycleManager, LifecycleConfig};

let config = LifecycleConfig::default();
let mut manager = StorageLifecycleManager::new(root, config)?;
manager.run_once()?;
```

## Examples

See `examples/compression_policies.rs` for comprehensive examples of:
- Immediate compression (archival)
- Age-based compression (time-series)
- Idle-based compression (IPC)
- Manual lifecycle management

Run with:
```bash
cargo run --example compression_policies
```

## Best Practices

1. **Choose the right policy:**
   - Archival: `Immediate`
   - Time-series: `AgeAfter { days: 1 }`
   - IPC/frequently accessed: `IdleAfter { days: 7, track_reads: true }`
   - Hot analytics: `Never`

2. **Tune compression level:**
   - Level 1-3: Fast compression, good for streaming
   - Level 3-6: Balanced (recommended default: 3)
   - Level 7-22: Higher compression, slower

3. **Set appropriate block size:**
   - Smaller (64KB): Better seek performance
   - Larger (1MB): Better compression ratio
   - Default 1MB is good for most use cases

4. **Enable access tracking for idle-based policies:**
   ```rust
   track_access: true
   ```

5. **Run lifecycle management periodically:**
   ```rust
   // Cron job or background thread
   loop {
       manager.run_once()?;
       std::thread::sleep(Duration::from_secs(3600)); // Every hour
   }
   ```

## Troubleshooting

### High CPU usage during compression
- Reduce `compression_level` (try 1-2)
- Increase lifecycle run interval
- Use `SizeThreshold` to limit compression work

### High memory usage
- Reduce `compression_block_size`
- Limit parallel workers (when implemented)

### Data not compressing
- Check if segments are sealed
- Verify policy criteria are met
- Check `LifecycleStats` for errors

### Compression not saving space
- Binary data compresses less than text
- Already compressed data won't compress further
- Small segments have overhead

## Future Enhancements

- [ ] Parallel compression workers
- [ ] Remote storage tiering (S3, etc.)
- [ ] Decompression back to hot on access
- [ ] Per-partition policies
- [ ] Compression statistics and monitoring
