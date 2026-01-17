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
    *   **Zero Syscalls:** No `File` IO, no sockets (unless using kernel-bypass), no `sleep` (except optimized `futex_wait`).
    *   **Zero Allocations:** No `Vec::push`, `String::new`. All memory pre-allocated.
    *   **Exclusive Access:** Owns the `FanInReader` and `QueueWriter`.
*   **Loop:**
    1.  Check **Command Channel** (non-blocking) for updates from Sidecar.
    2.  Wait/Poll Data (`FanInReader::wait`).
    3.  Process Data.

#### 2. The Sidecar Thread (Background)
*   **Role:** Management, Discovery, Logging, housekeeping.
*   **Affinity:** Shared core (OS scheduled).
*   **Responsibilities:**
    *   **Discovery:** Periodically scans directories for new streams/strategies.
    *   **Setup:** Opens files, maps memory, verifies headers (Heavy lifting).
    *   **Handoff:** Passes fully initialized objects (`QueueReader`) to the Hot Thread.
    *   **Logging:** Drains a lock-free log ring buffer from the Hot Thread and writes to disk/network.

### Implementation Reference

```rust
// Shared Channel: Sidecar -> Hot
let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(16);

// --- Thread 2: Sidecar (Control Plane) ---
thread::spawn(move || {
    loop {
        // Heavy IO operation
        if let Ok(new_reader) = Queue::open_subscriber("/path/to/new/stream") {
            // Handoff: Send fully ready object
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

    // 2. Critical Path (Zero Syscall)
    fanin.wait(); // BusySpin or Hybrid
    while let Some(msg) = fanin.next() {
        process(msg);
    }
}
```

### Applicability

*   **Strategies:** Use Sidecar to discover dynamic Order Entry gateways or listen for risk parameter updates.
*   **Routers:** Use Sidecar to discover new Strategy processes starting up and attaching to the bus.
*   **Feed Handlers:** Use Sidecar to manage socket reconnections while the Hot Thread processes the ring buffer from the NIC.
