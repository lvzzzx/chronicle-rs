Low-Latency Persisted Messaging Framework (Chronicle-style, Rust)

1. Overview

This project provides a single-host, low-latency, persisted messaging queue designed for HFT-style systems that use multiple cooperating processes. It combines in-memory performance (via mmap) with fast recovery and controlled retention.

Target deployment (initial milestone):
	•	Linux, single host
	•	Separate processes (typical): market feed, strategy, order router
	•	One-writer-per-queue, many independent readers
	•	Persistent per-reader offsets for restart and replay
	•	Segment rolling and retention cleanup
	•	Optional multi-queue fan-in reader (many queues → one consumer)

Non-goals (MVP):
	•	Multi-host networking/discovery
	•	Process supervision (starting/stopping processes)
	•	Central configuration distribution

2. Architecture: Data Plane vs Control Plane Helper

To keep the low-latency core deterministic and stable, the design is explicitly split:

2.1 Data plane: chronicle::core (the queue engine)

Responsibilities:
	•	On-disk format (segments, headers), record alignment
	•	Single-writer append protocol and recovery scanning
	•	Per-reader committed offsets and atomic update strategy
	•	Segment rolling and retention eligibility
	•	Blocking read (hybrid spin + futex wait)

Non-responsibilities:
	•	Naming conventions for “market data vs orders”
	•	Process lifecycle, orchestration, or deployment
	•	Dynamic strategy discovery policies (beyond primitives)

2.2 Control plane helper: chronicle::bus (thin glue, not a platform)

Responsibilities:
	•	Standard directory layout and endpoint naming
	•	READY/LEASE markers (readiness and liveness signals)
	•	Router discovery helpers (scan + watch + dedup)
	•	RAII registration helpers (clean shutdown cleanup)

Non-responsibilities:
	•	Starting/stopping processes
	•	Central service registry
	•	Admission control, scheduling, or quotas

Guiding principle: chronicle::bus must remain a small utility layer. Anything that smells like orchestration stays outside.

2.3 Passive Discovery & Service Resilience (Scope Split)

We deliberately split "primitives" (in this repo) from "policy" (in the trading system). This keeps the message framework ULL-safe and reusable across different risk stacks and operating models.

Why this split:
    - Hot-path integrity: discovery/recovery loops are I/O-heavy and must stay off the hot path.
    - Policy varies by shop: actions like Cancel All or Flatten are risk decisions, not framework defaults.
    - Minimal, composable primitives: the framework should expose signals, not make trading decisions.
    - Testability: state-machine behavior can be tested without coupling to risk logic.

What the framework provides (primitives):
    - Liveness / heartbeat checks for writers.
    - Clear error signals for disconnect (lock lost, heartbeat stale, segment missing).
    - Optional helper to poll discovery and emit state transitions without safety actions.

What the trading system provides (policy):
    - FSM ownership and state transitions.
    - Safety actions (Cancel All / Flatten).
    - Retry cadence, backoff, and escalation logic.

Suggested module split and APIs:
    - chronicle::core (signals only):
        - QueueReader::writer_status(ttl) -> WriterStatus
        - QueueReader::detect_disconnect(ttl) -> Option<DisconnectReason>
        - DisconnectReason enum (lock lost, heartbeat stale, segment missing)
    - chronicle::bus (optional helper, cold path):
        - SubscriberDiscovery helper that polls readiness + attempts open
        - Emits SubscriberEvent::{Connected(QueueReader), Disconnected(DisconnectReason), Waiting}
        - Does not perform safety actions or block the hot path
    - Trading system (policy):
        - Owns the FSM and performs safety actions on disconnect
        - Chooses cadence (fixed sleep, inotify, exponential backoff, etc.)
        - Pins hot thread and chooses wait strategy (busy spin or hybrid)

2.4 Hot IPC vs Archive Storage (Two-Tier Layout)

To avoid conflicting goals between low-latency IPC and long-term storage, Chronicle uses a two-tier queue layout:
    - Live queues (hot IPC): short retention, minimal durability overhead.
    - Archive queues (cold storage): long retention, aggressive indexing and snapshots.

The ArchiveTap process reads the live queue in strict order and re-appends events to the archive queue. Historical imports write directly to the archive queue and bypass live IPC entirely.

2.5 Domain-Specific Stream Tables (Market Data Replay)

Chronicle's stream layer is intentionally domain-specific. It is a replay engine for market data and is not a general-purpose state store or query engine.

Scope (market data only):
    - L2BookTable (crypto-style): snapshot + delta replay to maintain an L2 depth book.
    - Trade stream (tick-by-tick): trades are stored as a separate stream when needed.
    - L3BookTable (SZSE): order + trade events are required to reconstruct the L3 book.

Non-goals:
    - Random-access point-in-time queries.
    - General-purpose relational joins or SQL-like interfaces.

Trade <-> book relationships for feature extraction are handled in replay/ETL by consuming multiple streams.
The alignment key is ingest time:
    - MessageHeader.timestamp_ns is the canonical ingest-time for all messages.
    - BookEventHeader.ingest_ts_ns should match MessageHeader.timestamp_ns for book events.

