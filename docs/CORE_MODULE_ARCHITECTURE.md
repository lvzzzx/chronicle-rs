# Chronicle-RS Core Module Architecture

## Overview

The `core` module implements a **single-writer, multiple-reader (SWMR)** persistent message queue optimized for **high-frequency trading (HFT)** workloads. It achieves microsecond-level latency through lock-free coordination, memory-mapped I/O, and careful cache-line optimization.

**Key Design Goals**:
- Sub-microsecond write latency (P50: 100-300ns)
- Zero-copy reads via memory mapping
- Lock-free message passing (atomics + futex)
- Crash recovery with durability guarantees
- Segment-based retention for multi-TB datasets

---

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                         QueueWriter                             │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │ PreallocWorker│  │RetentionWorker│  │ AsyncSealer  │          │
│  │  (background) │  │  (background) │  │ (background) │          │
│  └──────────────┘  └──────────────┘  └──────────────┘          │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
                    ┌──────────────────┐
                    │   ControlFile    │ ◄─── Shared coordination block
                    │  (mmap, atomics) │
                    └──────────────────┘
                              ▲
                              │
┌─────────────────────────────────────────────────────────────────┐
│                      QueueReader (×N)                           │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
│  │  Seek Index  │  │ WaitStrategy │  │Reader Metadata│          │
│  │  (sparse)    │  │(spin/futex)  │  │ (double-slot)│          │
│  └──────────────┘  └──────────────┘  └──────────────┘          │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
         ┌───────────────────────────────────────┐
         │        Memory-Mapped Segments         │
         │  [000000000.q] [000000001.q] [...]    │
         │     64B Header + Aligned Records      │
         └───────────────────────────────────────┘
