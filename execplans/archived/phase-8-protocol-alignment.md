# Archived: Align Core Protocol with Updated Design (commit_len, control.meta, recovery, retention)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

This plan is archived. The protocol-alignment work it describes has been completed and is now part of the `chronicle-core` baseline. The roadmap Phase 8 is now “Hardening & Benchmarks,” so this document is retained only as a historical record of the alignment work (completed on 2026-01-16). A developer can still verify the behavior by appending messages, observing readers skip PAD records, exercising wait/notify behavior, and confirming retention skips stalled readers beyond the lag threshold.

## Progress

- [x] (2026-01-16 00:20Z) ExecPlan created for Phase 8 protocol alignment.
- [x] (2026-01-16 01:10Z) Updated on-disk record format to commit_len + PAD, and rewired writer/reader logic plus tests.
- [x] (2026-01-16 01:20Z) Implemented control.meta mapping, initialization protocol, and futex wait/notify integration.
- [x] (2026-01-16 01:30Z) Implemented recovery scan + tail repair (PAD or seal) on open_publisher.
- [x] (2026-01-16 01:40Z) Upgraded reader metadata to include heartbeat + CRC and updated retention for TTL + MAX_RETENTION_LAG.
- [x] (2026-01-16 01:50Z) Ran `cargo test` and recorded a green run in Artifacts.
- [x] (2026-01-16 21:30Z) Marked this plan archived; Phase 8 on the roadmap is now Hardening & Benchmarks.

## Surprises & Discoveries

- Observation: `#[repr(C, align(64))]` alone produced a 128-byte header due to implicit padding after the commit word.
  Evidence: `header_size_and_alignment` test failed until an explicit `_pad0: u32` and 32-byte tail pad were added.

## Decision Log

- Decision: Encode committed payload length as `commit_len = payload_len + 1` so zero-length payloads are legal while `commit_len == 0` remains the uncommitted sentinel.
  Rationale: Prevents ambiguity between “not committed” and “empty payload” without adding an extra flag.
  Date/Author: 2026-01-16, Codex
- Decision: Use a PAD record with `type_id = 0xFFFF` to repair crash tails and explicitly mark unusable slack at the end of a segment.
  Rationale: Readers can deterministically skip PAD records without blocking on uncommitted garbage.
  Date/Author: 2026-01-16, Codex
- Decision: Keep Linux futex as the primary wait primitive, with a sleep fallback on non-Linux to keep local builds working.
  Rationale: Production target is Linux; fallback avoids breaking macOS development.
  Date/Author: 2026-01-16, Codex
- Decision: Add an explicit `_pad0: u32` field in `MessageHeader` to preserve a 64-byte size with `align(64)` while keeping the on-disk layout explicit.
  Rationale: Prevents implicit padding from doubling the struct size while keeping serialization offsets controlled.
  Date/Author: 2026-01-16, Codex
- Decision: When no active readers remain, retention treats `head_segment` as the minimum to allow deletion of older segments.
  Rationale: Prevents unbounded disk growth when all readers are dead or lagging beyond the retention threshold.
  Date/Author: 2026-01-16, Codex
- Decision: Archive this plan to avoid conflict with the roadmap’s Phase 8 (Hardening & Benchmarks).
  Rationale: The roadmap was updated and this work is already completed; keeping it as an active Phase 8 plan would be misleading.
  Date/Author: 2026-01-16, Codex

## Outcomes & Retrospective

The implementation now matches the approved architecture: commit_len-based visibility, PAD tail repair, control.meta + futex waits, and retention that ignores dead/lagging readers. Tests were updated to the new API and all pass. A new lagging-reader test demonstrates MAX_RETENTION_LAG behavior without requiring massive data writes.

## Context and Orientation

The current implementation uses a `flags` byte to mark commits, stores reader positions as `{segment_id, offset}`, and relies on `eventfd` for notifications. The updated design replaces that with a commit-length word (`commit_len`), an explicit `control.meta` shared block (with futex wait/wake), PAD records for recovery, and reader metadata that includes heartbeats and corruption detection. The relevant files are:

