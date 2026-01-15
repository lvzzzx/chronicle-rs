# Project Roadmap

This roadmap tracks major milestones for the Chronicle-style persisted messaging queue. Phases are sequential by default, but can overlap when dependencies are satisfied. When a phase is underway, an ExecPlan should be created under `execplans/`.

## Current Status

- Phase 0 (Repo Bootstrap) completed.
- Phase 1 (Core Storage & Header) completed; ExecPlan in `execplans/phase-1-core-storage.md`.
- Phase 2 (Single-Writer Append Path) completed; ExecPlan in `execplans/phase-2-single-writer.md`.
- Phase 3 (Reader Path & Per-Reader Metadata) completed; ExecPlan in `execplans/phase-3-reader-path.md`.
- Phase 4 (Event Notification) completed; ExecPlan in `execplans/phase-4-event-notification.md`.

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
- Deliverables: notifier implementation, reader discovery, wake-ups on append.
- Validation: reader blocks and wakes reliably; add/remove reader handled.

### Phase 5 — Segment Rolling & Retention
- Deliverables: segment rollover at size boundary, index persistence, safe cleanup.
- Validation: read/write across segments; retention only after all readers pass.

### Phase 6 — Multi-Queue Fan-In (Order Bus)
- Deliverables: merge logic across queues by timestamp.
- Validation: deterministic ordering across multiple writers.

### Phase 7 — Hardening & Benchmarks
- Deliverables: stress tests, soak tests, perf benchmarks.
- Validation: stable throughput/latency and test coverage of edge cases.

### Phase 8 — Docs & Operational Guidance
- Deliverables: updated design docs, usage examples, configuration guidance.
- Validation: docs reflect actual behavior and configuration paths.
