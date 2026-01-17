# Code Review: Ultra-Low Latency Compliance

**Date:** 2026-01-17
**Target:** `crates/chronicle-core`
**Reviewer:** Dr. Chen (Agent)

## Executive Summary
The core memory layout and synchronization protocol (Waiters/Futex) are sound and meet ULL standards. However, the surrounding implementation details in `merge.rs` (Fan-In) and `writer.rs` (Retention) violate "Zero-Syscall" and "Zero-Allocation" principles on the hot path.

## Critical Violations

### 1. Heap Allocations in Fan-In
**File:** `src/merge.rs`
**Severity:** Critical
**Description:**
`FanInReader::next()` clones the message payload into a `Vec<u8>` to sort messages by timestamp.
```rust
// src/merge.rs:141
payload: view.payload.to_vec(), // <--- HEAP ALLOCATION
```
**Impact:** Deterministic latency is impossible. Throughput is limited by `malloc`/`free`.

### 2. Synchronous I/O in Writer
**File:** `src/writer.rs`
**Severity:** High
**Description:**
`append()` calls `ensure_capacity()`, which invokes `cleanup_segments()`. This triggers `fs::read_dir` inline.
```rust
// src/writer.rs:248
crate::retention::cleanup_segments(...) // <--- FILESYSTEM I/O
```
**Impact:** Write latency spikes correlated with directory size and disk contention.

### 3. Syscall Polling in Reader
**File:** `src/reader.rs`
**Severity:** Medium
**Description:**
`advance_segment` calls `exists()` (stat) potentially frequently at segment boundaries.
```rust
// src/reader.rs:248
if !next_path.exists() { // <--- SYSCALL
```
**Impact:** High system overhead (context switches) during segment transitions.

## Recommendations

1.  **Refactor Fan-In:** Implement a "Zero-Copy" Sort. Store `(ReaderIndex, View)` references or reuse fixed-size buffers.
2.  **Async Retention:** The `QueueWriter` should check limits but delegate physical deletion to a background thread (Sidecar).
3.  **Optimize Discovery:** Readers should only check for new segments if the current one is `SEALED` or via a notification mechanism, never busy-poll `stat`.