For SZSE L3 reconstruction rules and phase handling, see DESIGN-szse-l3-reconstructor.md.

2.6 Layering & Contracts (Module Boundaries)

Chronicle is organized into strict layers to keep the bus clean and the hot path stable.
Dependencies must flow downward only (higher layers may depend on lower layers; never the reverse).

Layer boundaries:
    - core (ULL bus): mmap segments, append protocol, reader offsets, wait strategy, retention.
    - bus (control helpers): layout conventions, discovery, readiness/lease markers.
    - protocol (stable schema): binary types, TypeId, versioning, flags.
    - ingest (adapters/importers): map third-party sources into protocol and append to queues.
    - stream (replay engines): L2/L3 state replay, trade table, feature extraction.
    - storage/tier (cold path): archive writing, compression, snapshots, read-only access.

API contracts:
    - core: MessageHeader.timestamp_ns is canonical ingest time for all messages.
    - protocol: backward-compatible evolution only; breaking changes require new TypeId or versioned struct.
    - ingest: must set MessageHeader.timestamp_ns and emit canonical protocol payloads only.
    - stream: ingest-time alignment for multi-stream replay (L2 book + trades).
    - bus/storage: must not affect hot-path determinism.

2.7 Migration Path (Keep Bus Clean)

Step 1: Document boundaries and contracts (this section).
Step 2: Gate heavy subsystems behind features so core + bus build cleanly.
Step 3: Keep protocol as a stable public contract with size/layout tests.
Step 4: Move domain-specific logic (adapters, replay, ETL) to stream/ingest modules.
Step 5: Consider crate split only after boundaries are enforced via features.

⸻

## 3. Core Architecture

### 3.1 Storage Format
*   **Segmented Files:** Data is split into fixed-size segments (e.g., 1GB).
*   **Memory Mapping:** Readers/Writers `mmap` segments.
*   **Zero-Copy:** Readers access `&[u8]` directly from the map.
*   **Format:** `[Header 64B] [Payload ...]` aligned to 64 bytes.

### 3.2 Latency Model (Signal Suppression)
To achieve ultra-low latency, we employ a "Signal Suppression" strategy using a "Waiter Flag + Check-After-Set" protocol.
*   **Happy Path:** Zero system calls. Writer writes to memory; Reader polls memory.
*   **Sleep Path:** Writer only syscalls (`futex_wake`) if `waiters_pending > 0`.
*   See **[DESIGN-latency.md](./DESIGN-latency.md)** for the formal proof of correctness and implementation details.

### 3.3 Control Plane (chronicle::bus)

⸻

4. Record Format and Memory Model (Normative)

4.1 Alignment
	•	HEADER_SIZE = 64
	•	RECORD_ALIGN = 64 (default)
	•	Record layout: [MessageHeader][payload bytes][padding]
	•	record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN)
	•	MAX_PAYLOAD_LEN = u32::MAX - 1 (commit_len encodes payload_len + 1)
	•	Zero-length payloads are valid and must be supported by codecs (payload_len = 0).

All record headers begin on a RECORD_ALIGN boundary.

4.2 Message header

use core::sync::atomic::AtomicU32;

#[repr(C, align(64))]
struct MessageHeader {
    /// Commit word:
    /// 0 = uncommitted
    /// >0 = committed payload length + 1 (allows zero-length payloads)
    commit_len: AtomicU32,

    seq: u64,
    timestamp_ns: u64,
    type_id: u16,
    flags: u16,

    // reserved / future use (checksum feature may repurpose this field)
    reserved_u32: u32,

    _pad: [u8; 64 - (4 + 8 + 8 + 2 + 2 + 4)],
}

Reserved type_id values (architecture):
	•	0xFFFF = PAD (internal padding record; readers must skip)

4.3 Publish/consume protocol (required discipline)

Writer sequence (Zero-Syscall "Memcpy" Path):
	1.	Reserve space: pos = write_offset.fetch_add(record_len, AcqRel)
	2.	Write header fields (except commit_len, which remains 0)
	3.	Write payload bytes (memcpy from stack/buffer to mmap region)
	4.	Publish commit: commit_len.store(payload_len + 1, Release)
	5.	Notify waiters (Section 5)

Writer sequence (Zero-Copy "In-Place" Path):
	*Optimization:* Instead of `memcpy`, the writer constructs the payload directly into the reserved mmap region.
	1.	Reserve space: pos = write_offset.fetch_add(record_len, AcqRel)
	2.	Obtain mutable pointer to payload area: `ptr = segment_base + pos + HEADER_SIZE`
	3.	Construct object in-place: `*ptr = Order { ... }`
	4.	Write header fields.
	5.	Publish commit.