```

---

## Component Breakdown

### 1. **MessageHeader** (`header.rs`)

The wire format for individual messages. **64 bytes, cache-line aligned**.

```rust
#[repr(C, align(64))]
pub struct MessageHeader {
    commit_len: u32,      // 0 = uncommitted, >0 = (payload_len + 1)
    version: u8,          // Wire format version (v1)
    _pad0: [u8; 3],
    seq: u64,             // Monotonic sequence number
    timestamp_ns: u64,    // Nanosecond timestamp
    type_id: u16,         // User-defined message type
    flags: u16,
    checksum: u32,        // CRC32 of payload
    _pad: [u8; 32],       // Cache-line fill
}
```

**Critical Design: Two-Phase Commit**

1. Writer allocates space, writes payload
2. Writer writes full header with `commit_len = 0` (uncommitted)
3. **Atomic store** (Release ordering) of `commit_len = payload_len + 1`
4. Reader sees committed message via **Atomic load** (Acquire ordering)

This prevents torn reads without locks.

**Why 64 bytes?**
- Matches CPU cache line (prevents false sharing)
- Single atomic operation to check `commit_len`
- Padding ensures next record starts on new cache line

---

### 2. **Segment** (`segment.rs`)

Messages are stored in **segmented files** (default 128MB each).

```
Segment File Layout (000000001.q):
┌─────────────────────────────────────────────┐
│  Segment Header (64B)                       │
│  - magic: 0x53454730 ('SEG0')               │
│  - version: 2                                │
│  - segment_id: 1                             │
│  - flags: SEALED (1) | UNSEALED (0)         │
├─────────────────────────────────────────────┤
│  Data Offset (64B)                          │
├─────────────────────────────────────────────┤
│  MessageHeader (64B)                        │
│  Payload (variable, aligned to 64B)         │
├─────────────────────────────────────────────┤
│  MessageHeader (64B)                        │
│  Payload (variable, aligned to 64B)         │
├─────────────────────────────────────────────┤
│  ...                                         │
└─────────────────────────────────────────────┘
```

**Key Functions**:

- `prefault_mmap()`: **Critical HFT optimization**
  - Writes to every 4KB page on creation
  - Forces OS to allocate physical RAM immediately
  - Eliminates page fault latency (10-100μs) from hot path

- `repair_unsealed_tail()`: Crash recovery
  - Scans for uncommitted messages
  - Pads remainder with `PAD_TYPE_ID` messages
  - Seals segment atomically

- `publish_segment()`: Uses `renameat2(RENAME_NOREPLACE)` on Linux
  - Atomic segment activation
  - Prevents partial writes from being visible

**Why Segmented?**
- **Retention**: Delete old segments without touching active data
- **Parallelism**: Readers on different segments don't contend
- **Growth**: Support multi-TB queues without massive files

---

### 3. **QueueWriter** (`writer.rs`)

Single writer instance with background workers.

#### Write Path (Hot Path)

```rust
pub fn append(&mut self, type_id: u16, payload: &[u8]) -> Result<()> {
    // 1. Check capacity (backpressure if queue full)
    self.ensure_capacity(record_len)?;

    // 2. Check if segment roll needed
    if self.write_offset + record_len > segment_size {
        self.roll_segment()?;  // May block for prealloc
    }

    // 3. Write payload directly to mmap (zero-copy)
    let offset = self.write_offset as usize;
    self.mmap.range_mut(offset + HEADER_SIZE, payload_len)?
        .copy_from_slice(payload);

    // 4. Write header with checksum
    let checksum = MessageHeader::crc32(payload);
    let header = MessageHeader::new_uncommitted(seq, timestamp_ns, type_id, 0, checksum);
    self.mmap.range_mut(offset, HEADER_SIZE)?
        .copy_from_slice(&header.to_bytes());

    // 5. Atomic commit (RELEASE ordering)
    let commit_len = payload_len + 1;
    MessageHeader::store_commit_len(header_ptr, commit_len);

    // 6. Update control block (for readers)
    self.control.set_write_offset(self.write_offset);

    // 7. Wake waiting readers (if any)
    if self.control.waiters_pending() > 0 {
        futex_wake(self.control.notify_seq())?;  // Syscall
    }

    Ok(())
}
```

**Optimizations**:

1. **Signal Suppression** (line 7):
   - Only call `futex_wake()` if readers are sleeping
   - Avoids syscall overhead (~500ns) when readers are spinning

2. **Metrics Tracking**:
   - `bytes_written`, `rolls` updated inline
   - Zero overhead (saturating_add)

#### Background Workers

**PreallocWorker**:
```rust
// Runs in background thread
loop {
    let next_segment_id = self.desired.swap(...);
    let mmap = prepare_segment_temp(root, next_segment_id, segment_size)?;
    prefault_mmap(&mmap);  // Force page allocation
    publish_segment(temp_path, final_path)?;
    ready_tx.send(PreparedSegment { segment_id, mmap })?;
}
```

When writer rolls to next segment:
- **Fast path**: Preallocated segment ready (~1-5μs)
- **Slow path**: Wait with spin-then-yield (~100-500μs)
- **Fallback**: Allocate synchronously (~10-50ms)

**RetentionWorker**:
```rust
// Runs in background thread
loop {
    let (head_segment, head_offset) = recv_timeout(check_interval);

    // Update min reader position
    let pos = min_live_reader_position(path, head_segment, head_offset, config)?;
    min_reader_pos.store(pos, Ordering::Release);

    // Delete old segments
    cleanup_segments(path, head_segment, head_offset, config)?;
}
```

Offloads filesystem I/O from write path.

**AsyncSealer** (optional):
```rust
// Runs in background thread
loop {
    let sealed_mmap = recv()?;
    sealed_mmap.sync()?;  // fsync in background
}
```

Trades durability for latency:
- **Without**: Writer blocks on fsync (~1-10ms)
- **With**: Writer continues, fsync happens async
- **Risk**: Power loss before fsync = data loss

---

### 4. **QueueReader** (`reader.rs`)

Multiple concurrent readers, each tracking position independently.

#### Read Path

```rust
pub fn next(&mut self) -> Result<Option<MessageView<'_>>> {
    loop {
        let offset = self.read_offset as usize;

        // 1. Atomic load of commit_len (ACQUIRE ordering)
        let commit = MessageHeader::load_commit_len(&self.mmap[offset]);

        // 2. If uncommitted, try advance to next segment or wait
        if commit == 0 {
            if self.advance_segment()? { continue; }
            return Ok(None);
        }

        // 3. Decode header and validate
        let header = MessageHeader::from_bytes(&self.mmap[offset..offset+64])?;
        let payload = &self.mmap[offset+64..offset+64+payload_len];
        header.validate_crc(payload)?;

        // 4. Skip padding records
        if header.type_id == PAD_TYPE_ID { continue; }

        // 5. Return zero-copy view
        return Ok(Some(MessageView {
            seq: header.seq,
            timestamp_ns: header.timestamp_ns,
            type_id: header.type_id,
            payload,  // Borrowed from mmap
        }));
    }
}
```

**Wait Strategies**:

```rust
pub enum WaitStrategy {
    BusySpin,  // 100% CPU, lowest latency (~10-20ns poll)
    SpinThenPark { spin_us: 10 },  // Hybrid: spin → futex_wait
    Sleep(Duration),  // Periodic poll, low CPU
}
```

**SpinThenPark Protocol** (default):
```rust
// 1. Register as waiter (SeqCst ordering)
self.control.waiters_pending().fetch_add(1, Ordering::SeqCst);

