# Project Roadmap

This roadmap tracks major milestones for the Chronicle-style persisted messaging queue. Phases are sequential by default, but can overlap when dependencies are satisfied. When a phase is underway, an ExecPlan should be created under `execplans/`.

## Current Status

- Phase 0 (Repo Bootstrap) completed.
- Phase 1 (Core Storage & Header) completed; ExecPlan in `execplans/phase-1-core-storage.md`.
- Phase 2 (Single-Writer Append Path) completed; ExecPlan in `execplans/phase-2-single-writer.md`.
- Phase 3 (Reader Path & Per-Reader Metadata) completed; ExecPlan in `execplans/phase-3-reader-path.md`.
- Phase 4 (Event Notification) completed; ExecPlan in `execplans/phase-4-event-notification.md`.
- Phase 5 (Segment Rolling & Retention) completed; ExecPlan in `execplans/phase-5-segment-rolling.md`.
- Phase 6 (Multi-Queue Fan-In) completed; ExecPlan in `execplans/phase-6-multi-queue-fanin.md`.
- Phase 7 (Queue Discovery) completed in `crates/chronicle-bus/` (scan + watch + dedup in `RouterDiscovery::poll()` with Linux inotify and periodic rescan fallback).
- Phase 8 (Hardening & Benchmarks) completed; ExecPlan in `execplans/phase-8-hardening-benchmarks.md`.

## Phases

### Phase 0 — Repo Bootstrap (foundation)
- Deliverables: `Cargo.toml`, module layout in `src/`, baseline error type, placeholder tests.
- Validation: `cargo build` and `cargo test` succeed.

### Phase 1 — Core Storage & Header
- Deliverables: `MessageHeader` layout and serialization, CRC utilities, minimal mmap wrapper.
- Validation: tests for size/alignment/CRC; round-trip append/read using mmap.

### Phase 2 — Single-Writer Append Path
- Deliverables: `Queue` + `QueueWriter`, atomic `write_index`, commit flag semantics.
- Validation: append loop with monotonic offsets; readable payloads from mmap.

### Phase 3 — Reader Path & Per-Reader Metadata
- Deliverables: `QueueReader::next` and `commit`, per-reader metadata persistence.
- Validation: restart recovery test; stuck-writer timeout behavior.

### Phase 4 — Event Notification (eventfd + inotify)
- Deliverables: Linux notifier using `eventfd` for reader wait and `inotify` for writer discovery; cross-platform sleep fallback.
- Validation: reader blocks and wakes reliably; add/remove reader handled.
- Note: DESIGN.md Section 5 describes a futex-based hybrid wait strategy; see Phase 11 for potential alignment.

### Phase 5 — Segment Rolling & Retention
- Deliverables: segment rollover at size boundary, index persistence, safe cleanup.
- Validation: read/write across segments; retention only after all readers pass.

### Phase 6 — Multi-Queue Fan-In (Fan-In Reader)
- Deliverables: merge logic across queues by timestamp.
- Validation: deterministic ordering across multiple writers.

### Phase 7 — Queue Discovery for Multi-Process Fan-In (chronicle-bus)
- Deliverables: `BusLayout`, `StrategyEndpoints`, READY/LEASE markers, `RouterDiscovery` scan + watch, `ReaderRegistration` RAII cleanup.
- Status:
  - **Complete**: `BusLayout`, `StrategyEndpoints`, `mark_ready()`, `write_lease()`, `ReaderRegistration` (RAII drop cleanup).
  - **Complete**: `RouterDiscovery::poll()` implements scan + watch (Linux inotify) with periodic rescan fallback.
- Validation: router attaches/detaches queues dynamically without restart; handles create/delete races safely.

### Phase 8 — Hardening & Benchmarks (chronicle-core)
- Deliverables: stress tests (`stress_single_writer.rs`), soak tests (`soak_writer_reader.rs`), Criterion benchmarks (`append.rs`, `read.rs`).
- Validation: stable throughput/latency under load; `cargo test -p chronicle-core` and `cargo bench -p chronicle-core` pass.

### Phase 9 — Docs, Examples & Operational Guidance
- Deliverables:
  - Updated design docs and configuration guidance.
  - **End-to-end example** demonstrating DESIGN.md Section 12 topology:
    - `examples/feed.rs` — market data writer (single queue, multiple symbols)
    - `examples/strategy.rs` — reads market data, filters symbols, writes orders via `chronicle-bus` layout
    - `examples/router.rs` — uses `RouterDiscovery` to find strategies, fan-in merges orders
  - README with quickstart showing how to run the example processes.
- Validation: example processes run concurrently, demonstrate queue creation, READY markers, discovery, and message flow.

### Phase 10 — CLI Tooling (chronicle-cli)
- Deliverables: `chron-cli` binary with subcommands per DESIGN.md Section 14:
  - `inspect <queue_path>`: display writer position, reader lag, process liveness.
  - `tail <queue_path> [-f]`: stream message headers and payload hexdumps.
  - `doctor <bus_root>`: identify stale locks, retention candidates.
  - `bench`: local throughput/latency smoke test.
- Validation: commands produce correct output against test queues.

### Phase 11 — Notification Protocol Alignment (optional)
- Deliverables: migrate from eventfd+inotify to futex-based hybrid wait per DESIGN.md Section 5.
- Rationale: DESIGN.md specifies `notify_seq` in `ControlBlock` with futex wake; current implementation uses eventfd. Alignment improves design/code consistency and may reduce syscall overhead.
- Validation: readers use hybrid spin + futex; writer increments `notify_seq` and calls `futex_wake`; existing tests pass.
