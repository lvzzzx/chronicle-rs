# Implement Segment Rolling and Retention Cleanup

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Phase 5 makes the queue scale beyond a single fixed-size segment. After this change, a writer automatically rolls to a new segment file when the current one runs out of space, readers can continue reading seamlessly across segment boundaries, and old segments can be deleted once every reader has advanced past them. You will be able to verify this by appending enough data to force a rollover, reading both messages with a single reader, and observing that retention deletes only the fully consumed segment while keeping the active segment intact.

## Progress

- [x] (2026-01-15 23:55Z) ExecPlan created for Phase 5 segment rolling and retention.
- [x] (2026-01-16 00:00Z) Updated segment helpers and reader metadata format for multi-segment positions.
- [x] (2026-01-16 00:03Z) Implemented writer rollover logic and index persistence for multi-segment queues.
- [x] (2026-01-16 00:05Z) Implemented reader multi-segment traversal and upgraded reader metadata.
- [x] (2026-01-16 00:07Z) Implemented retention cleanup based on minimum reader segment.
- [x] (2026-01-16 00:09Z) Added integration tests for rollover, cross-segment reads, and retention behavior.
- [x] (2026-01-16 00:11Z) Ran `cargo test`; all tests passed including new rollover/retention coverage.

## Surprises & Discoveries

None yet. Record any edge cases discovered around trailing padding, segment boundary detection, or retention safety.

## Decision Log

- Decision: Keep segment files named as zero-padded 9-digit ids with `.q` extension (for example `000000000.q`, `000000001.q`) and use `index.meta` as a 16-byte little-endian record `{current_segment, write_offset}`.
  Rationale: This matches the existing design and preserves the current on-disk format while enabling rollover.
  Date/Author: 2026-01-15, Codex
- Decision: Upgrade reader metadata to store `{segment_id, read_offset}` as 16 bytes, while treating legacy 8-byte files as `{segment_id = 0, read_offset = <u64>}`.
  Rationale: This keeps backward compatibility with Phase 3/4 data while allowing readers to resume across segments.
  Date/Author: 2026-01-15, Codex
- Decision: Retention deletes only segments with ids strictly less than the minimum reader segment id; when no readers exist, retention performs no deletions.
  Rationale: This is the safest default for Phase 5 and aligns with the requirement that all readers must advance before data is reclaimed.
  Date/Author: 2026-01-15, Codex
- Decision: Guard retention cleanup from deleting the current writer segment if reader metadata is inconsistent.
  Rationale: A `current_segment` lower than the computed minimum indicates corrupt reader metadata; keeping the active segment avoids data loss while still deleting safe segments.
  Date/Author: 2026-01-16, Codex

## Outcomes & Retrospective

Phase 5 is complete. Writers roll to new segments when a record would overflow, readers traverse across segments and persist `{segment_id, offset}`, and retention deletes only segments that every reader has fully left. New integration tests cover rollover and retention behavior, and `cargo test` is green.

## Context and Orientation

The repository currently supports a single fixed segment named `000000000.q`. `src/writer.rs` rejects any `index.meta` value with `current_segment != 0`, and `QueueWriter::append` returns `segment full` rather than rolling. `src/reader.rs` stores only a single `read_offset` in `readers/<name>.meta` and reads from the queue’s single in-memory `mmap`. `src/segment.rs` defines `SEGMENT_SIZE` and provides the 16-byte `index.meta` load/store helpers. There is no segment cleanup yet. The Phase 4 plan (`execplans/phase-4-event-notification.md`) added notifier integration, which should remain intact.

In this plan, a “segment” means a fixed-size file that stores a contiguous sequence of message records. “Segment rolling” means the writer closes the current segment and starts a new one once the next record would exceed the segment size. “Retention cleanup” means deleting segment files only when every reader has moved to a newer segment id.

## Plan of Work

First, extend `src/segment.rs` with helpers for multi-segment file naming and access. Define a formatting function that produces the zero-padded filename, a path helper that returns the full path under the queue directory, and add a small helper that validates a segment file’s size when opened. Keep `SEGMENT_SIZE` as the authoritative fixed size for Phase 5. This step also introduces a small `ReaderPosition` struct (or equivalent) representing `{segment_id, offset}` for use by reader metadata load/store functions.

Next, update `src/writer.rs` to support rollover. Replace the hard-coded `SEGMENT_FILE` constant with the new segment naming helper, allow `Queue::open` to load any `current_segment` from `index.meta`, and open the corresponding segment file (creating it if missing). Convert `current_segment` to an atomic so readers in the same process can observe rolls safely. Adjust `QueueWriter::append` to detect when a record would exceed `SEGMENT_SIZE`, flush and sync the current `mmap`, persist the old index, roll to the next segment id, create and map the new segment file, reset the write index to zero, and then write the record. If a single record is larger than `SEGMENT_SIZE - HEADER_SIZE`, return `Error::Unsupported` with a clear message. Keep the commit flag semantics unchanged, and keep the notifier wake-up after the commit flag is set. Ensure `QueueWriter::flush` writes the current segment id and offset after any roll.

