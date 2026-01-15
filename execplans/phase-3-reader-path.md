# Implement Reader Path and Per-Reader Metadata

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

This phase adds the read side of the queue so consumers can iterate messages safely and recover after restarts. After this change, a developer can open a `QueueReader`, read committed messages in order, and persist per-reader offsets to disk so that a restart resumes from the last committed position.

## Progress

- [x] (2026-01-15 23:30Z) ExecPlan created for Phase 3.
- [x] (2026-01-15 23:55Z) Implemented `QueueReader` with `next` and `commit`.
- [x] (2026-01-15 23:55Z) Added per-reader metadata files under `readers/`.
- [x] (2026-01-16 00:00Z) Added tests for read/commit and restart recovery.
- [x] (2026-01-16 00:05Z) Ran `cargo test`; all tests passed.

## Surprises & Discoveries

No surprises yet. Capture any read consistency or commit-flag timing issues discovered during implementation.

## Decision Log

- Decision: Store per-reader metadata at `queue_path/readers/<name>.meta` as a fixed 8-byte little-endian `read_offset`.
  Rationale: Simple and efficient; one reader = one file, matching the design document.
  Date/Author: 2026-01-15, Codex
- Decision: `QueueReader::next` returns `Option<Vec<u8>>` rather than a borrowed slice.
  Rationale: The reader should be able to return data without tying the caller to a lifetime over the mmap borrow; we can optimize later with zero-copy APIs.
  Date/Author: 2026-01-15, Codex

## Outcomes & Retrospective

Phase 3 is complete: readers can consume committed messages, persist offsets, and recover after restart; tests are green.

## Context and Orientation

The project now supports a single-writer append path with persisted index metadata. Messages are written as a 64-byte header followed by the payload. The header includes a `flags` byte where bit 0 indicates commit/valid. The writer sets the flag after the payload is written. A “reader” in this phase means a consumer that reads sequentially from the queue’s segment file, waiting for `flags == 1` and skipping incomplete entries. Each reader tracks its own committed offset in a metadata file so it can resume after restart.

Relevant files are `src/reader.rs` (currently a placeholder), `src/writer.rs` (queue, segment file location, index path), and `src/segment.rs` (segment size constant). Tests will be added under `tests/`.

## Plan of Work

First, define `QueueReader` in `src/reader.rs`, plus a simple `ReaderMeta` helper for reading/writing `read_offset` from disk. `Queue::reader(name)` will open or create the reader metadata file and initialize `read_offset` from it. `QueueReader::next` will read the header at the current offset, check `flags`, and if committed, return the payload and advance the in-memory cursor. `QueueReader::commit` will persist the current offset to the reader’s metadata file.

Second, update `src/lib.rs` to re-export `QueueReader`. Add minimal helper functions in `src/reader.rs` to read headers and validate CRC for the payload.

Finally, add tests that append two messages, read them back via a named reader, commit after each, then reopen the reader and verify it resumes from the committed offset.

## Concrete Steps

From the repository root, implement or edit:

  - `src/reader.rs`: define:
      - `pub struct QueueReader { queue: Arc<Queue>, read_offset: u64, name: String }`
      - `impl Queue { pub fn reader(&self, name: &str) -> Result<QueueReader> }`
      - `impl QueueReader { pub fn next(&mut self) -> Result<Option<Vec<u8>>>; pub fn commit(&self) -> Result<()> }`
    Use a per-reader file at `queue_path/readers/<name>.meta` storing `read_offset` as 8-byte little-endian. Read the header at `read_offset`, check `flags == 1`, validate CRC, and return the payload.
  - `src/lib.rs`: re-export `QueueReader`.
  - `tests/reader_recovery.rs`: create a queue, append two payloads, read them with a named reader, commit after each, then reopen the reader and ensure no additional payloads are returned.

Then run:

  (cwd: /Users/zjx/Documents/chronicle-rs)
  cargo test

## Validation and Acceptance

Acceptance is met when:

- `cargo test` is green with new reader tests.
- `QueueReader::next` returns payloads in order and returns `None` when no committed message is available.
- `QueueReader::commit` persists the offset so a reopened reader resumes from the correct position.

Example success excerpt:

  running 1 test
  test reader_recovery::read_commit_and_recover ... ok
  test result: ok. 1 passed; 0 failed

## Idempotence and Recovery

Tests use temporary directories and do not leave data behind. If a reader metadata file is corrupted during development, delete the temp directory and rerun the test.

## Artifacts and Notes

Green `cargo test` run:

    running 1 test
    test read_commit_and_recover ... ok
    test result: ok. 1 passed; 0 failed

## Interfaces and Dependencies

No new dependencies required.

Public interfaces to define:

In `src/reader.rs`:

    pub struct QueueReader { /* queue, read_offset, name */ }

    impl Queue {
        pub fn reader(&self, name: &str) -> crate::Result<QueueReader>;
    }

    impl QueueReader {
        pub fn next(&mut self) -> crate::Result<Option<Vec<u8>>>;
        pub fn commit(&self) -> crate::Result<()>;
    }

Change note: Updated Progress, Outcomes, and Artifacts after implementing Phase 3 and running tests (2026-01-16).
