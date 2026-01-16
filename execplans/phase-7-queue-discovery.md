# Implement Queue Discovery for Multi-Process Fan-In (Phase 7)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

After this change, a long-lived router process can discover new strategy queues on disk without restart, attach readers dynamically, and detach them when queue directories are removed. The observable outcome is that creating a new queue directory plus a READY marker causes the router to start reading from that queue, and deleting the directory causes the router to drop the reader and free file descriptors. You can see this behavior by running the new discovery tests and by executing a small end-to-end scenario in a test that creates and removes queue directories while the router stays running.

## Progress

- [x] (2026-01-15 23:40Z) ExecPlan drafted for Phase 7 queue discovery.
- [ ] Add discovery module using a cross-platform watcher and READY protocol.
- [ ] Extend fan-in reader to allow dynamic add/remove of queue readers.
- [ ] Add tests that create/delete READY queues and assert attach/detach behavior.
- [ ] Run `cargo test` and capture passing outputs in the plan.

## Surprises & Discoveries

None yet. Capture any watcher behavior differences (macOS vs Linux), or missed-event edge cases with evidence.

## Decision Log

- Decision: Use the `notify` crate for filesystem watching, rather than raw OS APIs.
  Rationale: The project must work on macOS and Linux; `notify` abstracts inotify/kqueue/FSEvents so we do not hard-code OS-specific logic.
  Date/Author: 2026-01-15, Codex
- Decision: READY file format is a minimal text payload with `version=1` on the first line.
  Rationale: Simple parsing, easy to version-gate, and matches the design requirement for a lightweight version handshake.
  Date/Author: 2026-01-15, Codex
- Decision: Fan-in sources use stable numeric IDs assigned at attach time; detached sources leave holes in the source list.
  Rationale: This preserves deterministic tie-breaking by source ID even when readers are removed.
  Date/Author: 2026-01-15, Codex
- Decision: Watcher is established before the initial scan, and scan deduplicates against watch events.
  Rationale: Avoids the startup race where a queue is created between scan and watch.
  Date/Author: 2026-01-15, Codex

## Outcomes & Retrospective

Not started yet. Summarize outcomes, remaining gaps, and lessons after implementation.

## Context and Orientation

The current codebase already supports single-queue writers/readers and a multi-queue `FanInReader` that merges messages by `(timestamp_ns, source_index)`. This is implemented in `src/writer.rs`, `src/reader.rs`, and `src/merge.rs`. There is no discovery layer yet. The design in `docs/DESIGN.md` defines a READY marker protocol: each strategy queue lives under a shared root directory, and the writer creates a READY file (via atomic rename) when the queue is ready for attachment. The router process must watch the root directory, attach new queues by opening a `QueueReader` with a unique router name, and detach when a queue directory is deleted. This phase implements that discovery layer as a module and extends the fan-in reader to support dynamic add/remove.

Definitions used in this plan:

- READY marker: a file named `READY` in a queue directory. Its content includes a version line `version=1`.
- Router: a long-lived process that discovers queues and merges their streams.
- Attach: create a `QueueReader` for a discovered queue and register it with the fan-in reader.
- Detach: remove the `QueueReader` when the queue directory is deleted, releasing mmaps and file handles.

Key files:

- `src/merge.rs`: fan-in merge reader from Phase 6.
- `src/reader.rs`: `QueueReader`, used for attaching queues.
- `src/writer.rs`: `QueueWriter`, used by strategies to create queues.
- `docs/DESIGN.md`: discovery protocol description.

New files in this phase:

- `src/discovery.rs`: discovery layer (watch + scan + READY parsing).
- `tests/discovery_fanin.rs`: integration tests for attach/detach behavior.

## Plan of Work

First, add a new discovery module under `src/discovery.rs`. Define a `QueueDiscovery` struct that takes a root directory, a router reader name, and a supported READY version. The constructor must create the root directory (if it does not exist), start a cross-platform watcher using `notify::recommended_watcher`, and then scan existing subdirectories for READY files. Scanning should assume a single level of strategy directories under the root. When a READY file is found, parse the version line and attach the queue only if the version is less than or equal to the supported version. The discovery layer must deduplicate: if a queue is already attached, ignore subsequent READY events for that directory.

