# Control v2 with dynamic segment sizing

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan is governed by `PLANS.md` at the repository root and must be maintained in accordance with it.

## Purpose / Big Picture

After this change, a queue’s segment size is no longer hard-coded in binaries. Writers and readers discover the segment size from `control.meta` (Control v2) and use that value consistently, with a default of 128 MiB for new queues. The control file now includes a generation counter so observers can read a consistent `(current_segment, write_offset)` pair without torn reads during segment rolls. The behavior is visible by creating a queue, verifying that `control.meta` reports the segment size, and by running tests that exercise segment rollover and retention with a small configured segment size.

## Progress

- [x] (2026-01-17 03:17Z) Read `NOTES.md` and the current implementations of `control.rs`, `segment.rs`, `writer.rs`, `reader.rs`, `retention.rs`, and the integration tests.
- [x] (2026-01-17 03:50Z) Update `ControlBlock` layout to version 2 with `segment_size` and a segment generation counter, and wire new accessors.
- [x] (2026-01-17 03:52Z) Make segment size dynamic end-to-end (segment creation/open, reader/writer bounds, retention math) using the value from `control.meta`.
- [x] (2026-01-17 03:56Z) Update tests to use explicit small segment sizes where needed, plus adapt to API changes.
- [x] (2026-01-17 03:58Z) Update `docs/DESIGN.md` for Control v2 and dynamic segment sizing.
- [x] (2026-01-17 04:00Z) Run `cargo test -p chronicle-core` and confirm new/updated tests pass.

## Surprises & Discoveries

- Observation: Existing retention logic already ignores readers that are stale past a TTL or lagging by more than a max retention window, which reduces but does not eliminate backpressure risk.
  Evidence: `crates/1-primitives/chronicle-core/src/retention.rs` uses `READER_TTL_NS` and `MAX_RETENTION_LAG` checks.

## Decision Log

- Decision: Bump `control.meta` to version 2 and fail fast on version mismatch.
  Rationale: The layout and semantics are changing (segment sizing, seqlock generation), and v1 readers would be unsafe.
  Date/Author: 2026-01-17, user request.

- Decision: Store `segment_size` in `control.meta` and treat it as the single source of truth.
  Rationale: Ensures all processes share the same queue geometry and avoids corruption from mismatched configs.
  Date/Author: 2026-01-17, user request.

- Decision: Default segment size to 128 MiB, and require readers/writers to trust `control.meta` for existing queues.
  Rationale: Large enough for production, but still configurable for tests and local environments. Mandatory auto-detection prevents mismatched writers.
  Date/Author: 2026-01-17, user request.

## Outcomes & Retrospective

Control v2 and dynamic segment sizing are implemented across writer, reader, segment, and retention logic. Integration tests now use small segment sizes to keep disk usage low, and the core test suite passes. Remaining work is limited to any future policy tuning (not part of this change).

## Context and Orientation

The data plane lives in `crates/1-primitives/chronicle-core`. Segment file sizing is currently hard-coded via `SEGMENT_SIZE` in `crates/1-primitives/chronicle-core/src/segment.rs`, and that constant is used by writer, reader, and retention logic in `crates/1-primitives/chronicle-core/src/writer.rs`, `crates/1-primitives/chronicle-core/src/reader.rs`, and `crates/1-primitives/chronicle-core/src/retention.rs`. Control metadata is stored in a memory-mapped `control.meta` file defined in `crates/1-primitives/chronicle-core/src/control.rs` using `ControlBlock` (version 1). The writer’s roll logic updates `current_segment` and `write_offset` independently, which can expose torn reads to monitoring tools. Integration tests in `crates/1-primitives/chronicle-core/tests` expect fixed segment sizing and use the segment helpers directly.

## Plan of Work

First, update `crates/1-primitives/chronicle-core/src/control.rs` to Control v2 by adding `segment_size` to the constant section and a `segment_gen` (generation) counter near the reader-hot fields, adjusting padding to keep 128-byte cacheline alignment. Update `ControlFile::create` to accept a `segment_size` argument, set it during initialization, and bump `CTRL_VERSION` to 2. Add accessors to read the segment size and to set and read a consistent `(current_segment, write_offset)` pair using a seqlock pattern around the generation counter.

