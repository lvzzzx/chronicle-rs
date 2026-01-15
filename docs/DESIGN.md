# Low-Latency Persisted Messaging Framework (Chronicle-style, Rust)

## 1. Overview
Goal: Build a **memory-mapped, persistent messaging queue** for HFT systems — combining **in-memory performance** with **durable persistence**.

Characteristics:
- Lock-free writers and readers
- `mmap`-based append-only storage
- Per-reader metadata for recovery
- Event-driven notification using Linux `eventfd`
- Multi-queue fan-in (many writers → one reader)
- Segment rolling and background retention cleanup

## 2. Core Design Principles
| Concept | Purpose |
|----------|----------|
| **Memory-mapped file (mmap)** | Gives in-memory speed with OS-backed durability. |
| **Append-only log** | Simplifies concurrency and recovery. |
| **Fixed header + valid bit** | Guarantees atomic, consistent message visibility. |
| **Per-reader metadata files** | Each reader tracks its own offset safely. |
| **Eventfd signaling** | Enables blocking reads without polling. |
| **Segment rolling** | Bounded file size, simple retention. |
| **Multi-queue fan-in** | Scales writers linearly and keeps queues isolated. |

## 3. Message Layout
### `MessageHeader` (64 bytes, 64-byte aligned)
```rust
#[repr(C, align(64))] // Cache-line aligned to prevent false sharing between writer/reader
struct MessageHeader {
    length: u32,        // 4 bytes
    seq: u64,           // 8 bytes
    timestamp_ns: u64,  // 8 bytes
    flags: u8,          // 1 byte (bit0 = valid/commit)
    type_id: u16,       // 2 bytes (msg type/schema version)
    _reserved: u8,      // 1 byte
    checksum: u32,      // 4 bytes (CRC32 of payload)
    _pad: [u8; 36],     // 36 bytes (Padding to reach 64 bytes)
}
```

**Write sequence:**
1. Write header with `flags = 0`.
2. Write payload.
3. Flip `flags` → `1` using `store(Ordering::Release)`.

**Read sequence:**
1. Read header.
2. Spin until `flags.load(Ordering::Acquire) == 1`.
3. If spin duration > `TIMEOUT`, treat as "Stuck Writer" (see Section 4).

## 4. Concurrency Model
### Single writer
- Uses an atomic `write_index: AtomicU64` for append position.
- Reservation:
  ```rust
  let pos = write_index.fetch_add(msg_size, Ordering::AcqRel);
  ```
- Ensures non-overlapping writes and correct memory visibility.

### Multiple writers (optional)
- Prefer **one queue per writer** (multi-queue fan-in).
- Reader merges messages by timestamp.

### Readers
- Each has its own `read_pos` persisted in `/readers/<name>.meta`.
- No lock contention; independent progress.

## 5. Event Notification System
**Goal:** Readers block until new messages arrive.

Implementation:
- Each reader creates an `eventfd(0, EFD_NONBLOCK|EFD_CLOEXEC)`.
- Writes its fd number into `/readers/<name>.efd`.
- Writer watches `/readers/` using `inotify`:
  - `IN_CREATE` → new reader
  - `IN_DELETE` → remove reader
- After commit:
  ```rust
  for r in readers {
      let _ = eventfd_write(r.fd, 1);
  }
  ```
Readers block on their own `eventfd` via `epoll_wait`.

## 6. Persistence Layout
### Directory structure
```
/var/lib/hft_bus/
└── orders/
    ├── queue/
    │   ├── writer_1/
    │   │   ├── 000000000.q
    │   │   ├── 000000001.q
    │   │   └── index.meta
    │   ├── writer_2/
    │   │   └── ...
    │
    ├── readers/
    │   ├── engine_1.meta
    │   ├── engine_1.efd
    │   ├── risk_monitor.meta
    │   └── risk_monitor.efd
    │
    └── config.toml
```

- `.q` files are **segments** (e.g. 1 GiB each).
- `index.meta` → stores `{ current_segment, write_offset }`.
- Reader `.meta` → stores last committed `read_pos`.

## 7. Segment Rolling
**Trigger:** `write_index >= SEGMENT_SIZE`
**Procedure:**
1. `msync()` and close current `.q`.
2. Increment segment ID.
3. Create `NNNNNNNNN.q` via `fallocate()`.
4. `mmap` it and reset `write_index = 0`.
5. Persist `current_segment` to `index.meta`.

**Reader behavior:**
- When reaching EOF, attempt to open next segment.
- If not found, block on eventfd.

**Cleanup:** Background process deletes segments only when *all readers*’ offsets exceed their range.