Reader sequence:
	1.	Load commit: n = commit_len.load(Acquire)
	2.	If n == 0, the record at this offset is not committed → wait/backoff
	3.	If n > 0, payload_len = n - 1; read exactly payload_len bytes of payload
	4.	Advance offset by align_up(HEADER_SIZE + payload_len, RECORD_ALIGN)

Append validation (normative):
	•	If payload_len > MAX_PAYLOAD_LEN, append() returns Err(PayloadTooLarge).
	•	If record_len exceeds remaining space in the current segment and a roll is required but cannot succeed, append() returns an error from the roll step.

4.4 Memory ordering guarantee (explicit)

The following is guaranteed:
	•	The writer’s commit_len.store(payload_len + 1, Release) synchronizes-with a reader’s commit_len.load(Acquire) that observes n > 0.
	•	Therefore, all prior writes by the writer for that record (header fields and payload bytes) happen-before the reader’s subsequent reads of the payload.

Rule: Readers must never read payload bytes unless they have first observed commit_len > 0 via an Acquire load. This rule is part of the API contract.

4.5 Correctness invariants (normative)
	•	A record is committed iff commit_len.load(Acquire) > 0; payload_len = commit_len - 1.
	•	Readers must not read payload bytes unless they have observed commit_len > 0 with Acquire.
	•	Writer uses a single-writer append discipline; no two writers share a queue directory.
	•	Record boundaries are 64-byte aligned and computed from payload_len; readers must advance using align_up.
	•	Records with type_id = 0xFFFF (PAD) are internal and must be skipped by readers.
	•	Segment transitions are driven by boundary + SEALED checks; control.current_segment is a hint, not a correctness signal.
	•	Retention ignores dead readers (TTL exceeded); readers must heartbeat even when idle to remain live.

4.6 Message serialization policy (Hot path)

To maintain microsecond-level latency, the hot path (Market Data and Orders) avoids expensive serialization (JSON, Protobuf, Bincode).

*   **Zero-Copy POD:** Messages should be defined as `#[repr(C)]` or `#[repr(C, packed)]` structs.

4.7 BookEvent payload header (protocol v2)

*   **Header size:** `BookEventHeader` is 64 bytes and precedes the order book payload.
*   **Length field:** `record_len` is a `u32` that must equal `64 + payload_len`.
*   **Direct Casting:** Writers cast the struct memory directly to `&[u8]` for `append()`. Readers cast the `mmap` payload pointer back to the struct reference.
*   **Fixed Layout:** Prefer fixed-size fields (e.g., `u64`, `f64`) over variable-length strings or vectors where possible.
*   **No Allocation:** Payloads should be stack-allocated or pre-allocated; avoid heap allocations during the `append()` call.

Cold-path operations (Configuration, Periodic Snapshots) may use standard SerDes (e.g., `serde`).

⸻

5. Blocking Wait / Notification (Normative)

5.1 Motivation

Readers should block when no committed records are available without constant polling.

5.2 Shared control block (control.meta)

Each queue directory contains a small shared-mapped control.meta file:

use core::sync::atomic::{AtomicU32, AtomicU64};

const CTRL_MAGIC: u32 = 0x4348524E; // 'CHRN'
const CTRL_VERSION: u32 = 2;

#[repr(C, align(128))]
struct ControlBlock {
    // Constant / low-frequency fields (avoid sharing with hot fields).
    magic: AtomicU32,       // CTRL_MAGIC when initialized
    version: AtomicU32,     // CTRL_VERSION
    init_state: AtomicU32,  // 0=uninit, 1=initializing, 2=ready
    _pad0: [u8; 4],
    writer_epoch: AtomicU64, // increments on writer restart (optional)
    segment_size: AtomicU64, // bytes per segment file
    _pad1: [u8; 96],       // pad to 128 bytes

    // Reader-hot, rarely written (separate cache line).
    segment_gen: AtomicU32,   // seqlock for (current_segment, write_offset)
    current_segment: AtomicU32,
    _pad2: [u8; 120],       // pad to 128 bytes

    // Writer-hot (frequently written).
    write_offset: AtomicU64, // writer position within current segment (resets on roll)
    writer_heartbeat_ns: AtomicU64,
    notify_seq: AtomicU32,   // futex wait/wake target (queue-level)
    _pad3: [u8; 108],       // pad to 128 bytes
}

Segment size is stored in control.meta and is the single source of truth for queue geometry. Readers and writers must load it from the control block and ignore local defaults for existing queues. The segment_gen field is a seqlock generation counter used to publish a consistent (current_segment, write_offset) pair; readers should read it before/after and retry if it changes or is odd.

5.3 Initialization protocol (atomic and safe)

To avoid readers mapping partially initialized control blocks:

Publisher creates atomically:
	1.	Create control.meta.tmp, set file size
	2.	mmap and set init_state = 1 (initializing)
	3.	Initialize fields: version, segment_size, current_segment, write_offset, writer_epoch
	4.	Set magic = CTRL_MAGIC
	5.	Set init_state = 2 with Release
	6.	rename(control.meta.tmp → control.meta) (atomic)

