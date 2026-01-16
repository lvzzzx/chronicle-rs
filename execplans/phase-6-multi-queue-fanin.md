# Implement Multi-Queue Fan-In (Fan-In Reader)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Phase 6 lets a single reader consume messages from multiple queue directories in deterministic timestamp order. After this change, you can run several independent writers (one per queue) and read a single merged stream that is ordered by `timestamp_ns`, with ties broken predictably by source index. You can verify this by creating two queues in temporary directories, writing interleaved timestamps to each, and then observing that the merged reader returns the messages sorted by timestamp (and by source index when timestamps match).

## Progress

- [x] (2026-01-15 22:30Z) ExecPlan created for Phase 6 multi-queue fan-in (Fan-In Reader).
- [x] (2026-01-15 23:10Z) Writer timestamps added via `append_with_timestamp`; `append` uses wall-clock time.
- [x] (2026-01-15 23:12Z) Fan-in merge implemented in `crates/chronicle-core/src/merge.rs` using `MessageView`.
- [x] (2026-01-15 23:14Z) Added fan-in ordering tests in `crates/chronicle-core/tests/merge_fanin.rs`.
- [x] (2026-01-16 21:20Z) ExecPlan updated to reflect the current implementation and file paths.

## Surprises & Discoveries

None yet. Capture any tricky ordering edge cases or metadata/segment interactions that affect merge correctness.

## Decision Log

- Decision: Define `timestamp_ns` as nanoseconds since the Unix epoch (`SystemTime::now().duration_since(UNIX_EPOCH)`), and treat smaller timestamps as earlier in the merged order.
  Rationale: The header already stores `timestamp_ns`, and this definition is easy to compute and to reproduce in tests.
  Date/Author: 2026-01-15, Codex
- Decision: Break timestamp ties using the reader’s source index (lower index wins).
  Rationale: This yields deterministic order across queues without requiring cross-queue sequence coordination.
  Date/Author: 2026-01-15, Codex
- Decision: Implement `FanInReader` with a per-reader pending buffer that is filled using `QueueReader::next` (returning `MessageView`), not a new `next_message` API.
  Rationale: This keeps the public reader API stable and avoids adding a second read path.
  Date/Author: 2026-01-15, Codex
- Decision: Add `QueueWriter::append_with_timestamp` for deterministic tests; `QueueWriter::append` calls it with the current time.
  Rationale: Tests need control over timestamps to prove merge ordering without depending on wall-clock timing.
  Date/Author: 2026-01-15, Codex

## Outcomes & Retrospective

Phase 6 delivered: writer timestamps are real-time or injected for tests, and fan-in merges by `(timestamp_ns, source_index)`. Tests validate merge order and empty behavior. The implementation lives under `crates/chronicle-core/src/merge.rs` with integration tests in `crates/chronicle-core/tests/merge_fanin.rs`.

## Context and Orientation

The repository currently implements a single-queue reader and writer with segment rolling. `crates/chronicle-core/src/header.rs` defines `MessageHeader` containing `timestamp_ns`, `crates/chronicle-core/src/writer.rs` stamps headers with real-time or injected timestamps, and `crates/chronicle-core/src/reader.rs` returns `MessageView` entries that expose timestamp/sequence/type alongside payload. `crates/chronicle-core/src/merge.rs` contains the fan-in merge logic that orders messages across multiple queues by timestamp. Each queue still maintains its own `readers/<name>.meta` file for reader progress.

Terminology used in this plan:
“Fan-in” means consuming from multiple independent queues and producing a single ordered stream. “Fan-In Reader” is the merged reader that performs this fan-in by comparing timestamps. “Pending message” means a message already read from a queue and held in memory until it is selected for delivery.

## Plan of Work

If re-implementing or extending this phase, make sure the writer stamps `timestamp_ns` in the header and allows deterministic injection via `append_with_timestamp` in `crates/chronicle-core/src/writer.rs`. Keep `QueueReader::next` returning `MessageView` in `crates/chronicle-core/src/reader.rs`, since `FanInReader` relies on it to fill pending buffers. Implement fan-in logic in `crates/chronicle-core/src/merge.rs` by tracking a `Vec<Option<PendingMessage>>`, selecting the smallest `(timestamp_ns, source_index)` each call, and returning a `MergedMessage` with source metadata and payload. Add or update tests in `crates/chronicle-core/tests/merge_fanin.rs` to validate ordering and empty behavior.

## Concrete Steps

Edit or add the following files:

    crates/chronicle-core/src/writer.rs
    crates/chronicle-core/src/reader.rs
    crates/chronicle-core/src/merge.rs
    crates/chronicle-core/tests/merge_fanin.rs

Run tests from the repository root:

    (cwd: /Users/zjx/Documents/chronicle-rs)
    cargo test -p chronicle-core

## Validation and Acceptance

Acceptance is met when:

1) `QueueWriter::append` stores a non-zero `timestamp_ns` derived from the current time, and `append_with_timestamp` allows deterministic timestamp injection.
2) `QueueReader::next` returns `MessageView` with `timestamp_ns`, `seq`, and `type_id` alongside payload.
3) `FanInReader::next` returns merged messages ordered by `(timestamp_ns, source_index)` with no cross-queue reordering or dropped messages.
4) The tests in `crates/chronicle-core/tests/merge_fanin.rs` fail before the change and pass after, and `cargo test -p chronicle-core` succeeds overall.

## Idempotence and Recovery

All steps are safe to rerun. The tests use temporary directories, so failed runs can be retried without cleanup beyond rerunning `cargo test -p chronicle-core`. If a local clock error prevents timestamp generation, run tests that use `append_with_timestamp` first, then revisit the system time issue.

## Artifacts and Notes

Expected test output snippets (example):

    running 2 tests
    test merge_orders_by_timestamp_and_source ... ok
    test merge_returns_none_when_empty ... ok

## Interfaces and Dependencies

Implement or expose the following interfaces:

In `crates/chronicle-core/src/writer.rs`:

    impl QueueWriter {
        pub fn append_with_timestamp(&mut self, type_id: u16, payload: &[u8], timestamp_ns: u64) -> Result<()>;
    }

In `crates/chronicle-core/src/reader.rs`:

    pub struct MessageView<'a> {
        pub seq: u64,
        pub timestamp_ns: u64,
        pub type_id: u16,
        pub payload: &'a [u8],
    }

    impl QueueReader {
        pub fn next(&mut self) -> Result<Option<MessageView<'_>>>;
    }

In `crates/chronicle-core/src/merge.rs`:

    pub struct MergedMessage {
        pub source: usize,
        pub seq: u64,
        pub timestamp_ns: u64,
        pub type_id: u16,
        pub payload: Vec<u8>,
    }

    pub struct FanInReader {
        pub fn new(readers: Vec<QueueReader>) -> Self;
        pub fn next(&mut self) -> Result<Option<MergedMessage>>;
        pub fn commit(&mut self, source: usize) -> Result<()>;
    }

No new external dependencies are required. Use the standard library for time (`std::time::SystemTime`) and existing crate modules (`crate::reader`, `crate::header`).

Change note: 2026-01-16 updated this plan to match the current implementation (paths under `crates/chronicle-core`, `MessageView`-based fan-in, and the existing test locations).
