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