Subscriber open:
	•	mmap(control.meta)
	•	spin/wait until init_state.load(Acquire) == 2
	•	verify magic == CTRL_MAGIC and version supported

5.4 Hybrid wait strategy

Default wait strategy is hybrid:
	•	Busy spin for SPIN_US (configurable; default 5–20µs)
	•	If still no data, futex wait on notify_seq with optional timeout

Writer notifies after each commit:
	•	notify_seq.fetch_add(1, Relaxed)
	•	futex_wake(notify_seq) (wake all waiters on this queue)

Scalability statement:
	•	Broadcast queues (market data) typically have many readers; waking them is expected.
	•	Fan-in queues are typically SPSC (router is sole reader per strategy queue), so no herd.
	•	For MVP, queue-level wake is acceptable; per-reader notification can be added later if needed.
	•	Eventfd can be layered on top (e.g., for epoll integration), but futex is the core MVP primitive.

⸻

6. Persistence Layout and Writer Exclusivity

6.1 Directory structure (single host)

/var/lib/hft_bus is an example root; the path must be configurable.

/var/lib/hft_bus/
└── orders/
    └── queue/
        ├── strategy_A/
        │   ├── orders_out/
        │   │   ├── READY
        │   │   ├── control.meta
        │   │   ├── writer.lock
        │   │   ├── 000000000.q
        │   │   ├── 000000001.q
        │   │   ├── index.meta           # optional tail checkpoint
        │   │   └── readers/
        │   │       └── router.meta
        │   └── orders_in/
        │       ├── READY
        │       ├── control.meta
        │       ├── writer.lock
        │       ├── 000000000.q
        │       └── readers/
        │           └── strategy_A.meta
        └── strategy_B/
            └── ...

6.2 writer.lock semantics (non-optional PID staleness check)

The writer holds an advisory lock on writer.lock. The file stores {pid, start_time_ticks, writer_epoch} for diagnostics, where start_time_ticks is the /proc/<pid>/stat starttime field (clock ticks since boot) captured when the lock is acquired.

On open_publisher():
	•	Attempt to acquire lock.
	•	If lock cannot be acquired:
	•	Read pid and start_time_ticks from lock file.
	•	Verify process existence via /proc/<pid> (Linux-only and mandatory for MVP).
	•	If /proc/<pid> exists, compare start_time_ticks against /proc/<pid>/stat starttime to avoid PID reuse.
	•	If /proc/<pid> does not exist or start_time mismatch, treat as stale and retry lock acquisition.
	•	Otherwise, return Err(WriterAlreadyActive).

Parsing note (Linux):
	•	/proc/<pid>/stat format contains comm in parentheses; fields after it are space-separated.
	•	starttime is the 22nd field in the full stat record (counting from pid as field 1).
	•	To parse reliably, read the line, find the last ')' (end of comm), then split the remainder by spaces.

File locks release automatically on process exit/crash; the PID+starttime check avoids false failures due to stale metadata or PID recycling edge cases.

⸻

7. Segment Files and Rolling (Normative)

7.1 Segment header and seal mechanism (chosen)

MVP uses a segment header flag to signal seal/end-of-segment.

At the start of every segment file:

const SEG_MAGIC: u32 = 0x53454730; // 'SEG0'
const SEG_VERSION: u32 = 1;
const SEG_FLAG_SEALED: u32 = 1;

#[repr(C)]
struct SegmentHeader {
    magic: u32,
    version: u32,
    segment_id: u32,
    flags: u32,          // bit0 = SEALED
    _pad: [u8; 48],      // total 64 bytes
}

Segment data begins at SEG_DATA_OFFSET = 64.

7.2 Writer rolling protocol (ordered)

Trigger: write_offset + record_len > segment_size (from control.meta).
	1.	Create and preallocate next segment file N+1.q (fallocate)
	2.	Write SegmentHeader for N+1 (flags=0)
	3.	Publish new segment to control block with a seqlock update:
	•	segment_gen += 1 (odd)
	•	current_segment.store(N+1, Relaxed)
	•	write_offset.store(SEG_DATA_OFFSET, Relaxed)
	•	segment_gen += 1 (even, Release)
	4.	Seal old segment N.q:
	•	set SEG_FLAG_SEALED in N’s segment header
	•	msync the page containing the segment header (best effort)
	5.	Notify waiters

Note: sealing after publishing is acceptable because readers only transition on seal + boundary checks (below). Seal is the definitive “no more records will be appended to this segment” signal.

7.3 Reader transition protocol (precise)

Readers track (segment_id, offset) and only transition when unable to progress in the current segment.

Definitions:
	•	SEG_END = segment_size (from control.meta)
	•	MIN_RECORD = HEADER_SIZE (payload may be zero)
	•	last_possible_record_boundary = SEG_END - MIN_RECORD
	•	If offset > last_possible_record_boundary, there is not enough room to store even a minimal record header, so no valid new record can start at or beyond this point.

Reader pseudocode:

