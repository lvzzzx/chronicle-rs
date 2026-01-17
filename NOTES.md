# NOTES

## Domain Knowledge

### High-Performance Crypto Market Feed Stack (e.g., Binance)

For a production-grade, low-latency feed in Rust, avoid generic web libraries and focus on minimizing allocations and parsing overhead.

*   **Network Layer:** `tokio` + `tokio-tungstenite`. Use `rustls` for TLS as it is faster, safer, and links statically compared to OpenSSL.
*   **JSON Parsing:** `simd-json`. Essential for handling high-volume exchange JSON. It uses AVX2/NEON instructions and can be 10x faster than `serde_json` by parsing in-place.
*   **Orderbook Management:**
    *   **BTreeMap:** Good for full-depth correctness (O(log N)).
    *   **Flat Vectors:** For fixed-depth (e.g., Top 20), use a pre-allocated `Vec` or array to maximize cache locality.
*   **System Tuning:** Use `core_affinity` to pin the feed process to an isolated CPU core to eliminate context-switching jitter.
*   **Integration:** Map exchange-specific JSON to internal `#[repr(C)]` structs and use `chronicle-rs`'s `append_in_place` to write directly to the shared memory bus without intermediate copies.

### Low-Latency IPC & Synchronization

#### The Gold Standard: Busy Spinning on Isolated Cores

In ultra-low latency (ULL) trading systems, the preferred strategy for inter-process communication (IPC) is **Busy Spinning on Isolated Cores**.

*   **Zero-Syscall Path:** The reader never sleeps. It runs a tight `while true` loop polling a memory-mapped offset. This eliminates context switch overhead (~1-3µs) and system call latency (~50-200ns).
*   **System Tuning:** Use the `isolcpus` kernel boot parameter to prevent the OS scheduler from placing other tasks on critical cores. Use `core_affinity` in Rust to pin threads to these cores.
*   **Cache Locality:** Constant polling keeps target cache lines "hot" in L1/L2 cache, ensuring the reader sees writer updates at the speed of the memory bus.
*   **Implementation:** Use `std::hint::spin_loop()` within the loop to signal the CPU it's in a busy-wait state, improving performance on some architectures without yielding to the OS.
*   **Signal Suppression:** For hybrid paths, writers should only call `futex_wake` if a "waiter flag" in shared memory is set, keeping the happy path entirely in userspace.

## Codebase Gotchas

## Known Issues