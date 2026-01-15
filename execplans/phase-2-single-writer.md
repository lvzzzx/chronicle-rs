# Implement Single-Writer Append Path

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

This phase turns the core storage into a usable queue for a single writer. After this change, a developer can open a queue directory, append one or more messages, and see a correctly committed header and payload in the memory-mapped segment file. The behavior is observable through tests that validate the write index, header fields, and CRC over the payload.

## Progress

- [x] (2026-01-15 22:20Z) ExecPlan created for Phase 2.
- [x] (2026-01-15 23:05Z) Defined `Queue` and `QueueWriter` and implemented `append` with atomic `write_index`.
- [x] (2026-01-15 23:05Z) Implemented minimal segment + index metadata persistence for a single segment.
- [x] (2026-01-15 23:10Z) Added tests for append behavior, header commit flag, and restart recovery.
- [x] (2026-01-15 23:15Z) Ran `cargo test`; all tests passed.

## Surprises & Discoveries

No surprises yet. Record any misalignment, durability, or atomic ordering issues encountered while implementing append semantics.

## Decision Log

- Decision: For Phase 2, operate on a single segment file named `000000000.q` within the queue directory and persist `index.meta` as a fixed 16-byte little-endian record.
  Rationale: Keeps the single-writer path minimal while still persisting write position for recovery. Segment rolling is deferred to Phase 5.
  Date/Author: 2026-01-15, Codex
- Decision: The commit flag is bit 0 of `MessageHeader.flags`, set with `store` and read with `load` semantics modeled using standard Rust atomic ordering around writes.
  Rationale: Mirrors the design document and provides a clear, testable definition of visibility.
  Date/Author: 2026-01-15, Codex
- Decision: Persist `index.meta` only on explicit `flush` rather than every append.
  Rationale: Avoids fsync/file I/O in the hot path and matches the low-latency design intent.
  Date/Author: 2026-01-15, Codex

## Outcomes & Retrospective

Phase 2 is complete: single-writer append works end-to-end with index persistence on `flush`, and tests are green.

## Context and Orientation

The repository now contains a working `MessageHeader` and an `MmapFile` wrapper. There is no queue implementation yet. The design document (`docs/DESIGN.md`) defines append-only message storage with a 64-byte header and a commit flag. For this phase, a “segment” means a single mmap-backed file that holds a stream of header+payload records. “Atomic write index” means a counter updated with CPU atomic operations so multiple threads never claim the same bytes, even though Phase 2 only uses a single writer. “Commit flag” means the header’s `flags` bit 0 is set after the payload is fully written so readers can detect a complete record.

Key files to edit are `src/writer.rs` for queue and writer logic, `src/segment.rs` for index metadata persistence, and `src/lib.rs` to re-export public types. Tests will be added under `tests/`.

## Plan of Work

First, implement `Queue` and `QueueWriter` in `src/writer.rs`. `Queue` owns the mmap for the segment file, the queue path, an atomic `write_index`, and the current segment id (always 0 for Phase 2). `Queue::open(path)` creates the directory if missing, opens or creates `000000000.q` at a fixed size, and loads or initializes `index.meta` to restore `write_index`. `QueueWriter::append(payload)` computes the total record length (64-byte header + payload), reserves space by incrementing `write_index`, writes the header with `flags = 0`, copies the payload, then writes the header again with `flags = 1`. It then persists `index.meta` with the updated write offset.

Next, implement minimal segment metadata helpers in `src/segment.rs`. Store `current_segment` and `write_offset` as a 16-byte record (`u64` + `u64` in little-endian). Provide `load_index(path)` and `store_index(path, index)` functions, and use `sync_all` on the metadata file to ensure it is durable for tests.

Finally, add integration tests that open a queue in a temp directory, append multiple payloads, and verify that the header and payload bytes at the expected offsets match the values written. Also include a recovery test that re-opens the queue and checks that `write_index` continues from the persisted offset.

## Concrete Steps

From the repository root, implement or edit these files:

  - `src/segment.rs`: define `SegmentIndex` as `{ current_segment: u64, write_offset: u64 }` and implement:
      - `pub fn load_index(path: &Path) -> Result<SegmentIndex>`
      - `pub fn store_index(path: &Path, index: &SegmentIndex) -> Result<()>`
    Store the index as 16 bytes: `current_segment` then `write_offset`, both little-endian.
  - `src/writer.rs`: define `Queue` and `QueueWriter`:
      - `pub struct Queue { path: PathBuf, mmap: MmapFile, write_index: AtomicU64, current_segment: u64 }`
      - `pub struct QueueWriter { queue: Arc<Queue> }`
      - `impl Queue { pub fn open(path: impl AsRef<Path>) -> Result<Arc<Self>>; pub fn writer(self: &Arc<Self>) -> QueueWriter }`
      - `impl QueueWriter { pub fn append(&self, payload: &[u8]) -> Result<()> }`
    In `append`, compute checksum with `MessageHeader::crc32`, fill `MessageHeader`, and write the committed header after payload is written. Persist `index.meta` after the append.
  - `src/lib.rs`: re-export `Queue` and `QueueWriter` so tests can use them.
  - `tests/append_single_writer.rs`: create a temp queue directory, append two payloads, open the segment with `MmapFile`, read headers and payloads at expected offsets, and validate `flags == 1` plus CRC correctness. Add a recovery check by reopening `Queue::open` and appending again, verifying that offsets continue past the last write.

Use a fixed segment size for Phase 2, such as `1 * 1024 * 1024` bytes, defined as `const SEGMENT_SIZE: usize = 1_048_576` in `src/segment.rs` and used by `Queue::open`.

Then run:

  (cwd: /Users/zjx/Documents/chronicle-rs)
  cargo test

## Validation and Acceptance

Acceptance is met when:

- `cargo test` is green with new tests added.
- Appending two messages results in two committed headers (`flags == 1`) and payloads at the correct offsets.
- CRC validation passes for each payload using `MessageHeader::validate_crc`.
- Re-opening the queue continues writing after the last persisted `write_offset` in `index.meta`.

Example successful test excerpt:

  running 1 test
  test append_single_writer::append_two_messages ... ok
  test result: ok. 1 passed; 0 failed

## Idempotence and Recovery

All steps are safe to rerun. The tests use temporary directories and do not leave data behind. If an append test fails because the segment is too small, increase `SEGMENT_SIZE` and re-run. If index metadata is corrupted during development, delete the temp directory and re-run tests.

## Artifacts and Notes

Green `cargo test` run:

    running 1 test
    test append_two_messages_and_recover ... ok
    test result: ok. 1 passed; 0 failed

## Interfaces and Dependencies

No new dependencies are required beyond those already added in Phase 1.

Public interfaces to define:

In `src/writer.rs`, define:

    pub struct Queue { /* path, mmap, write_index, current_segment */ }
    pub struct QueueWriter { /* Arc<Queue> */ }

    impl Queue {
        pub fn open(path: impl AsRef<Path>) -> crate::Result<std::sync::Arc<Self>>;
        pub fn writer(self: &std::sync::Arc<Self>) -> QueueWriter;
    }

    impl QueueWriter {
        pub fn append(&self, payload: &[u8]) -> crate::Result<()>;
    }

In `src/segment.rs`, define:

    pub struct SegmentIndex {
        pub current_segment: u64,
        pub write_offset: u64,
    }

    pub fn load_index(path: &Path) -> crate::Result<SegmentIndex>;
    pub fn store_index(path: &Path, index: &SegmentIndex) -> crate::Result<()>;

Change note: Updated Decision Log and Outcomes to record index persistence moving to explicit flush (2026-01-15).