loop:
  if offset > last_possible_record_boundary:
      // cannot start another record in this segment
      if segment_header.flags has SEALED and file_exists(segment_id + 1):
          remap to segment_id + 1
          offset = SEG_DATA_OFFSET
          continue
      wait()
      continue

  hdr = read MessageHeader at (segment_id, offset)
  n = hdr.commit_len.load(Acquire)

  if n > 0:
      payload_len = n - 1
      if hdr.type_id == 0xFFFF:
          // PAD: skip without surfacing to user
          offset += align_up(HEADER_SIZE + payload_len, RECORD_ALIGN)
          continue
      consume payload[0..payload_len]
      offset += align_up(HEADER_SIZE + payload_len, RECORD_ALIGN)
      continue

  // n == 0: no committed message here yet
  if segment SEALED and offset > last_possible_record_boundary:
      // handled by top-of-loop on next iteration
      continue

  wait() or backoff

This protocol is self-synchronizing and avoids depending on control.current_segment for correctness. control.current_segment is a hint for availability, not a correctness signal.

7.4 Recovery scan and tail repair (normative)

On open_publisher(), the writer must establish a safe append point:
	•	If index.meta is present, start from its {segment_id, offset} checkpoint; otherwise scan from SEG_DATA_OFFSET of the latest segment.
	•	Scan forward record-by-record while commit_len.load(Acquire) > 0 and payload_len <= MAX_PAYLOAD_LEN.
	•	The first record with commit_len == 0 or an invalid header is treated as the end-of-log.

Tail repair requirement:
	•	If the end-of-log offset is within a segment and there is space for a header, the writer MUST overwrite that position with a PAD record that spans the remaining bytes in the segment:
		•	type_id = 0xFFFF (PAD)
		•	payload_len = remaining_bytes - HEADER_SIZE
		•	commit_len = payload_len + 1 (Release)
		•	payload bytes may be zero-filled
	•	If there is not enough space for a header, the writer MUST seal the segment and roll.
	•	After PAD/seal, the writer sets write_offset to SEG_DATA_OFFSET of the next segment and proceeds with normal appends.

Rationale: Readers encountering a partially written record will either skip a committed PAD record or see a sealed segment boundary; they must never block indefinitely on uncommitted garbage.

⸻

8. Backpressure, Capacity, and Failure Behavior (Operational Contract)

8.1 Capacity limits

Each queue enforces a configured maximum storage budget:
	•	either MAX_BYTES or MAX_SEGMENTS

If accepting a new record would exceed the cap, append() returns Err(QueueFull) (fail-fast). The writer does not block indefinitely.

Operational guidance:
	•	Applications must monitor QueueFull and treat it as a circuit-breaker condition (alert, shed load, drop non-critical messages, etc.).
	•	QueueFull persists until readers advance and retention frees segments; retry/backoff policy is an application decision.

8.2 Disk full behavior

If fallocate() fails during segment creation (disk full or quota), writer returns an error from append() or roll step. This is treated as a hard fault; the system must alert.

⸻

9. Reader Liveness, Retention, and Dead Readers (Normative + Operational)

9.1 Reader meta fields

Each reader meta persists:
	•	committed position (segment_id, offset) or equivalent monotonic index
	•	last_heartbeat_ns

Readers update:
	•	committed position on commit()
	•	heartbeat every HEARTBEAT_INTERVAL (default 1s)

Meta updates must be atomic and corruption-detecting (recommended double-slot with generation + CRC).

9.2 Live reader definition (TTL)

A reader is live if:
	•	now_ns - last_heartbeat_ns <= READER_TTL_NS

Readers must heartbeat even when idle; otherwise they will be treated as dead and ignored for retention.

Defaults are workload dependent:
	•	market data: TTL ~ 10s
	•	orders/control: TTL ~ 30–60s
	•	audit/compliance: TTL ~ 300s

Lagging reader policy (MAX_RETENTION_LAG):
	•	A reader is considered retention-inactive if head_offset - committed_offset > MAX_RETENTION_LAG, even if it is heartbeating.
	•	This prevents stalled but alive processes from blocking retention indefinitely.
	•	MAX_RETENTION_LAG is configurable (e.g., 10 GiB or a fixed number of segments); exceeding it should be logged and surfaced to operators.

9.3 Retention eligibility

A segment is eligible for deletion when all live readers have committed positions beyond the segment’s range.

Dead/stale readers (beyond TTL) are ignored for retention. This prevents indefinite disk growth after crashed processes.

9.4 Clean shutdown deregistration

On clean shutdown, readers should delete their own meta file to avoid clutter. TTL remains necessary for crash safety.

9.5 Archival Sidecar (Long-Term Retention)

For compliance or backtesting, data often needs to be kept longer than the NVMe capacity allows. We use a **Tiered Storage** approach via an "Archival Strategy".

**Architecture:**
*   **Archiver Process:** A standard `QueueReader` that subscribes to critical feeds.
*   **Compression:** It reads raw segments, compresses them (e.g., `zstd`, `LZ4`), and writes them to a cheaper storage tier (HDD / S3 / NAS).
*   **Retention Interaction:** The NVMe retention policy treats the Archiver like any other reader. Once the Archiver has committed (and safely stored the data elsewhere), the raw NVMe segments are eligible for deletion.