Next, define a `DiscoveryEvent` enum with `Attach` and `Detach` variants. The `Attach` variant carries the queue directory path, a stable source ID, and a `QueueReader`. The `Detach` variant carries the same path and source ID. The `QueueDiscovery::poll` method should drain the watcher channel (using `try_recv` in a loop) and return any attach/detach events that were discovered. For events, treat creation or rename of `READY` as attach, and treat deletion of the queue directory or `READY` as detach. When a queue directory is deleted, remove the reader from the attachment map to avoid file descriptor leaks.

Then, extend `FanInReader` in `src/merge.rs` to support dynamic readers. Convert its `readers` and `pending` vectors into parallel `Vec<Option<...>>` collections. Add methods `add_reader` (returns a new source ID) and `remove_reader` (sets the slot to None and clears pending state). Update `next` and `commit` to skip or error on missing sources while keeping the deterministic `(timestamp_ns, source_id)` ordering for active sources.

Finally, add tests in `tests/discovery_fanin.rs`. The first test should construct a discovery instance, then create a queue directory and READY file, and assert that an attach event appears within a bounded polling loop. The second test should delete the queue directory and assert that a detach event is emitted and the source is removed. Use short sleeps with a timeout loop to avoid flaky tests on slower machines. Keep READY contents minimal, for example `version=1\n`.

## Concrete Steps

All commands should be run from the repository root:

    (cwd: /Users/zjx/Documents/chronicle-rs)

1) Add the `notify` dependency in `Cargo.toml`.

2) Create `src/discovery.rs` with:
   - `QueueDiscovery` + `DiscoveryEvent`
   - READY parsing helpers
   - Watcher setup and initial scan

3) Update `src/merge.rs` to support `add_reader` and `remove_reader`.

4) Add `tests/discovery_fanin.rs` with attach/detach tests.

5) Run tests:

    cargo test

Expected (example) output excerpt:

    running 2 tests
    test discovery_attaches_and_detaches ... ok
    test discovery_skips_newer_ready_version ... ok

## Validation and Acceptance

Acceptance is met when:

- Creating a queue directory plus a READY file results in a `DiscoveryEvent::Attach` with a live `QueueReader`.
- Removing the queue directory results in `DiscoveryEvent::Detach` and the fan-in reader no longer attempts to read from that source.
- Running `cargo test` passes, and the new tests fail prior to the implementation and pass after.

## Idempotence and Recovery

These steps are safe to rerun. If the watcher fails to initialize, verify that the root directory exists and that the `notify` crate is added correctly. If the discovery tests are flaky due to filesystem event timing, increase the polling timeout (for example, from 250ms to 1s) and rerun `cargo test`.

## Artifacts and Notes

READY file example (written by strategies):

    version=1

Example event sequence (conceptual):

    Attach: path=/tmp/queues/strategy_a, source=0
    Detach: path=/tmp/queues/strategy_a, source=0

## Interfaces and Dependencies

Add the following dependency to `Cargo.toml`:

    notify = "6"

In `src/discovery.rs`, define:

    pub enum DiscoveryEvent {
        Attach { source: usize, path: std::path::PathBuf, reader: crate::reader::QueueReader },
        Detach { source: usize, path: std::path::PathBuf },
    }

    pub struct QueueDiscovery {
        pub fn new(root: impl AsRef<std::path::Path>, reader_name: &str, supported_version: u32) -> crate::Result<Self>;
        pub fn poll(&mut self) -> crate::Result<Vec<DiscoveryEvent>>;
    }

In `src/merge.rs`, extend:

    impl FanInReader {
        pub fn add_reader(&mut self, reader: crate::reader::QueueReader) -> usize;
        pub fn remove_reader(&mut self, source: usize) -> crate::Result<()>;
    }

Change note: Initial ExecPlan drafted for Phase 7 queue discovery based on the `docs/DESIGN.md` discovery section and Phase 6 fan-in implementation.
