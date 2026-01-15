# Implement Multi-Queue Fan-In (Fan-In Reader)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Phase 6 lets a single reader consume messages from multiple queue directories in deterministic timestamp order. After this change, you can run several independent writers (one per queue) and read a single merged stream that is ordered by `timestamp_ns`, with ties broken predictably. You can verify this by creating two queues in temporary directories, writing interleaved timestamps to each, and then observing that the merged reader returns the messages sorted by timestamp (and by source index when timestamps match).

## Progress

- [x] (2026-01-15 22:30Z) ExecPlan created for Phase 6 multi-queue fan-in (Fan-In Reader).
- [ ] Update writer timestamps and add explicit timestamp append helper.
- [ ] Extend `QueueReader` to return headers with payloads for merge logic.
- [ ] Implement `FanInReader` fan-in merge in `src/merge.rs`.
- [ ] Add fan-in ordering tests and run `cargo test`.

## Surprises & Discoveries

None yet. Capture any tricky ordering edge cases or metadata/segment interactions that affect merge correctness.

## Decision Log

- Decision: Define `timestamp_ns` as nanoseconds since the Unix epoch (`SystemTime::now().duration_since(UNIX_EPOCH)`), and treat smaller timestamps as earlier in the merged order.
  Rationale: The header already stores `timestamp_ns`, and this definition is easy to compute and to reproduce in tests.
  Date/Author: 2026-01-15, Codex
- Decision: Break timestamp ties using the reader’s source index (lower index wins).
  Rationale: This yields deterministic order across queues without requiring cross-queue sequence coordination.
  Date/Author: 2026-01-15, Codex
- Decision: Implement a `FanInReader` that owns `Vec<QueueReader>` and holds one pending message per reader, filled by calling `QueueReader::next_message` when the pending slot is empty.
  Rationale: This avoids invasive changes to the reader, keeps per-queue ordering intact, and enables deterministic merge selection.
  Date/Author: 2026-01-15, Codex
- Decision: Add `QueueWriter::append_with_timestamp` for deterministic tests; `QueueWriter::append` calls it with the current time.
  Rationale: Tests need control over timestamps to prove merge ordering without depending on wall-clock timing.
  Date/Author: 2026-01-15, Codex

## Outcomes & Retrospective

Not started yet. Update this section as milestones complete.

## Context and Orientation

The repository currently implements a single-queue reader and writer with segment rolling. `src/header.rs` defines `MessageHeader` containing `timestamp_ns`, but `QueueWriter::append` always stores `timestamp_ns = 0`, and `QueueReader::next` returns only payload bytes (no header information). `src/merge.rs` is a placeholder module. A new fan-in reader must merge messages from multiple queues by timestamp while preserving per-queue order and reader metadata semantics (each queue retains its own `readers/<name>.meta`).

Key files:
- `src/header.rs` defines `MessageHeader` with `timestamp_ns`.
- `src/writer.rs` builds headers and appends records.
- `src/reader.rs` advances read offsets and persists reader metadata.
- `src/merge.rs` will host the Fan-In Reader merge logic.
- Tests live under `tests/`.

Terminology used in this plan:
“Fan-in” means consuming from multiple independent queues and producing a single ordered stream. “Fan-In Reader” is the merged reader that performs this fan-in by comparing timestamps. “Pending message” means a message already read from a queue and held in memory until it is selected for delivery.

## Plan of Work

First, update the writer timestamp semantics. In `src/writer.rs`, add a helper method `append_with_timestamp` that accepts `timestamp_ns` and stores it in the header. Update the existing `append` method to call `append_with_timestamp` with `SystemTime::now()` converted to nanoseconds since the Unix epoch. If the system clock is before the epoch, return `Error::Unsupported` with a clear message. This keeps existing callers working while making timestamps meaningful.

Next, extend the reader to surface headers. In `src/reader.rs`, introduce a `QueueMessage` struct containing `{ header: MessageHeader, payload: Vec<u8> }`. Add a `next_message` method that returns `Option<QueueMessage>` and advances the reader (reusing the existing logic in `next`). Update `QueueReader::next` to delegate to `next_message` and return only the payload so existing tests and callers continue to compile.

Then implement the Fan-In Reader in `src/merge.rs`. Define a `FanInReader` that owns a vector of `QueueReader` instances and a parallel vector of pending messages (one per reader). `FanInReader::next` should ensure each pending slot is filled by calling `QueueReader::next_message` when it is empty, then choose the pending message with the smallest `timestamp_ns`, breaking ties by smaller source index. Return a `MergedMessage` struct containing `source`, `header`, and `payload`. The returned `source` index identifies which reader produced the message. Provide `FanInReader::commit(source)` (and optionally `commit_message(&MergedMessage)`) that delegates to the underlying reader’s `commit` so callers can persist progress after processing.

Finally, add integration tests to prove deterministic ordering. Create `tests/merge_fanin.rs` to build two temporary queues, append messages with explicit timestamps using `append_with_timestamp`, create `FanInReader` over two readers (with the same reader name in each queue), and verify that the merged output is ordered by timestamp with tie-breaking by source index. Add a second test to confirm that when all queues are empty, `FanInReader::next` returns `None` without error.

## Concrete Steps

Edit or add the following files:

    src/writer.rs
    src/reader.rs
    src/merge.rs
    tests/merge_fanin.rs (new)

Run tests from the repository root:

    (cwd: /Users/zjx/Documents/chronicle-rs)
    cargo test

## Validation and Acceptance

Acceptance is met when:

1) `QueueWriter::append` stores a non-zero `timestamp_ns` derived from the current time, and `append_with_timestamp` allows deterministic timestamp injection.
2) `QueueReader::next_message` returns `MessageHeader` plus payload, and the existing `QueueReader::next` API remains usable.
3) `FanInReader::next` returns merged messages ordered by `(timestamp_ns, source_index)` with no cross-queue reordering or dropped messages.
4) The new tests in `tests/merge_fanin.rs` fail before the change and pass after, and `cargo test` succeeds overall.

## Idempotence and Recovery

All steps are safe to rerun. The tests use temporary directories, so failed runs can be retried without cleanup beyond rerunning `cargo test`. If a local clock error prevents timestamp generation, run tests that use `append_with_timestamp` first, then revisit the system time issue.

## Artifacts and Notes

Expected test output snippets (example):

    running 2 tests
    test merge_orders_by_timestamp_and_source ... ok
    test merge_returns_none_when_empty ... ok

## Interfaces and Dependencies

Implement or expose the following interfaces:

In `src/writer.rs`:

    impl QueueWriter {
        pub fn append_with_timestamp(&self, payload: &[u8], timestamp_ns: u64) -> Result<()>;
    }

In `src/reader.rs`:

    pub struct QueueMessage {
        pub header: MessageHeader,
        pub payload: Vec<u8>,
    }

    impl QueueReader {
        pub fn next_message(&mut self) -> Result<Option<QueueMessage>>;
    }

In `src/merge.rs`:

    pub struct MergedMessage {
        pub source: usize,
        pub header: MessageHeader,
        pub payload: Vec<u8>,
    }

    pub struct FanInReader {
        pub fn new(readers: Vec<QueueReader>) -> Self;
        pub fn next(&mut self) -> Result<Option<MergedMessage>>;
        pub fn commit(&self, source: usize) -> Result<()>;
    }

No new external dependencies are required. Use the standard library for time (`std::time::SystemTime`) and existing crate modules (`crate::reader`, `crate::header`).

Change note: Initial ExecPlan drafted for Phase 6 multi-queue fan-in (2026-01-15).