// 2. Load notify_seq
let seq = self.control.notify_seq().load(Ordering::Acquire);

// 3. Double-check data available (check-after-set pattern)
if self.peek_committed()? {
    self.control.waiters_pending().fetch_sub(1, Ordering::SeqCst);
    return Ok(());
}

// 4. Sleep on futex (kernel will wake us)
futex_wait(self.control.notify_seq(), seq, timeout)?;

// 5. Deregister
self.control.waiters_pending().fetch_sub(1, Ordering::SeqCst);
```

This prevents missed wakeups even if writer publishes between step 2 and 4.

#### Batch APIs (New in This Release)

```rust
// Allocating batch read
let messages: Vec<OwnedMessage> = reader.drain_available(1000)?;

// Zero-copy callback (no allocations)
let count = reader.for_each_available(1000, |msg| {
    process(msg);
    true  // Continue
})?;
```

---

### 5. **ControlFile** (`control.rs`)

Shared coordination block between writer and readers. **512 bytes, cache-line optimized**.

```rust
#[repr(C, align(128))]
pub struct ControlBlock {
    // Cache Line 0: Cold fields (read once at startup)
    magic: AtomicU32,
    version: AtomicU32,
    writer_epoch: AtomicU64,
    segment_size: AtomicU64,
    _pad0: [u8; 96],

    // Cache Line 1: Reader-hot, rarely written
    segment_gen: AtomicU32,      // Seqlock for segment transitions
    current_segment: AtomicU32,
    _pad1: [u8; 120],

    // Cache Line 2: Writer-hot
    write_offset: AtomicU64,
    writer_heartbeat_ns: AtomicU64,
    _pad2: [u8; 112],

    // Cache Line 3: Coordination (wake/wait)
    notify_seq: AtomicU32,
    waiters_pending: AtomicU32,
    _pad3: [u8; 120],
}
```

**Why Separate Cache Lines?**

Without separation:
- Writer updates `write_offset` → invalidates reader's cache line
- Reader checks `write_offset` → invalidates writer's cache line
- **Result**: Cache ping-pong, latency spikes

With separation:
- Each field group lives on its own cache line
- Independent updates don't cause false sharing
- **Result**: Clean cache access patterns

**Seqlock for Segment Transitions**:

```rust
// Writer rolls to next segment
pub fn set_segment_index(&self, segment: u32, offset: u64) {
    self.segment_gen.fetch_add(1, Ordering::SeqCst);  // Mark writing
    self.current_segment.store(segment, Ordering::Relaxed);
    self.write_offset.store(offset, Ordering::Relaxed);
    self.segment_gen.fetch_add(1, Ordering::SeqCst);  // Mark stable
}

