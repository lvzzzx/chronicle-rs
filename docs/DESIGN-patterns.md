# Architectural Patterns

This document outlines standard architectural patterns for building Ultra-Low Latency (ULL) applications using `chronicle-rs`.

## The "Sidecar Thread" Pattern

In ULL systems, we rigorously separate **Data Plane** (Hot Path) logic from **Control Plane** (Cold Path) logic. Mixing them introduces non-deterministic latency spikes ("jitter") due to system calls, I/O blocking, and cache pollution.

### The Problem

If a trading loop performs IO (e.g., checking for new files, writing logs to disk, allocating memory) within the critical path, it risks blocking for unpredictable durations.
*   `fs::read_dir()` can take 10µs or 10ms depending on disk contention.
*   `malloc()` can trigger lock contention or page faults.

### The Solution

Split the application into at least two threads:

#### 1. The Hot Thread (Pinned)
*   **Role:** Critical Path processing (Trading, Routing, Risk Checks).
*   **Affinity:** Pinned to an isolated CPU core (`isolcpus`).
*   **Constraints:**
    *   **Minimal Syscalls:** Avoid `open`, `stat`, or `write` (logging) in the loop. `futex_wait` is permitted only when idle.
    *   **Minimal Allocations:** Ideally zero, but note that the current `FanInReader` implementation performs heap allocations (`Vec<u8>`) for merged messages. A future zero-copy version is planned.
    *   **Exclusive Access:** Owns the `FanInReader` and `QueueWriter` (passed by value).
*   **Loop:**
    1.  Check **Command Channel** (non-blocking) for updates from Sidecar.
    2.  Wait/Poll Data (`FanInReader::wait`). This handles liveness heartbeats internally.
    3.  Process Data.

#### 2. The Sidecar Thread (Background)
*   **Role:** Management, Discovery, Logging, housekeeping.
*   **Affinity:** Shared core (OS scheduled).
*   **Responsibilities:**
    *   **Discovery:** Periodically scans directories for new streams/strategies.
    *   **Setup:** Opens files, maps memory, verifies headers (Heavy lifting).
    *   **Handoff:** Passes **ownership** of the fully initialized `QueueReader` to the Hot Thread.
    *   **Logging:** Drains a lock-free log ring buffer from the Hot Thread and writes to disk/network.

### Implementation Reference

```rust
// Shared Channel: Sidecar -> Hot
// Note: QueueReader is Send, so we can move it across threads.
let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(16);

// --- Thread 2: Sidecar (Control Plane) ---
thread::spawn(move || {
    loop {
        // Heavy IO operation
        // API Note: Requires reader name
        if let Ok(new_reader) = Queue::open_subscriber("/path/to/new/stream", "my_router") {
            // Handoff: Send fully ready object (Ownership Transfer)
            let _ = cmd_tx.send(Command::AddReader(new_reader));
        }
        thread::sleep(Duration::from_secs(1));
    }
});

// --- Thread 1: Hot Thread (Data Plane) ---
// Pin to Core 1...
loop {
    // 1. Check Control Plane (Fast, Non-blocking)
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::AddReader(reader) => fanin.add_reader(reader),
            Command::Stop => break,
        }
    }

    // 2. Critical Path
    // wait() internally updates heartbeats to keep readers live.
    fanin.wait(); 
    while let Some(msg) = fanin.next() {
        // Warning: msg.payload is currently a Vec<u8> (allocation)
        process(msg);
    }
}
```

---

## Review Notes (Concerns / Clarifications)

This section captures design concerns to revisit later. It is not prescriptive guidance.

1. **Hot Thread Syscalls vs `fanin.wait()`**
   * If `fanin.wait()` performs syscalls (eventfd/epoll/clock), clarify that it is only allowed when the hot loop is idle and intentionally blocking.
   * Otherwise the "no syscalls on hot path" rule is contradicted by the example loop.

2. **Control-Plane Channel Contention**
   * The example uses a bounded channel, which is good for backpressure.
   * If there are multiple sidecar producers, contention can bleed into hot-path polling. Consider noting "single-producer preferred" or the tradeoff.

3. **Sidecar Cadence**
   * `sleep(Duration::from_secs(1))` implies up to 1s discovery latency.
   * Consider making the cadence configurable or referencing event-driven mechanisms (e.g., inotify) to set expectations.

4. **Prealloc Durability Semantics**
   * "Publish via rename/link" does not specify durability.
   * If crash-safety is required, note the fsync sequence (file, rename, directory) or explicitly say durability is out of scope.

5. **Retention Atomics / Ordering**
   * Examples use `Relaxed` loads for head/offset.
   * If approximate values are acceptable, say so; otherwise document the required ordering to avoid accidental over-deletion.

## Sidecar Control Plane with Dedicated Workers

When the control plane grows (discovery + retention + preallocation + logging), a single sidecar thread can become a bottleneck. For strict ULL systems, split the control plane into **dedicated workers** so a slow task cannot delay preallocation or interfere with roll behavior.

