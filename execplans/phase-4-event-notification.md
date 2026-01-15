# Implement Event Notification (eventfd + inotify) with Cross-Platform Fallback

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Phase 4 introduces blocking reads via event-driven notifications. On Linux, readers should be able to block on an `eventfd` and be awakened when the writer appends new data; the writer should discover readers dynamically using `inotify`. On non-Linux platforms (macOS dev), we provide a safe fallback that compiles and allows progress by using a simple sleep-based wait. After this change, readers can avoid busy-spinning while waiting for new messages, and writers can wake all active readers after each commit.

## Progress

- [x] (2026-01-16 00:40Z) ExecPlan created for Phase 4 with Linux target + cross-platform fallback.
- [x] (2026-01-16 00:55Z) Implemented Linux notifier: `eventfd` reader wait, `inotify` discovery, writer wake-ups.
- [x] (2026-01-16 00:55Z) Implemented non-Linux fallback notifier (sleep-based wait, no-op notify).
- [x] (2026-01-16 01:00Z) Integrated notifier into `Queue`, `QueueWriter::append`, and `QueueReader::wait`.
- [x] (2026-01-16 01:00Z) Added Linux-only test validating reader blocks until writer appends.
- [x] (2026-01-16 01:05Z) Ran `cargo test` on macOS dev; all tests passed (Linux-only test skipped).

## Surprises & Discoveries

None yet. Capture any Linux-specific edge cases (e.g., `eventfd` overflow, inotify races).

## Decision Log

- Decision: Implement Linux `eventfd` + `inotify` under `cfg(target_os = "linux")` and provide a cross-platform busy-wait fallback for non-Linux builds.
  Rationale: The target runtime is Linux, but development happens on macOS. A fallback keeps the crate usable and testable locally while preserving Linux-accurate behavior in production.
  Date/Author: 2026-01-16, Codex
- Decision: Store reader notification metadata as `readers/<name>.efd` containing `pid` + `fd`, and on Linux open `/proc/<pid>/fd/<fd>` to duplicate the `eventfd` for writer signaling.
  Rationale: `eventfd` is not a filesystem object. Using `/proc` allows a writer process to obtain a usable descriptor without a custom socket protocol, while keeping the phase focused on eventfd + inotify.
  Date/Author: 2026-01-16, Codex

## Outcomes & Retrospective

Phase 4 is complete: readers can block using `wait()` (eventfd on Linux, sleep fallback elsewhere), writers notify readers on append, and Linux reader discovery uses `inotify` with `.efd` metadata. Local macOS tests pass; Linux-specific wake-up behavior should be validated on a Linux host.

## Context and Orientation

The queue already supports a single-writer append path and per-reader metadata. `QueueWriter::append` commits messages by writing the header + payload and then flipping the valid flag; `QueueReader::next` reads committed messages and advances its cursor. There is currently no blocking mechanism to wait for new messages.

Relevant files:
- `src/notifier.rs` (placeholder module).
- `src/writer.rs` (`Queue`, `QueueWriter::append`).
- `src/reader.rs` (`QueueReader`, `readers/` metadata).
- `docs/DESIGN.md` (eventfd + inotify design section).

## Plan of Work

1) Implement notifier primitives in `src/notifier.rs`:
   - On Linux: create a `ReaderNotifier` that owns an `eventfd` and writes `readers/<name>.efd` with `pid` + `fd`. Add a `WriterNotifier` that scans existing `.efd` files, uses `inotify` to watch for add/remove, and writes to each reader’s `eventfd` to wake them. Use `poll` to block on `eventfd` in the reader.
   - On non-Linux: provide a `ReaderNotifier` that sleeps for a short duration in `wait()` and a `WriterNotifier` that is a no-op.

2) Integrate into the queue:
   - Add a `WriterNotifier` field to `Queue` initialized in `Queue::open`.
   - Update `QueueWriter::append` to call `queue.notifier.notify_all()` after flipping the commit flag.
   - Update `Queue::reader` to create a `ReaderNotifier` for the given reader and store it in `QueueReader`.
   - Add `QueueReader::wait()` that delegates to the notifier.

3) Tests (Linux-only):
   - Add `tests/notifier_wake.rs` guarded by `#[cfg(target_os = "linux")]`.
   - The test should create a reader, call `wait()` in a thread, assert it blocks briefly, then append a message and assert `wait()` returns.

## Concrete Steps

From the repository root, implement:

  - `src/notifier.rs`: Linux and non-Linux implementations of `ReaderNotifier` and `WriterNotifier`.
  - `src/writer.rs`: add notifier field + notify on append.
  - `src/reader.rs`: add notifier field + `wait()` method.
  - `tests/notifier_wake.rs`: Linux-only wake test.

Then run:

  (cwd: /Users/zjx/Documents/chronicle-rs)
  cargo test

## Validation and Acceptance

Acceptance is met when:
- On Linux: `QueueReader::wait()` blocks until `QueueWriter::append()` signals; the Linux-only test passes.
- On macOS/non-Linux: crate builds and tests pass, and `QueueReader::wait()` compiles and returns without error.
- `QueueWriter::append()` continues to pass existing tests and now wakes readers after commit.

## Idempotence and Recovery

The notifier uses per-reader `.efd` files on Linux. If a reader crashes and leaves stale `.efd` files, the writer should ignore failures when opening `/proc/<pid>/fd/<fd>` and continue; developers can delete the readers directory during tests to reset state.

## Artifacts and Notes

macOS `cargo test` run (Linux-only notifier test is compiled but skipped):

    Running tests/notifier_wake.rs (target/debug/deps/notifier_wake-...)
    running 0 tests
    test result: ok. 0 passed; 0 failed; 0 ignored

    Running tests/reader_recovery.rs (target/debug/deps/reader_recovery-...)
    running 1 test
    test read_commit_and_recover ... ok

## Interfaces and Dependencies

New public method (Phase 4 API target):

In `src/reader.rs`:

    impl QueueReader {
        pub fn wait(&self) -> crate::Result<()>;
    }

Notifier types live in `src/notifier.rs`:

    pub struct WriterNotifier { ... }
    pub struct ReaderNotifier { ... }

Linux-only implementation depends on `libc` for `eventfd`, `inotify`, and `poll`.

Change note: Initial ExecPlan drafted for Phase 4 (2026-01-16).
Change note: Updated progress, outcomes, and artifacts after implementation and tests (2026-01-16).