**Benefit:**
*   **Zero Latency Impact:** The critical path (Feed -> Strategy) is unaffected by compression CPU cost or HDD I/O latency.
*   **Isolation:** If archiving stalls, only the cleanup is delayed; trading continues at full speed.

⸻

10. Durability Semantics (Normative)

Durability tiers are explicit:
	•	append():
	•	makes the record visible after commit
	•	survives process crash (data typically remains in page cache)
	•	no strict guarantee under power loss
	•	flush_async():
	•	calls msync(MS_ASYNC) on recent ranges
	•	reduces risk but no strict power-loss guarantee
	•	flush_sync():
	•	calls msync(MS_SYNC) on recent ranges
	•	aims for on-disk persistence of data pages
	•	metadata persistence may additionally require fdatasync() at roll/checkpoint boundaries
	•	segment roll must persist both the sealed segment header and control/meta state before reporting durability

Operational guidance:
	•	msync(MS_SYNC) can be millisecond-scale; batch flushes (e.g., every 100–1000 messages or every 10–50ms) rather than flushing every message.

⸻

11. API Design

11.1 chronicle::core API (data plane)

pub struct Queue;

pub struct QueueWriter;

pub struct QueueReader;

pub enum WaitStrategy {
    Hybrid { spin_us: u32 },         // spin then futex
    BusyPoll(std::time::Duration),   // no futex
}

pub enum QueueError {
    PayloadTooLarge,
    QueueFull,
    WriterAlreadyActive,
    UnsupportedVersion,
    CorruptMetadata,
    Io(std::io::Error),
}

Core operations:

impl Queue {
    pub fn open_publisher(path: impl AsRef<std::path::Path>) -> Result<QueueWriter>;
    pub fn open_subscriber(path: impl AsRef<std::path::Path>, reader: &str) -> Result<QueueReader>;
}

impl QueueWriter {
    pub fn append(&mut self, type_id: u16, payload: &[u8]) -> Result<()>;
    pub fn flush_async(&mut self) -> Result<()>;
    pub fn flush_sync(&mut self) -> Result<()>;
}

pub struct MessageView<'a> {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub payload: &'a [u8],
}

impl QueueReader {
    pub fn next(&mut self) -> Result<Option<MessageView<'_>>>;
    pub fn commit(&mut self) -> Result<()>;
    pub fn wait(&self, timeout: Option<std::time::Duration>) -> Result<()>;
}

11.3 Error semantics (architecture contract)
	•	PayloadTooLarge: payload_len > MAX_PAYLOAD_LEN; no partial write occurs.
	•	QueueFull: configured capacity would be exceeded; no partial write occurs.
	•	WriterAlreadyActive: writer.lock is held by a live writer (PID + start_time_ticks check).
	•	UnsupportedVersion: control/segment format version mismatch.
	•	CorruptMetadata: failed CRC or invalid meta structure; recovery may be required.
	•	Io: underlying OS or filesystem error (open/mmap/fallocate/msync/fdatasync).
	•	Errors are fail-fast; callers decide retry/backoff/drop policy.

11.2 chronicle::bus API (control plane helper; intentionally thin)

11.2.1 Layout and endpoints
The IPC layout is standardized by `chronicle::layout`. Control-plane helpers
(READY/LEASE) live in `chronicle::bus`.

pub struct IpcLayout {
    root: std::path::PathBuf, // e.g. /var/lib/hft_bus
}

impl IpcLayout {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self;
    pub fn streams(&self) -> StreamsLayout;
    pub fn orders(&self) -> OrdersLayout;
}

pub struct StreamsLayout {
    root: std::path::PathBuf,
}

impl StreamsLayout {
    pub fn raw_queue_dir(&self, venue: &str) -> Result<std::path::PathBuf>;
    pub fn clean_queue_dir(&self, venue: &str, symbol: &str, stream: &str) -> Result<std::path::PathBuf>;
}

pub struct StrategyId(pub String);

pub struct StrategyEndpoints {
    pub orders_out: std::path::PathBuf, // strategy -> router
    pub orders_in: std::path::PathBuf,  // router -> strategy
}

pub struct OrdersLayout {
    root: std::path::PathBuf,
}

impl OrdersLayout {
    pub fn strategy_endpoints(&self, id: &StrategyId) -> Result<StrategyEndpoints>;
}

pub fn mark_ready(endpoint_dir: &std::path::Path) -> std::io::Result<()>;
pub fn write_lease(endpoint_dir: &std::path::Path, payload: &[u8]) -> std::io::Result<()>;

READY semantics:
	•	Strategy/router writes READY.tmp then rename() to READY atomically once initialization is complete.

LEASE semantics (optional but recommended):
	•	Process periodically updates LEASE content with {pid, epoch, timestamp_ns}; used for diagnostics and policy.