- `docs/DESIGN.md` for the normative protocol.
- `src/header.rs` for record layout and CRC helpers.
- `src/writer.rs` and `src/reader.rs` for append/read logic and API surface.
- `src/segment.rs` for segment headers, paths, and offsets.
- `src/retention.rs` for cleanup logic.
- `src/notifier.rs` for current eventfd/inotify logic (to be superseded by futex-based wait). 

We will introduce a new `src/control.rs` module for the control block, and a `src/wait.rs` module for futex wait/wake on Linux. Existing tests under `tests/` must be updated to reflect the new format and API.

## Plan of Work

This section is retained for historical context only; no further work is planned in `chronicle-core` for this phase.

Next, implement `control.meta` as a small shared mmap file and add a `ControlBlock` view that exposes atomic reads/writes of `current_segment`, `write_offset`, and `notify_seq`, with proper initialization via `init_state` and `magic/version` checks. Wire `Queue::open_publisher` and `Queue::open_subscriber` to initialize or wait on this block. Implement a Linux futex wait/wake helper and have `QueueReader::wait` use it via the control block’s `notify_seq`. On non-Linux builds, keep a short sleep fallback so tests can run locally.

Then, implement the recovery scan and tail repair in `open_publisher`. The writer must scan from the last checkpoint, find the first uncommitted or invalid record, and repair the tail by writing a PAD record spanning the remaining bytes if possible or sealing and rolling otherwise. This ensures readers never block indefinitely on garbage.

Finally, upgrade reader metadata to a double-slot file with generation and CRC, storing `{segment_id, offset, last_heartbeat_ns}`. Add a small CRC helper for metadata, update `QueueReader::commit` to persist offset + heartbeat, and add a `heartbeat()` or `maybe_heartbeat()` that keeps the file fresh even when idle. Update retention to ignore readers that are beyond TTL or beyond `MAX_RETENTION_LAG` relative to the writer head offset. Update retention tests to cover lagging-reader behavior.

## Concrete Steps

No further steps in this repository. The listed edits below reflect the already-completed alignment work.

  - `src/header.rs`: replace flags/length with commit_len-based layout helpers and constants. Add `const PAD_TYPE_ID: u16 = 0xFFFF` and `const HEADER_SIZE: usize = 64` plus `const RECORD_ALIGN: usize = 64` and `const MAX_PAYLOAD_LEN: usize = u32::MAX as usize - 1`.
  - `src/segment.rs`: add segment header constants (`SEG_MAGIC`, `SEG_VERSION`, `SEG_FLAG_SEALED`), `SEG_DATA_OFFSET`, and helpers to write/read the segment header and seal it. Ensure new segments are created with a header and data offset set to 64.
  - `src/control.rs` (new): define `ControlBlock` with cache-line-separated fields and provide `ControlFile::create` and `ControlFile::open` that follow the init protocol. Provide accessors for `current_segment`, `write_offset`, and `notify_seq`.
  - `src/wait.rs` (new): implement `futex_wait`/`futex_wake` on Linux and a fallback sleep wait elsewhere.
  - `src/writer.rs`: rename/replace `Queue::open` with `open_publisher`, implement writer.lock acquisition with PID + start_time_ticks check, map `control.meta`, use commit_len protocol, add tail recovery, and use control block values for segment id/offset. Replace eventfd notifier calls with `control.notify_seq` increments and futex wake.
  - `src/reader.rs`: add `open_subscriber`, use `control.meta`, read commit_len with Acquire, skip PAD records, align record advances, and update wait() to use futex wait on notify_seq with optional timeout.
  - `src/retention.rs`: update to read new reader metadata, enforce TTL and MAX_RETENTION_LAG, and compute lag using a global head offset `(segment_id * SEGMENT_SIZE + write_offset)`.
  - `src/error.rs`: extend error enum to include `PayloadTooLarge`, `QueueFull`, `WriterAlreadyActive`, `UnsupportedVersion`, and `CorruptMetadata` to align with the design’s error semantics.
  - `src/lib.rs`: update public API to include `Queue::open_publisher`, `Queue::open_subscriber`, `QueueWriter::flush_async/flush_sync`, `QueueReader::wait(timeout)`, and `MessageView`.
  - Update tests in `tests/` to reflect the new API and commit_len protocol, and add a recovery/PAD test plus a retention lag test.

