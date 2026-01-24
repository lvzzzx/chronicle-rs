# Phase 13: CLI Latency Monitor ("The Auditor")

## 1. Objective
Implement a `monitor` command in `chronicle-cli` that acts as a passive "Auditor" sidecar. It will tail a queue in real-time, calculating and visualizing Inter-Process Latency (Writer -> Monitor) to detect system jitter and performance degradation.

## 2. Design Principles
*   **Zero-Copy Reading:** Use `chronicle-core`'s standard zero-copy path to minimize the monitor's own overhead.
*   **High-Fidelity Statistics:** Use `HdrHistogram` to capture true p99/Max outliers without averaging artifacts.
*   **Visual Feedback:** A TUI (Terminal User Interface) providing immediate "at-a-glance" health status.
*   **Clock Synchronization:** Use `QuantaClock` to match the writer's timing characteristics.
    *   *Note:* Inter-process latency measurement via wall-clock anchors has a margin of error equivalent to `SystemTime` precision (~20-100ns) plus NTP drift between the process start times. This is acceptable for detecting typical HFT outliers (1µs - 1ms).

## 3. Architecture

### 3.1. CLI Command
```bash
chronicle-cli monitor <QUEUE_PATH> [--interval <MS>]
```

### 3.2. Components
1.  **Sampler Loop:**
    *   Hot loop draining the queue.
    *   Calculates `delta = QuantaClock::now() - Message::timestamp`.
    *   Records `delta` into an `HdrHistogram`.
2.  **Aggregator:**
    *   Runs on a timer (default 100ms).
    *   Snapshots the histogram.
    *   Resets the histogram (interval recording) to show *instantaneous* latency, not lifetime average.
3.  **Renderer (TUI):**
    *   Uses `ratatui` + `crossterm`.
    *   **Layout:**
        *   **Header:** Queue Path, Message Rate (msg/s), Throughput (MB/s).
        *   **Latency Stats:** Min, p50, p90, p99, p99.9, p99.99, Max.
        *   **Histogram/Bar Chart:** Visual distribution of the last interval.

## 4. Implementation Details

### 4.1. Dependencies
Add to `crates/4-app/chronicle-cli/Cargo.toml`:
*   `hdrhistogram`: For statistical accumulation.
*   `ratatui`: For TUI rendering.
*   `crossterm`: For terminal manipulation.
*   `quanta`: For timing.

### 4.2. Logic Flow
```rust
let clock = QuantaClock::new();
let mut reader = Queue::open_subscriber(path)?;
let mut hist = HdrHistogram::<u64>::new(3).unwrap(); // 3 sig figs

loop {
    // 1. Drain Queue (up to batch limit)
    while let Some(msg) = reader.next() {
        let now = clock.now();
        // Handle clock skew (if writer started before reader with slight drift) by saturating sub
        let latency = now.saturating_sub(msg.timestamp_ns); 
        hist.record(latency)?;
    }
    
    // 2. Check Ticker
    if last_render.elapsed() > update_interval {
        render_tui(&hist, ...);
        hist.reset(); // Clear for next frame
    }
    
    // 3. Yield/Sleep strategies
    // Use busy-spin or hybrid wait based on expected throughput
}
```

## 5. Verification Plan
*   **Unit Tests:** Verify histogram recording logic.
*   **Integration:**
    *   Spawn a `stress_writer` (from benchmarks or tests) sending messages.
    *   Run `chronicle-cli monitor` to verify it displays plausible non-zero stats.

## 6. Future Enhancements
*   **Log to File:** `--log-csv` to save stats for post-mortem.
*   **Alerting:** Flash screen red if p99 > Threshold.