### What "Worker" Means
A **worker** is a dedicated background execution unit (typically a single OS thread) that owns exactly one cold-path responsibility and processes a bounded queue of requests. The hot path never waits on a worker.

### Recommended Worker Split

1. **Prealloc Worker (highest priority)**
   * **Role:** Prepare the *next* segment ahead of time.
   * **Safety rules:**
     * Never touch the live segment file path.
     * Preallocate into a temp file, then publish via rename/link.
     * Attach a `segment_id` to the prepared mmap and only consume if it matches.
     * Discard stale results silently.
   * **Why:** Prevents roll stalls and avoids data corruption under high roll rates.

2. **Discovery Worker (lower priority)**
   * **Role:** Scan for new streams/readers, open and validate them.
   * **Handoff:** Send fully initialized readers to the hot thread via a bounded channel.

3. **Retention Worker (lower priority)**
   * **Role:** Periodic cleanup of old segments using `retention::cleanup_segments`.
   * **Constraint:** Never touch the active head segment.

### Control Plane Topology

* **Hot Thread:** append + roll only; no blocking I/O.
* **Workers:** each runs on a shared core with no affinity requirements.
* **Queues:** bounded; prealloc queue depth is 1-2 to cap work.

### Minimal Prealloc Worker Skeleton

```rust
struct PreparedSegment {
    segment_id: u64,
    mmap: MmapFile,
}

// Sidecar worker: prealloc only
thread::spawn(move || {
    while let Ok(req) = prealloc_rx.recv() {
        let temp_path = segment_temp_path(&root, req.segment_id);
        if let Ok(mmap) = prepare_segment(&root, req.segment_id, size) {
            if expected_next_id.load(Ordering::Acquire) == req.segment_id {
                let _ = publish_segment(&temp_path, &segment_path(&root, req.segment_id));
                let _ = prepared_slot.swap(Some(PreparedSegment { segment_id: req.segment_id, mmap }));
            }
        }
        // stale or failed work is dropped
    }
});
```

### Integration Notes for This Codebase