11.2.2 RAII registration (for cleanup, not orchestration)
The bus helper provides a small RAII handle that:
	•	creates reader meta directory if needed
	•	registers heartbeat scheduling hooks (caller still drives the loop)
	•	deletes meta on clean shutdown (best-effort)

pub struct ReaderRegistration {
    reader_name: String,
    meta_path: std::path::PathBuf,
}

impl ReaderRegistration {
    pub fn register(queue_dir: &std::path::Path, reader_name: &str) -> std::io::Result<Self>;
    pub fn meta_path(&self) -> &std::path::Path;
}

impl Drop for ReaderRegistration {
    fn drop(&mut self) {
        // best-effort: delete meta file on clean shutdown
        // (TTL still required for crash safety)
    }
}

This remains thin: it does not start threads, does not own event loops, and does not supervise processes.

11.2.3 Router discovery helper (scan + watch + dedup)

pub struct RouterDiscovery;

pub enum DiscoveryEvent {
    Added { strategy: StrategyId, orders_out: std::path::PathBuf },
    Removed { strategy: StrategyId },
}

impl RouterDiscovery {
    pub fn new(layout: OrdersLayout) -> std::io::Result<Self>;

    /// Returns newly added/removed strategy queues since the last poll.
    pub fn poll(&mut self) -> std::io::Result<Vec<DiscoveryEvent>>;
}

Implementation notes:
	•	Use a filesystem watcher library plus an initial scan.
	•	Deduplicate scan results vs watch events.
	•	On removal, callers must drop associated QueueReader to release mmaps/FDs.
	•	Discovery is best-effort; missed events are recovered by periodic rescan.

⸻

12. Multi-Process HFT Topology (Single Host)

```text
┌─────────────────────────────────────────────────────────────┐
│                    Market Data Layer                         │
├─────────────────────────────────────────────────────────────┤
│  Binance Feed Process          Coinbase Feed Process         │
│  ├─ WS: 100 symbols           ├─ WS: 80 symbols             │
│  └─ Writes to:                └─ Writes to:                 │
│     streams/raw/binance_spot/queue/  streams/raw/coinbase_pro/queue/ │
└─────────────────────────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                    Strategy Layer                            │
├─────────────────────────────────────────────────────────────┤
│  Strategy A (Arb)              Strategy B (Market Making)    │
│  ├─ Reads: streams/raw/binance_spot  ├─ Reads: streams/raw/binance_spot │
│  ├─ Filters: BTC,ETH,SOL      ├─ Filters: BTC,ETH           │
│  └─ Writes to:                └─ Writes to:                 │
│     orders/queue/strategy_A/     orders/queue/strategy_B/   │
└─────────────────────────────────────────────────────────────┘
                         │
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                    Router Layer                              │
├─────────────────────────────────────────────────────────────┤
│  Router Process                                              │
│  ├─ Discovery: Finds strategy_A, strategy_B queues          │
│  ├─ Reads: orders/queue/strategy_*/orders_out               │
│  ├─ Aggregates & routes to exchange                         │
│  └─ Writes fills back to: orders/queue/strategy_*/orders_in │
└─────────────────────────────────────────────────────────────┘
```

12.1 Market feed → strategies (broadcast)
	•	Feed process: single writer to per-feed queue(s)
	•	Strategies: many readers with independent offsets

12.2 Strategies → router (fan-in)
	•	Each strategy writes to its own orders_out queue
	•	Router discovers and attaches to all orders_out queues, merges

12.3 Router → strategies (ack/fill/control)
	•	Router writes to each strategy’s orders_in
	•	Each strategy reads its own orders_in

12.4 Market Data Subscription Models (Broadcast Guidance)

For high-symbol-count feeds (e.g., Crypto with 100-500 symbols), the following model is recommended:

**Model A: Single Queue, Client-Side Filtering**

The feed process writes all configured symbols to a single queue. Strategies read the entire stream and filter by `symbol_id` (or similar field in the payload) on the fly.

*   **Pros:**
    *   **Stateless Feed:** The feed process doesn't need to know which strategy wants which symbol.
    *   **Write Latency:** Optimal sequential writes to a single set of memory-mapped segments.
    *   **Deterministic Replay:** All strategies observe the exact same sequence of market events across all symbols, preserving global temporal order.
*   **Cons:**
    *   **Read Overhead:** Strategies ingest messages they might eventually skip. However, with `mmap` and 64-byte headers, skipping a message is typically a ~1µs operation (pointer increment after a cache-friendly header check).

**Guidance:** Use Model A for most Crypto and Equity use cases where symbol counts are in the hundreds. Per-symbol queues should only be considered if symbol counts reach thousands or if hardware-level isolation is strictly required.

12.5 Snapshot + Replay Pattern (Late Joiners)

To handle L2 Orderbook initialization without blocking the main feed, use the **Periodic Snapshot** pattern.

