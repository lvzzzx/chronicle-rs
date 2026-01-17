# Latency & Synchronization Design

This document details the low-latency synchronization protocols used in Chronicle-RS to achieve zero-syscall operation in the happy path.

## The Goal

In Ultra-Low Latency (ULL) systems, system calls are expensive (~50-100ns + cache pollution).
*   **Writer Goal:** Never call `futex_wake` if readers are active (busy-spinning).
*   **Reader Goal:** Never call `futex_wait` if data is already available.

To achieve this, we use a **Signal Suppression** strategy with a specific race-prevention protocol.

## The Protocol: "Waiter Flag + Check-After-Set"

We introduce a shared atomic state `waiters_pending` in the Control Block.

### Shared State

```rust
struct ControlBlock {
    // ...
    // Atomic counter.
    // 0 = All readers are active (spinning in userspace).
    // >0 = At least one reader is sleeping (or preparing to sleep).
    pub waiters_pending: AtomicU32, 
    
    // The futex word used for the actual wait queue.
    pub notify_seq: AtomicU32,
}
```

### 1. The Writer (Signal Suppression)

The Writer attempts to stay entirely in userspace. It only enters the kernel if it knows a reader is waiting.

```rust
fn append() {
    // 1. Write Data
    write_payload();
    
    // 2. Commit (Release Ordering)
    // Makes data visible to readers.
    store_commit_len();
    
    // 3. Update Sequence
    notify_seq.fetch_add(1, Relaxed);
    
    // 4. Check Waiters (Relaxed Load)
    // Optimization: If 0, we assume everyone is awake and skip the syscall.
    if waiters_pending.load(Relaxed) > 0 {
        // Slow Path: Wake up the OS scheduler
        futex_wake(notify_seq);
    }
}
```

**Why Relaxed is safe:**
If the Writer sees `0` but a Reader was *just* about to sleep (transitioning from 0->1), the Reader's "Check-After-Set" step (see below) will catch the new data and abort the sleep.

### 2. The Reader (Check-After-Set)

The Reader must ensure it never sleeps if the Writer wrote data *during* the transition from "Active" to "Sleeping".

```rust
fn wait() {
    // 1. Busy Spin (Userspace)
    // Most messages are caught here.
    spin_for(10_us);
    
    // --- PREPARE TO SLEEP ---
    
    // 2. Register Presence (SeqCst)
    // "I am going to sleep. Writer, you must wake me."
    waiters_pending.fetch_add(1, SeqCst);
    
    // 3. The Double-Check (Check-After-Set)
    // CRITICAL: We must check for data ONE LAST TIME.
    // Race Scenario:
    //   - Reader checks data (None)
    //   - Writer writes data
    //   - Writer checks waiters (0) -> Skips Wake
    //   - Reader increments waiters (1)
    //   - Reader sleeps? -> DEADLOCK!
    // This step prevents that deadlock.
    if peek_committed() {
        // Data was written while we were registering!
        // Abort sleep, deregister, and process data.
        waiters_pending.fetch_sub(1, SeqCst);
        return;
    }
    
    // 4. Sleep (Kernel)
    // Safe now. If Writer comes *after* step 2, they will see waiters > 0.
    let seq = notify_seq.load(Acquire);
    futex_wait(notify_seq, seq);
    
    // 5. Deregister
    waiters_pending.fetch_sub(1, SeqCst);
}
```

## Memory Ordering

*   **Writer Commit:** `Release` (Publish data).
*   **Writer Check:** `Relaxed` (Optimization hint).
*   **Reader Register:** `SeqCst` (Strict ordering relative to the data check).
*   **Reader Check:** `Acquire` (See the data).

This combination guarantees that we never miss a wake-up, while minimizing the cost of the fast path (Writer just does a Load, Reader just does a Spin).