## 8. API Design
### Public Structures
```rust
pub struct Queue {
    path: PathBuf,
    mmap: MmapMut,
    write_index: AtomicU64,
    notifier: Option<Notifier>,
}

pub struct QueueWriter {
    queue: Arc<Queue>,
}

pub struct QueueReader {
    queue: Arc<Queue>,
    read_index: u64,
    reader_meta: ReaderMeta,
}

pub enum Notifier {
    EventFd(RawFd),
    BusyPoll(Duration),
}
```

### Core API
```rust
impl Queue {
    pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>>;
    pub fn writer(&self) -> QueueWriter;
    pub fn reader(&self, name: &str) -> Result<QueueReader>;
}

impl QueueWriter {
    pub fn append(&self, payload: &[u8]) -> Result<()>;
    pub fn flush(&self) -> Result<()>;
}

impl QueueReader {
    pub fn next(&mut self) -> Result<Option<&[u8]>>;
    pub fn commit(&mut self) -> Result<()>;
    pub fn wait(&self) -> Result<()>; // blocks via eventfd
}
```

## 9. Multi-Queue Fan-In (Order Bus)
**Pattern:** many writers → one reader.
Each writer appends to its own queue directory.

Reader merges by:
1. Peeking each queue’s next header.
2. Selecting the smallest `timestamp_ns`.
3. Processing and advancing that reader.

Supports deterministic, timestamp-ordered message processing.

### HFT scenario mapping
- Market-data ingest: one queue per venue/feed; a single fan-in reader merges by `timestamp_ns` for a unified stream.
- Order routing: strategy writes to one queue; multiple gateway readers consume with distinct reader names (multi-subscriber).
- Strategy fan-out: feed handlers write per-feed queues; each strategy builds its own fan-in reader and keeps independent progress.
- Risk aggregation: each strategy writes fills/positions to its own queue; risk service merges them by timestamp.
- Compliance/audit: each service writes to its own queue; recorder merges into a single ordered audit stream.
- Replay/backtest: playback processes write historical feeds to per-feed queues with recorded timestamps; fan-in reproduces a deterministic stream.

### Queue discovery for multi-process fan-in
Order routing and other HFT control-plane services are typically long-lived processes that must attach to many strategy queues without restart. Discovery is therefore required to add new sources dynamically while keeping writers isolated (one writer per queue).

**Proposed approach (directory + READY marker):**
- **Directory convention:** all strategy queues live under a shared root, e.g. `orders/strategies/<strategy_id>/`.
- **Startup handshake:** strategy initializes its queue via `Queue::open`, then creates a `READY` file using atomic rename (`READY.tmp` → `READY`). The file should include a minimal version payload (for example `version=1` on the first line, or a small JSON blob like `{"version":1,"pid":123,"created_at":"..."}`).
- **Router discovery:** establish a cross-platform filesystem watcher first (use a library such as `notify` rather than raw `inotify`), then scan for subdirectories containing `READY`, then process any events that arrived during the scan. The router must deduplicate so a queue discovered by scan and watch is only attached once.
- **Attach reader:** open the queue and create a `QueueReader` with a unique router name (e.g. `router-<pid>`), then register it with the fan-in merge layer. If the READY version is newer than the router supports, skip the queue and log a warning rather than crashing.

**Removal / shutdown:** directory deletion is the removal signal. When a queue directory is removed, the router must drop the corresponding `QueueReader` to release mmaps and file descriptors. Leaving idle readers in a long-lived router is not acceptable due to fd exhaustion risk.

**Safety / failure modes:**
- If a strategy crashes before `READY`, the router never attaches.
- If it crashes after `READY`, the router can still consume committed data.
- Router restart re-establishes the watcher, rescans, and resumes from per-reader metadata.
- Segment size checks still guard against partially initialized files.

**Alternatives (not chosen yet):**
- Manifest file (`queues.json`) with atomic updates.
- Control queue (registry bus) for announcements.
- Periodic scan only (simplest, slower to detect).

**Open questions for review:**
- Is a manifest-based option needed for multi-host deployments, or is filesystem discovery sufficient?

## 10. Memory Model Summary
| Operation | Ordering | Reason |
|------------|-----------|--------|
| Writer: append + flag | `Release` | ensure payload visible |
| Reader: poll flag | `Acquire` | ensure payload consistent |
| Writer index update | `AcqRel` | atomic append ordering |

## 11. Module Layout (proposed crate structure)
```
src/
├── lib.rs
├── mmap.rs
├── header.rs
├── writer.rs
├── reader.rs
├── notifier.rs
├── merge.rs
├── segment.rs
└── retention.rs
```

## 12. Next Steps
1. Implement `mmap.rs` (open + fallocate + msync).
2. Build minimal writer/reader loop.
3. Add eventfd-based blocking.
4. Integrate timestamp-merge reader.
5. Prototype with micro-benchmarks.