* **Avoid truncating published segments:** If preallocation publishes a fully prepared file, the writer must try `open_segment` first. Calling `create_segment` (which truncates) defeats preallocation and reintroduces `ftruncate` on the hot path. Only fall back to `create_segment` when the file truly does not exist.
* **Verify identity before use:** The prepared mmap must carry the `segment_id`, and the writer should validate the header before swapping (or rely on `open_segment`'s header checks).
* **Retention can still block on directory locks:** `unlink` and `rename` contend on the directory inode. If retention deletes large batches, it can stall `open`/`rename` during roll. Consider batching deletes, yielding between unlinks, or renaming into a trash directory and deleting later.

### Applicability

* **ULL Trading Loops:** prealloc worker isolates roll latency from discovery or retention jitter.
* **Routers/Strategy Hosts:** discovery can block without affecting the writer.

## Async Cleanup (Retention) Pattern

To avoid filesystem latency spikes (metadata scans, file deletion) on the Hot Path, strict ULL systems must **disable** built-in limits in the Writer and offload retention to a background thread.

### The Problem
If `WriterConfig` has `max_bytes` or `max_segments` set, `append()` performs periodic directory scans (`fs::read_dir`) to check usage. This introduces non-deterministic latency spikes (jitter) every ~10ms.

### The Solution

1.  **Hot Thread:** Configure `QueueWriter` with **no limits**.
    ```rust
    let config = WriterConfig {
        max_bytes: None,    // Disable inline checks
        max_segments: None, // Disable inline checks
        ..Default::default()
    };
    let mut writer = Queue::open_publisher_with_config(&path, config)?;
    ```

2.  **Sidecar Thread:** Run a cleanup loop.
    ```rust
    use chronicle_core::retention::cleanup_segments;
    
    // In Sidecar Thread
    thread::spawn(move || {
        loop {
            // "Soft Cap" logic: Keep last 100 segments (~10GB)
            // This scans disk and deletes files WITHOUT blocking the writer.
            // Note: Use a separate handle or raw retention function to avoid lock contention if applicable.
            // Here we use the raw utility function which is safe to run concurrently.
            
            // Get current head from shared state or by inspecting the directory (less precise but safe)
            // Ideally, the Hot Thread publishes its 'current_segment' to an atomic for the Sidecar to read.
            let head_segment = shared_control.current_segment.load(Ordering::Relaxed) as u64;
            let head_offset = shared_control.write_offset.load(Ordering::Relaxed);
            let segment_size = shared_control.segment_size.load(Ordering::Relaxed);

            let _ = cleanup_segments(
                &queue_path, 
                head_segment, 
                head_offset, 
                segment_size
            );
            
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    ```

### Applicability

*   **Strategies:** Use Sidecar to discover dynamic Order Entry gateways or listen for risk parameter updates.
*   **Routers:** Use Sidecar to discover new Strategy processes starting up and attaching to the bus.
*   **Feed Handlers:** Use Sidecar to manage socket reconnections while the Hot Thread processes the ring buffer from the NIC.

## Passive Discovery & Service Resilience

In Ultra Low Latency (ULL) systems, components must decouple their lifecycle from their dependencies. We employ a **Passive Discovery** pattern to handle startup order and runtime failures gracefully.

### 1. The Pattern
Consumers (Readers) do not assume Producers (Writers) exist at startup. Instead, they operate a Finite State Machine (FSM):

1.  **Initializing:** Load configuration.
2.  **Connecting (Passive Wait):** Periodically poll (e.g., 500ms) for the shared memory artifacts (Control File, Writer Lock).
    *   *Constraint:* This polling occurs on a "slow path" (separate thread or blocked state) and must never interfere with the hot path once connected.
3.  **Connected (Hot Path):** Bind to the memory-mapped files and consume data using busy-spin or hybrid wait strategies. Zero syscalls allowed here.
4.  **Disconnected (Failure):** If the transport is lost (file access error, writer heartbeat missing), degrade back to the **Connecting** state.

### Scope Split: What Lives Here vs. In Trading Systems

We intentionally split **primitives** (in `chronicle-core` / `chronicle-bus`) from **policy** (in the trading system). This keeps the message framework ULL-safe and reusable across many strategies.

**Why this split:**

* **Hot-path integrity:** Discovery/recovery loops are I/O-heavy and must stay off the hot path. The core should never force blocking behavior.
* **Policy varies by shop:** Actions like **Cancel All**, **Flatten**, or specific retry cadences are risk-policy decisions and must stay in the application.
* **Minimal, composable primitives:** The framework should expose liveness signals and errors, not make trading decisions.
* **Testability:** Clear separation makes state-machine behavior testable without coupling to risk logic.

**What the framework provides (primitives):**

* Liveness / heartbeat checks.
* Errors or reasons for disconnect (writer lock lost, heartbeat stale, missing segment).
* Optional helper to poll discovery and emit state transitions **without** taking safety actions.

**What the trading system provides (policy):**

* FSM ownership and state transitions.
* Safety actions (Cancel All / Flatten).
* Retry cadence and escalation logic.

### Suggested Module Split and APIs

**`chronicle-core` (signals only):**

* `QueueReader::writer_status(ttl) -> WriterStatus`
* `QueueReader::detect_disconnect(ttl) -> Option<DisconnectReason>`
* `DisconnectReason` enum (e.g., lock lost, heartbeat stale, segment missing)

**`chronicle-bus` (optional helper, cold path):**

* `PassiveDiscovery` helper that polls directory readiness + attempts open.
* Emits `PassiveEvent::{Connected(QueueReader), Disconnected(DisconnectReason), Waiting}`
* Does **not** perform safety actions or block the hot path.

**Trading System (policy):**

* Owns the FSM and performs safety actions on disconnect.
* Chooses cadence (fixed sleep, inotify, exponential backoff, etc.).
* Pins hot thread and chooses wait strategy (busy spin or hybrid).

### 2. Usage by Component Type

Different components react differently to the `Disconnected` -> `Connecting` transition.

#### A. Passive Tools (Monitors, Loggers, Archivers)
*   **Goal:** Observability.
*   **Behavior:** Simply display/log a "Waiting for Source" status.
*   **Data Consistency:** It is acceptable to visualize "gaps" or simply resume from the latest data point.

#### B. Trading Strategies (Critical Path)
*   **Goal:** Profit & Safety.
*   **Behavior:**
    *   **Startup:** Start in `WAITING_FOR_FEED`. Do not crash if Feed Handler is down.
    *   **On Disconnect:** **IMMEDIATE SAFETY ACTION REQUIRED.**
        1.  **Mass Cancel:** Send `Cancel All` to execution gateways. (You are blind to the market).
        2.  **Flatten:** Optionally close positions if risk limits dictate.
        3.  Enter `WAITING_FOR_FEED` state.
    *   **On Reconnect:**
        1.  **Gap Detection:** Check if `sequence` numbers are contiguous.
        2.  **State Rebuild:** If a gap is detected or the Writer epoch changed, **discard** live delta updates.
        3.  **Snapshot:** Wait for a Market Snapshot message to rebuild the internal Order Book.
        4.  Resume `TRADING` state.

### 3. Implementation Example (Rust)

```rust
// Simplified FSM for a Strategy
enum State {
    Initializing,
    WaitingForFeed,
    Trading(QueueReader),
    SafetyShutdown,
}

loop {
    match state {
        State::WaitingForFeed => {
             match Queue::open_subscriber(...) {
                 Ok(reader) => state = State::Trading(reader),
                 Err(_) => thread::sleep(Duration::from_millis(500)),
             }
        }
        State::Trading(mut reader) => {
             match reader.next() {
                 Ok(Some(msg)) => process(msg),
                 Ok(None) => {}, // Busy spin
                 Err(e) => {
                     // CRITICAL: Feed lost!
                     send_cancel_all(); 
                     state = State::WaitingForFeed; 
                 }
             }
        }
    }
}
```