// Reader reads consistent snapshot
pub fn segment_index(&self) -> (u32, u64) {
    loop {
        let start = self.segment_gen.load(Ordering::Acquire);
        if (start & 1) != 0 { continue; }  // Writer is updating

        let segment = self.current_segment.load(Ordering::Acquire);
        let offset = self.write_offset.load(Ordering::Acquire);

        let end = self.segment_gen.load(Ordering::Acquire);
        if start == end && (end & 1) == 0 {
            return (segment, offset);  // Consistent snapshot
        }
    }
}
```

This allows readers to see atomic transitions without blocking writer.

---

### 6. **Writer Lock** (`writer_lock.rs`)

Prevents multiple writers via **flock + process liveness detection**.

```rust
pub fn acquire(path: &Path) -> Result<WriterLock> {
    let file = OpenOptions::new().create(true).write(true).open(path)?;

    loop {
        // Try exclusive lock (non-blocking)
        if flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) == 0 {
            let (pid, start_time) = lock_identity()?;
            write_lock_record(&file, pid, start_time, epoch)?;
            return Ok(WriterLock { file });
        }

        // Lock held by another process - check if it's alive
        let info = read_lock_info(path)?;
        if !lock_owner_alive(&info)? {
            continue;  // Process dead, retry acquire
        }

        return Err(Error::WriterAlreadyActive);
    }
}
```

**Process Liveness Check** (Linux):
```rust
fn lock_owner_alive(info: &WriterLockInfo) -> Result<bool> {
    let proc_start = proc_start_time(info.pid)?;  // Read /proc/{pid}/stat
    Ok(proc_start == info.start_time)
}
```

This prevents PID reuse attacks:
- Old process (PID 1234, start_time=100) crashes
- New process gets PID 1234 (start_time=200)
- Lock check sees different start_time → knows it's a different process

---

### 7. **Retention** (`retention.rs`)

Automatic cleanup of old segments.

```rust
pub struct RetentionConfig {
    pub reader_ttl: Duration,      // 30s default
    pub max_reader_lag: u64,       // 10GB default
}
```

**Algorithm**:

1. Scan `readers/*.meta` for all reader positions
2. Filter out stale readers:
   - No heartbeat in last `reader_ttl` → ignore
   - More than `max_reader_lag` behind → ignore
3. Find minimum position across live readers
4. Delete segments below minimum

**Protection**:
- Slow reader won't block retention forever (max_reader_lag)
- Zombie reader won't block retention (reader_ttl)
- Writer never waits on deletion (background thread)

---

### 8. **Seek Index** (`seek_index.rs`)

Sparse index for time-travel queries.

```
Index File (000000001.idx):
┌────────────────────────────────┐
│  Header (80B)                  │
│  - min_seq, max_seq            │
│  - min_ts_ns, max_ts_ns        │
│  - entry_count, stride=4096    │
├────────────────────────────────┤
│  Entry[0]: seq=0, ts, offset   │
│  Entry[1]: seq=4096, ts, offset│
│  Entry[2]: seq=8192, ts, offset│
│  ...                            │
└────────────────────────────────┘
```

**Stride = 4096** means one index entry per 4096 messages:
- Worst case: Scan 2048 messages (average) after index lookup
- Index size: ~24 bytes per 4096 messages = 0.006% overhead

**Seek Algorithm**:

```rust
pub fn seek_seq(&mut self, target_seq: u64) -> Result<bool> {
    let headers = load_seek_headers(segments)?;
    let header = select_header_for_seq(headers, target_seq);  // Binary search
    let entries = load_index_entries(header)?;
    let entry = find_entry_by_seq(entries, target_seq)?;  // Binary search

    self.open_segment_for_seek(entry.segment_id, entry.offset)?;
    self.scan_to_seq(target_seq)?;  // Linear scan
    Ok(true)
}
```

Average latency:
- Index lookup: ~10μs (2-3 disk reads)
- Linear scan: ~50μs (2048 messages in memory)
- **Total**: ~60μs to seek billions of messages

---

## Performance Characteristics

### Write Path

| Operation | Latency (P50) | Latency (P99) | Notes |
|-----------|---------------|---------------|-------|
| `append()` (no roll) | 100-300ns | 500ns-1μs | Pure mmap write |
| `append()` (with roll, preallocated) | 1-5μs | 10-50μs | Swap mmap pointers |
| `append()` (with roll, no prealloc) | 10-50ms | 100-500ms | fsync + page faults |
| `futex_wake()` | 500ns-1μs | 2-5μs | Only if readers waiting |

### Read Path

| Operation | Latency | Notes |
|-----------|---------|-------|
| `next()` (data available) | 50-150ns | Atomic load + memcpy |
| `next()` (data available, validation) | 200-500ns | + CRC32 check |
| `wait()` (BusySpin) | 10-20ns/poll | 100% CPU |
| `wait()` (SpinThenPark) | 10-20μs | Spin → futex |
| `seek_seq()` | ~60μs | Index + linear scan |

### Memory Overhead

| Component | Size | Notes |
|-----------|------|-------|
| `MessageHeader` | 64B | Per message |
| `SegmentHeader` | 64B | Per segment (128MB) |
| `ControlBlock` | 512B | Shared |
| `ReaderMeta` | 80B | Per reader |
| Seek Index | 24B / 4096 msgs | 0.006% overhead |

---

## Concurrency Model

### Writer Side (Single Thread)

```
Main Thread:
  ├─ append() [hot path]
  ├─ roll_segment() [warm path]
  └─ flush_sync() [cold path]

Background Threads:
  ├─ PreallocWorker: prepare next segment
  ├─ RetentionWorker: cleanup old segments
  └─ AsyncSealer: fsync sealed segments
```

**No locks in hot path**. All coordination via atomics.

### Reader Side (Multiple Threads)

```
Reader Thread 1:
  ├─ next() [read-only]
  ├─ wait() [may futex_wait]
  └─ commit() [writes reader.meta]

Reader Thread 2:
  ├─ next() [read-only]
  └─ ...

Reader Thread N:
  └─ ...
```

Readers are **fully independent**:
- No shared state between readers
- No locks
- Each tracks position in `readers/{name}.meta`

---

## Failure Modes & Recovery

### Writer Crashes

1. **During Message Write**:
   - Uncommitted message (`commit_len = 0`)
   - Readers skip it
   - Next writer repairs tail: `repair_unsealed_tail()`

2. **After Commit, Before Segment Seal**:
   - Segment marked UNSEALED
   - Next writer seals it: `seal_segment()`
   - Padding added to fill remainder

3. **During Segment Roll**:
   - Control block points to old segment
   - Next writer reads control block, continues from there
   - Temp segments (`.q.tmp`) are cleaned up

### Reader Crashes

1. **No impact on writer or other readers**
2. Reader position saved in `readers/{name}.meta`
3. Double-slot with CRC protects against partial writes
4. Next reader instance resumes from saved position

### Disk Full

1. **Prealloc fails** → writer sees error count, logs warning
2. **Segment roll fails** → writer returns `Error::Io`
3. **Reader continues** reading existing segments

---

## Key Takeaways

### What Makes This Fast

1. **Lock-Free**: No mutexes, only atomics (seq-cst or relaxed)
2. **Zero-Copy**: Readers borrow directly from mmap
3. **Prefaulting**: Eliminates page faults from hot path
4. **Cache-Line Aware**: False sharing prevention
5. **Background Workers**: Offload expensive operations
6. **Signal Suppression**: Avoid syscalls when readers spin

### What Makes This Reliable

1. **Two-Phase Commit**: Atomic message visibility
2. **CRC32 Validation**: Detect corruption
3. **Crash Recovery**: Automatic tail repair
4. **Writer Lock**: Prevent split-brain
5. **Process Liveness**: Handle PID reuse
6. **Double-Slot Metadata**: Atomic reader position updates

### What Makes This Scalable

1. **Segmented Storage**: Multi-TB support
2. **Automatic Retention**: Background cleanup
3. **Sparse Indexing**: Fast time-travel
4. **Independent Readers**: No contention
5. **Configurable**: Tune for workload

---

## Usage Examples

### Basic Producer/Consumer

```rust
use chronicle::core::{Queue, WriterConfig, ReaderConfig};

// Writer
let mut writer = Queue::open_publisher("./queue")?;
writer.append(1, b"hello")?;
writer.flush_sync()?;

// Reader
let mut reader = Queue::open_subscriber("./queue", "my-reader")?;
while let Some(msg) = reader.next()? {
    println!("{:?}", msg.payload);
}
reader.commit()?;
```

### Low-Latency HFT Setup

```rust
use chronicle::core::WriterConfig;
use std::time::Duration;

let config = WriterConfig {
    defer_seal_sync: true,          // Async fsync
    prealloc_wait: Duration::from_millis(1),  // Spin-wait for prealloc
    require_prealloc: true,         // Fail-fast if not ready
    memlock: true,                  // Lock pages in RAM
    retention: RetentionConfig {
        reader_ttl: Duration::from_secs(5),
        max_reader_lag: 1 << 30,    // 1GB
    },
    ..WriterConfig::default()
};

let writer = Queue::open_publisher_with_config("./queue", config)?;
```

### Batch Processing

```rust
// Read 1000 messages at once
let messages = reader.drain_available(1000)?;
for msg in messages {
    process(msg);
}

// Zero-copy callback
reader.for_each_available(1000, |msg| {
    if process(msg).is_err() { return false; }
    true  // Continue
})?;
```

### Monitoring

```rust
let metrics = writer.metrics();
println!("Rolls: {}, Bytes: {}", metrics.rolls, metrics.bytes_written);
println!("Errors: prealloc={} seal={} retention={}",
    metrics.prealloc_errors,
    metrics.seal_errors,
    metrics.retention_errors
);
```

---

## Further Reading

- **Phase 1-10 Execution Plans**: `execplans/` directory
- **API Documentation**: `cargo doc --open`
- **LLM-Friendly Standards**: `docs/LLM_FRIENDLY_CODING_STANDARDS.md`
- **Chronicle Queue (Java)**: https://chronicle.software/chronicle-queue/

---

**Last Updated**: 2026-01-27
**Maintainer**: Chronicle-RS Team