Run the test suite from the repository root after each major step:

  (cwd: /Users/zjx/Documents/chronicle-rs)
  cargo test

## Validation and Acceptance

Acceptance for this archived phase was met when:

- `cargo test` is green and new tests cover PAD skipping, recovery tail repair, and retention lag policy.
- Appends write commit_len = payload_len + 1 and readers only consume records when commit_len > 0.
- Readers skip PAD records and do not stall on crash-tail garbage.
- `QueueReader::wait()` blocks and wakes via futex on Linux (sleep fallback on non-Linux).
- Retention ignores dead readers (TTL exceeded) and lagging readers (MAX_RETENTION_LAG exceeded).

## Idempotence and Recovery

All steps remain safe to rerun. New metadata formats are designed to tolerate partial writes by using a double-slot + CRC scheme. If tests fail due to stale on-disk formats, delete the temporary test directories (created via `tempfile`) and rerun. If control.meta is corrupt, delete the queue directory and recreate it via open_publisher.

## Artifacts and Notes

Green `cargo test` run:

    running 2 tests
    test header::tests::crc_matches_known_payload ... ok
    test header::tests::header_size_and_alignment ... ok
    test result: ok. 2 passed; 0 failed

    running 1 test
    test append_two_messages_and_recover ... ok
    test result: ok. 1 passed; 0 failed

    running 2 tests
    test merge_orders_by_timestamp_and_source ... ok
    test merge_returns_none_when_empty ... ok
    test result: ok. 2 passed; 0 failed

    running 1 test
    test read_commit_and_recover ... ok
    test result: ok. 1 passed; 0 failed

    running 1 test
    test retention_deletes_only_when_all_readers_advance ... ok
    test result: ok. 1 passed; 0 failed

    running 1 test
    test retention_ignores_lagging_reader ... ok
    test result: ok. 1 passed; 0 failed

    running 1 test
    test append_read_round_trip ... ok
    test result: ok. 1 passed; 0 failed

    running 1 test
    test segment_rollover_and_read_across ... ok
    test result: ok. 1 passed; 0 failed

## Interfaces and Dependencies

Public API targets at the end of this phase:

In `src/lib.rs`:

    impl Queue {
        pub fn open_publisher(path: impl AsRef<std::path::Path>) -> crate::Result<QueueWriter>;
        pub fn open_subscriber(path: impl AsRef<std::path::Path>, reader: &str) -> crate::Result<QueueReader>;
    }

    pub enum WaitStrategy {
        Hybrid { spin_us: u32 },
        BusyPoll(std::time::Duration),
    }

    pub struct MessageView<'a> {
        pub seq: u64,
        pub timestamp_ns: u64,
        pub type_id: u16,
        pub payload: &'a [u8],
    }

Reader metadata format (double-slot):

    struct ReaderMetaSlot {
        segment_id: u64,
        offset: u64,
        last_heartbeat_ns: u64,
        generation: u64,
        crc32: u32,
        _pad: [u8; 4],
    }

Two slots are stored back-to-back; the reader loads the highest generation with a valid CRC.

Change note: ExecPlan created for protocol alignment phase (2026-01-16).
Change note: Updated progress, decisions, and artifacts after implementation and green tests (2026-01-16).
Change note: 2026-01-16 archived this plan to resolve the phase-numbering conflict with Phase 8 (Hardening & Benchmarks).