**Architecture:**
*   **Hot Path (`queue/`):** Feed writes incremental updates (Diffs). This path never pauses.
*   **Cold Path (`snapshots/`):** Feed periodically serializes full book state to a separate directory (e.g., every 10s or 10k messages).

**Workflow:**
1.  **Feed Process:** Writes updates to `queue/`. Background thread writes `snapshots/snapshot_<seq>.bin`.
2.  **Strategy (Late Joiner):**
    *   Finds latest snapshot (e.g., `snapshot_2000.bin`).
    *   Loads full book state (Seq 2000).
    *   Opens `queue/` and seeks to Seq 2001.
    *   Replays updates from Seq 2001 to Head.

This ensures the critical path (Feed writing updates) is decoupled from heavy state serialization and strategy restarts.

12.6 Discovery Mechanisms

The system employs a hybrid discovery model: **Static** for critical infrastructure, **Dynamic** for strategy scaling.

*   **Market Feeds (Upstream): Static Configuration.**
    *   Since Market Data defines the "world view," it is strictly defined in configuration (e.g., `router.toml`).
    *   The Router expects specific queues (e.g., `binance_spot`) to exist on startup and will fail-fast if they are missing.
*   **Strategies (Downstream): Dynamic Discovery.**
    *   The Router uses a **"Scan + Watch"** mechanism on the `orders/` directory to automatically route orders from new strategies.
    *   **Mechanism:** `readdir` on startup + `inotify` (Linux) or `kqueue` (macOS) for runtime events.
    *   **Robustness:** Relies on the filesystem as the source of truth. Strategies are only discovered when their `READY` marker is atomically visible.

12.7 Process Architecture & Management

The system is strictly **Multi-Process**. Each component (Feed, Strategy, Router) runs in its own OS process to ensure performance isolation and crash resilience. IPC is handled exclusively via `chronicle::core` queues (shared memory).

**Process Management:**
Since `chronicle-rs` does not handle supervision (Section 1 "Non-goals"), external tools must be used.

*   **Production:** `systemd` (Recommended).
    *   Use unit files (e.g., `hft-feed.service`) for robust start/stop/restart policies and logging (`journalctl`).
    *   Dependencies can be managed via `Requires=` (e.g., ensure bus directory exists).
*   **Development:** `tmux`, `overmind` (Procfile), or shell scripts.
    *   Focus on visibility and easy termination (`Ctrl+C`).

**Do not** build process supervision or orchestration logic into the Rust binaries.

12.8 Threading Model (Sidecar Pattern)

To maintain deterministic latency, applications must separate the Data Plane (Hot Path) from the Control Plane (Cold Path).

*   **Hot Thread (Pinned):** Performs no system calls (no `open`, `stat`, or `malloc`). Owns the `FanInReader` and `QueueWriter`.
*   **Sidecar Thread (Background):** Handles file discovery, logging, and heavy initialization.
*   **Handoff:** The Sidecar opens new queues and passes fully initialized `QueueReader` objects to the Hot Thread via a lock-free SPSC queue.

See **[DESIGN-patterns.md](./DESIGN-patterns.md)** for implementation details.

⸻

13. Workspace / Modules

chronicle-rs/
├── src/
│   ├── lib.rs
│   ├── core/
│   │   ├── control.rs
│   │   ├── header.rs
│   │   ├── segment.rs
│   │   ├── writer.rs
│   │   ├── reader.rs
│   │   ├── retention.rs
│   │   └── wait.rs
│   ├── protocol/
│   ├── layout/
│   ├── bus/
│   └── storage/
├── examples/
├── tests/
├── benches/
└── docs/
    └── DESIGN.md


⸻

14. Tooling / CLI (chronicle-cli)

To manage and inspect the bus state (which `systemd` cannot see), a dedicated CLI tool is required. This tool interacts directly with the on-disk structures.

**Key Commands:**
*   `chron-cli inspect <queue_path>`:
    *   Displays writer position (current segment/offset).
    *   Lists all registered readers and their lag (bytes/segments behind).
    *   Checks process liveness (PID/lock validation).
*   `chron-cli tail <queue_path> [-f]`:
    *   Prints message headers (seq, timestamp, type) and hexdumps payloads.
    *   Essential for debugging data flow verification.
*   `chron-cli doctor <bus_root>`:
    *   Identifies stale lock files from crashed processes.
    *   Reports on retention candidates (segments eligible for deletion).
*   `chron-cli bench`:
    *   Runs a throughput/latency test on the local filesystem to verify `mmap` performance.

⸻

15. Testing Requirements (MVP)
	•	Memory model invariants:
	•	Reader never reads payload without observing commit (debug assertions)
	•	Crash recovery:
	•	kill -9 writer at random points; readers recover, writer restarts and scans tail
	•	Segment rolling under load:
	•	read/write concurrently through roll boundaries
	•	Futex correctness:
	•	timeouts wake; writer crash does not deadlock readers
	•	Retention and dead readers:
	•	stale reader meta does not block cleanup after TTL
	•	Disk full:
	•	fallocate() failure surfaces promptly and predictably
