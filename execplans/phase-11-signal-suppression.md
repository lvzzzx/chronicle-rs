# Phase 11: Signal Suppression (Latency Optimization)

## Problem Statement
The current implementation suffers from the **"Blind Wake"** inefficiency. The Writer calls `futex_wake` (a system call) on every single message append.
*   **Cost:** ~50-100ns per message (Userspace -> Kernel -> Userspace).
*   **Impact:** Limits max throughput and introduces latency jitter in the hot path.
*   **Goal:** The Writer must remain in userspace (zero syscalls) unless a Reader is explicitly sleeping and requesting a wake-up.

## Technical Approach
We will implement the **"Signal Suppression"** pattern (also known as the "Waiter Flag" optimization).

### 1. Control Plane (`control.rs`)
Modify `ControlBlock` to include a `waiters_pending` atomic counter.

```rust
pub struct ControlBlock {
    // ... existing fields
    pub notify_seq: AtomicU32,
    pub waiters_pending: AtomicU32, // NEW: 1 = someone is sleeping, 0 = all active
    // ... padding
}
```

### 2. Writer (`writer.rs`)
Update `append()` to check `waiters_pending` before waking.

```rust
// Pseudo-code
self.store_commit_len();
// Optimization: Relaxed load is sufficient. If we miss a race, the reader 
// has a timeout/check-loop to recover.
if self.control.waiters_pending.load(Ordering::Relaxed) > 0 {
    futex_wake(self.control.notify_seq());
}
```

### 3. Reader (`reader.rs`)
Update `WaitStrategy` to register presence before sleeping. This requires a "Check-After-Set" race protection ceremony.

```rust
// Pseudo-code for wait()
self.control.waiters_pending.fetch_add(1, Ordering::SeqCst);

// CRITICAL: Double-check data availability after flagging we are asleep.
// This handles the race where Writer wrote *just* before we incremented.
if self.peek_committed() {
    self.control.waiters_pending.fetch_sub(1, Ordering::SeqCst);
    return; // Data found, abort sleep
}

// Sleep
futex_wait(self.control.notify_seq(), expected_seq);

// Cleanup
self.control.waiters_pending.fetch_sub(1, Ordering::SeqCst);
```

### 4. Fan-In Reader (`merge.rs`)
**Challenge:** Standard futexes cannot "Wait for Any" of N addresses.
**Strategy for this Phase:**
*   **Primary:** Stick to **Busy Polling** (Gold Standard). The `FanInReader` will iterate through all readers.
*   **Yielding:** Implement a `WaitStrategy` that performs a `std::thread::yield_now()` or `spin_loop` if no data is found across all queues.
*   **Defer:** `epoll`/`eventfd` based waiting (which allows true blocking wait for N queues) is a heavy "Control Plane" feature and should be separate from the "Data Plane" latency optimizations. It requires the Writer to write to `eventfd`s, which we want to avoid in the hot path.

## Migration Steps

1.  **Update `ControlBlock`**: Add `waiters_pending`. Increment `CTRL_VERSION` to invalidate old shared memory files (safety).
2.  **Update `QueueReader`**: Implement the `wait()` protocol (Register -> Check -> Sleep -> Deregister).
3.  **Update `QueueWriter`**: Add the conditional wake logic.
4.  **Update `FanInReader`**: Explicitly define its polling/yielding behavior (it currently relies on the user calling `next()` in a loop).

## Verification
*   **Benchmark:** Compare `cargo bench` throughput before/after. Expect significant gain in single-writer/single-reader scenarios.
*   **Correctness:** Verify no deadlocks (Writer skips wake vs Reader sleeps forever) using `soak_writer_reader` tests.