Second, replace the compile-time segment size assumptions with dynamic sizing. In `crates/1-primitives/chronicle-core/src/segment.rs`, introduce a default constant (128 MiB) and change `open_segment`, `create_segment`, and `repair_unsealed_tail` to accept a `segment_size` parameter. Add a helper to validate `segment_size` bounds and convert from `u64` to `usize`. Thread `segment_size` through `writer.rs` (tail scan, roll, append bounds, retention math) and `reader.rs` (read bounds, segment advance, commit checks) by storing it in the writer/reader struct. Update `crates/1-primitives/chronicle-core/src/retention.rs` to accept `segment_size` as an explicit parameter and adjust all call sites accordingly.

Third, update tests and small call sites. Integration tests should opt into a small segment size by creating a `WriterConfig` with `segment_size` set, so tests do not allocate large files. Any direct calls to `create_segment`, `open_segment`, or retention helpers should pass the test segment size explicitly.

Finally, update `docs/DESIGN.md` to describe Control v2, the seqlock-protected segment index, and the fact that segment size is stored in `control.meta` rather than being fixed at compile time.

## Concrete Steps

1) Edit `crates/1-primitives/chronicle-core/src/control.rs` to bump `CTRL_VERSION` to 2, extend `ControlBlock` with `segment_size` and `segment_gen`, and add `ControlFile::segment_size()` plus a seqlock-based `segment_index()` and `set_segment_index()`.

2) Edit `crates/1-primitives/chronicle-core/src/segment.rs` to define `DEFAULT_SEGMENT_SIZE` (128 MiB) and update segment functions to take `segment_size`. Add a validation helper to safely convert from `u64` to `usize` and enforce a minimum of `SEG_DATA_OFFSET + HEADER_SIZE`.

3) Edit `crates/1-primitives/chronicle-core/src/writer.rs` to store `segment_size`, read it from `control.meta` on open (ignoring local config when the queue already exists), and pass it through segment/retention helpers. Replace roll updates with `set_segment_index()`.

4) Edit `crates/1-primitives/chronicle-core/src/reader.rs` to store `segment_size` from control, use it for bounds and segment advance logic, and pass it to segment helpers.

5) Edit `crates/1-primitives/chronicle-core/src/retention.rs` to accept `segment_size` as a parameter, update computations, and adjust call sites.

6) Update integration tests under `crates/1-primitives/chronicle-core/tests` and in-module tests in `crates/1-primitives/chronicle-core/src/writer.rs` to use a small test segment size and match new function signatures.

7) Update `docs/DESIGN.md` with the new Control v2 layout and dynamic segment size semantics.

## Validation and Acceptance

Run tests from the repo root:

    cd /Users/zjx/Documents/chronicle-rs
    cargo test -p chronicle-core

Expected outcome: all core tests pass. In particular, `segment_rollover_and_read_across` should still pass with a small configured segment size, and retention tests should still delete expected segments. If any test uses default sizing, it should continue to function but may create larger files.

## Idempotence and Recovery

All edits are safe to reapply. If a test fails due to outdated on-disk formats (Control v1), remove the temporary test directory and rerun. No destructive operations are performed outside temporary directories.

## Artifacts and Notes

- Example verification snippet for tests (illustrative only):

    running 1 test
    test segment_rollover_and_read_across ... ok

- Control v2 migration is fail-fast: any existing v1 `control.meta` will be rejected by `UnsupportedVersion` or a size mismatch error.

## Interfaces and Dependencies

- `crates/1-primitives/chronicle-core/src/control.rs` must define `ControlBlock` with `segment_size: AtomicU64` and `segment_gen: AtomicU32`, `CTRL_VERSION = 2`, and new methods `segment_size()` and `segment_index()` plus `set_segment_index()`.
- `crates/1-primitives/chronicle-core/src/segment.rs` must expose `DEFAULT_SEGMENT_SIZE` and accept `segment_size: usize` in `open_segment`, `create_segment`, and `repair_unsealed_tail`.
- `crates/1-primitives/chronicle-core/src/writer.rs` must store `segment_size: usize` in `QueueWriter` and use it consistently for bounds checks, roll logic, and retention.
- `crates/1-primitives/chronicle-core/src/reader.rs` must store `segment_size: usize` in `QueueReader` and use it for bounds checks and segment transitions.
- `crates/1-primitives/chronicle-core/src/retention.rs` functions must accept `segment_size: u64` and use it for global position math.

Plan update note: Initial plan created from user requirements to bump control version, store segment size in control, and default to 128 MiB with mandatory auto-detection.
Plan update note: Marked core implementation, tests, and design doc changes complete; tests remain to be run.
Plan update note: Recorded successful test run and updated outcomes.