Then, update `src/reader.rs` to store and load reader positions that include segment id. Store reader metadata as 16 bytes little-endian and accept legacy 8-byte files as segment 0. Give `QueueReader` its own `MmapFile` for the current segment, plus `segment_id` and `read_offset` fields. Modify `Queue::reader` to open the segment file that the metadata references and to fail with `Error::Corrupt` if that segment file is missing, because retention should never remove segments still referenced by a reader. Update `QueueReader::next` to read within its current segment; when it reaches the end of the file or sees an all-zero header with `length == 0` and `flags == 0`, check if the next segment file exists, and if it does then switch to that file and reset `read_offset` to zero. Only return `None` if the next segment file does not exist or if the current header is incomplete (flags unset) and the next segment is absent. This preserves correct behavior when a writer has rolled and left trailing zero padding in the old segment.

After reader and writer work, add retention cleanup in a new `src/retention.rs` module and expose it via a `Queue::cleanup()` method. The cleanup function should scan `readers/` for `.meta` files, parse each reader position, compute the minimum segment id, and delete any segment files with ids lower than that minimum. Always keep the current writer segment and any newer segment ids. If there are no reader metadata files, do not delete any segments. Return the list of deleted segment ids to support tests.

Finally, add integration tests that exercise the new behavior. Add `tests/segment_rollover.rs` to append a payload large enough to fill a segment, append a second payload that forces a roll, and verify that both segment files exist and that a single reader can read both messages in order. Add `tests/retention_cleanup.rs` to create at least two segments, advance a reader into the newer segment, call cleanup, and assert that only the older segment is deleted. Add a second reader (or hold one back) to assert that cleanup does not delete when any reader is still on the older segment.

## Concrete Steps

Implement the changes described above by editing these files:

    src/segment.rs
    src/writer.rs
    src/reader.rs
    src/retention.rs (new)
    src/lib.rs
    tests/segment_rollover.rs (new)
    tests/retention_cleanup.rs (new)

Then run the test suite from the repository root:

    (cwd: /Users/zjx/Documents/chronicle-rs)
    cargo test

Expect all existing tests plus the new rollover/retention tests to pass. If a test fails due to old metadata formats, delete the temporary test directories created by `tempfile` and rerun.

## Validation and Acceptance

Acceptance is met when the following behaviors are observed:

A writer appending a record that would exceed `SEGMENT_SIZE` transparently rolls to the next segment file and continues writing without returning a `segment full` error. A reader can read across the boundary and obtains both messages in order. The new rollover test should fail before the change and pass after.

Retention deletes only segments with ids lower than every reader’s current segment. When at least one reader remains on the older segment, cleanup leaves that segment intact. When all readers advance to the newer segment, cleanup deletes the older segment. The retention test should demonstrate both outcomes.

## Idempotence and Recovery

All steps are safe to rerun. Segment files are created in test temp directories, and cleanup logic is designed to be a no-op when no readers exist. If segment files are left in a dirty state during development, delete the temporary queue directories and rerun tests; no persistent state is required for Phase 5.

## Artifacts and Notes

A successful `cargo test` run should include the new test names, for example:

    running 1 test
    test segment_rollover_and_read_across ... ok

    running 1 test
    test retention_deletes_only_when_all_readers_advance ... ok

## Interfaces and Dependencies

Implement the following interfaces and helpers:

In `src/segment.rs`, add functions that are used by both reader and writer:

    pub fn segment_filename(id: u64) -> String
    pub fn segment_path(root: &Path, id: u64) -> PathBuf
    pub fn open_segment(root: &Path, id: u64) -> Result<MmapFile>
    pub fn create_segment(root: &Path, id: u64) -> Result<MmapFile>

Also add a reader position struct to unify metadata parsing:

    pub struct ReaderPosition {
        pub segment_id: u64,
        pub offset: u64,
    }

In `src/writer.rs`, ensure `Queue` and `QueueWriter` expose:

    impl Queue {
        pub fn cleanup(&self) -> Result<Vec<u64>>;
    }

`QueueWriter::append` must roll segments when needed and keep the commit-flag semantics intact. `QueueWriter::flush` must persist `{current_segment, write_offset}` in `index.meta` after any roll.

In `src/reader.rs`, update metadata to store and load `ReaderPosition`, accept legacy 8-byte files, and track `segment_id` plus `read_offset` in `QueueReader`.

`src/retention.rs` should depend only on the standard library and `crate::segment` helpers; no new external dependencies are required.

Change note: Initial ExecPlan drafted for Phase 5 (2026-01-15).
Change note: Updated progress, decisions, and outcomes after implementation and tests (2026-01-16).
