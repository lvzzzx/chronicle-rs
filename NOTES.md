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

## Codebase Gotchas

## Known Issues