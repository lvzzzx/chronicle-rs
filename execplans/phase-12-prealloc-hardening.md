# Harden Preallocation and Roll Path

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan must be maintained in accordance with `PLANS.md` at the repository root (`PLANS.md`).

## Purpose / Big Picture

After this change, segment preallocation is safe and effective in the current codebase. The writer will not truncate a published preallocated segment, will not consume a stale or mismatched segment, and will not allow background preallocation to touch live data. You can see it working by running new tests that fail before the change and pass after, and by observing that roll no longer calls a truncating create when a preallocated file already exists.

## Progress

- [x] (2026-01-17 00:00Z) Read `crates/1-primitives/chronicle-core/src/writer.rs`, `crates/1-primitives/chronicle-core/src/segment.rs`, and `crates/1-primitives/chronicle-core/src/mmap.rs` to confirm current prealloc/roll behavior and truncation paths.
- [x] (2026-01-17 00:00Z) Add temp-file preallocation and publish-by-rename with segment id tagging and stale-result discard.
- [x] (2026-01-17 00:00Z) Update roll fallback to open existing segments without truncation before creating new ones.
- [x] (2026-01-17 00:00Z) Add tests that prove preallocated segments are not truncated and stale preallocs are not consumed.
- [x] (2026-01-17 00:00Z) Add a minimal directory-lock contention probe and capture baseline results.
- [x] (2026-01-17 00:00Z) Run `cargo test -p chronicle-core` and capture expected output snippets.

## Surprises & Discoveries

- Observation: Preallocation publishing a next-segment file caused the reopen path to advance even when the current segment was not sealed, breaking append recovery.
  Evidence: `append_two_messages_and_recover` failed with `assertion failed: commit_c > 0` until the reopen logic gated advancement on the sealed flag.

## Decision Log

- Decision: Use a temp file plus atomic publish-by-rename for preallocated segments, and tag prepared mmaps with `segment_id`.
  Rationale: Prevents background prefault from touching live segments and blocks stale prealloc consumption.
  Date/Author: 2026-01-17 / assistant

- Decision: Add a non-truncating open-or-create path for roll fallback.
  Rationale: Avoids `truncate(true)` when a preallocated file already exists.
  Date/Author: 2026-01-17 / assistant

- Decision: Use a shared prealloc slot (`Arc<Mutex<Option<PreparedSegment>>>`) for worker-to-writer handoff.
  Rationale: Keeps the hot path non-blocking while making the IPC mechanism explicit and easy to validate.
  Date/Author: 2026-01-17 / assistant

- Decision: Use `MmapFile::create_new` in `open_or_create_segment` to avoid a race where a newly published file is truncated after a NotFound check.
  Rationale: Ensures open-or-create never truncates a preallocated file even if it appears between open and create.
  Date/Author: 2026-01-17 / assistant

- Decision: Gate reopen advancement on `SEG_FLAG_SEALED` only, ignoring `next_path.exists()` alone.
  Rationale: Preallocation publishes the next file before roll; existence is no longer a safe proxy for roll completion.
  Date/Author: 2026-01-17 / assistant

## Outcomes & Retrospective

Preallocation is now safe and non-truncating, stale slots are ignored, and reopen no longer advances on a preallocated next file. All `chronicle-core` tests pass and the directory contention probe reports baseline roll latency for future comparison.

## Context and Orientation

`crates/1-primitives/chronicle-core/src/writer.rs` owns the hot writer path, including `QueueWriter::roll_segment`. Preallocation integration may already exist (for example as a helper on the writer) or may need to be added as part of this work; the plan assumes we will implement or refactor the integration point rather than rely on a specific existing method name. `crates/1-primitives/chronicle-core/src/segment.rs` builds and opens segment files, and `crates/1-primitives/chronicle-core/src/mmap.rs` defines `MmapFile::create`, which currently uses `OpenOptions::truncate(true)`. A "segment" is a fixed-size memory-mapped file that holds messages and a header. A "preallocated segment" is a segment prepared ahead of time to avoid allocating and page-faulting on the hot path. The current behavior can truncate a preallocated file on roll, which defeats preallocation and can reintroduce hot-path I/O costs.

## Plan of Work

First, add a safe preallocation workflow in `crates/1-primitives/chronicle-core/src/segment.rs` that writes into a temp path and publishes atomically (rename with no-replace) only after preparation completes. This ensures the background thread never writes into the live segment path. Then, add a clear handoff mechanism between the sidecar worker and the writer: a shared slot stored in `QueueWriter` and an `Arc` clone held by the worker so the writer can atomically consume prepared segments on roll. The slot must include the `segment_id`, and stale results must be discarded if the expected id has moved on. Next, update the roll fallback in `crates/1-primitives/chronicle-core/src/writer.rs` to prefer a non-truncating open of an existing segment, only creating a new one when the file is missing (using a create-new path to avoid truncation races). Finally, add targeted unit tests in `crates/1-primitives/chronicle-core/src/segment.rs` and `crates/1-primitives/chronicle-core/src/writer.rs` to prove preallocated segments are not truncated and stale prepared segments are not consumed. As a separate, small probe, add a minimal contention measurement to quantify directory lock impact during retention and roll.

## Concrete Steps

1) Implement temp-segment preallocation and publish helpers in `crates/1-primitives/chronicle-core/src/segment.rs`. The helper set includes a function that builds a temp filename, a function that prepares a temp segment by writing the header and prefaulting pages, and a function that publishes via atomic rename without replacement. The temp helper must not touch the final segment path at any point.

2) Add a non-truncating open-or-create helper in `crates/1-primitives/chronicle-core/src/segment.rs`. The helper must attempt `open_segment` first and only call a create-new path if the file is missing, retrying `open_segment` if a file appears between checks.

3) Update `QueueWriter` in `crates/1-primitives/chronicle-core/src/writer.rs` to hold a shared preallocation slot, for example `Arc<Mutex<Option<PreparedSegment>>>`, where `PreparedSegment` includes `segment_id` and `mmap`. The sidecar worker clones the `Arc` and installs prepared segments into the slot; the writer consumes the slot during roll by locking and validating that the segment id matches `next_segment_id`. Stale results are discarded by checking an atomic expected id before publish and by ignoring mismatched ids on consumption.

4) Update the roll fallback to call the non-truncating open-or-create helper instead of `prepare_segment`, so a published preallocation never gets truncated by the hot path.

5) Add unit tests in `crates/1-primitives/chronicle-core/src/segment.rs` and `crates/1-primitives/chronicle-core/src/writer.rs` that write a sentinel value into a preallocated segment, then verify it survives `open_or_create_segment`, and that stale prepared segments are rejected during roll. The tests should fail before the change and pass after.

6) Add a minimal contention probe, preferably as a new bench under `crates/1-primitives/chronicle-core/benches/dir_contention.rs`, that spawns a writer rolling segments while a background thread deletes old segments. Record the distribution of roll latency and include the output snippet in `Artifacts and Notes`.

7) Run tests from the repo root and capture the results. Use:

    cargo test -p chronicle-core

Expected output snippet (example):

    test segment::tests::open_or_create_preserves_preallocated ... ok
    test writer::tests::stale_prealloc_is_ignored ... ok

## Validation and Acceptance

The change is accepted when the new tests pass, and roll no longer truncates a published preallocated segment. A human verifier should be able to run the test suite and see the new preallocation safety tests pass, and run the new contention bench to observe a latency distribution that can be compared across changes.

## Idempotence and Recovery

All steps are additive and safe to re-run. If the new tests fail, re-run after fixing the relevant module. If the contention bench is noisy, re-run on an otherwise idle system and record the variance.

## Artifacts and Notes

Include concise snippets of test output and any contention benchmark summary, for example:

    $ cargo test -p chronicle-core
    ...
    test segment::tests::open_or_create_preserves_preallocated ... ok
    test writer::tests::stale_prealloc_is_ignored ... ok

    $ cargo bench -p chronicle-core --bench dir_contention
    ...
    roll p50: 5.06825ms, p99: 10.176208ms

## Interfaces and Dependencies

Add or update the following public helpers in `crates/1-primitives/chronicle-core/src/segment.rs` and use them in `crates/1-primitives/chronicle-core/src/writer.rs`:

    pub fn segment_temp_path(root: &Path, id: u64) -> PathBuf
    pub fn prepare_segment_temp(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile>
    pub fn publish_segment(temp: &Path, final_path: &Path) -> Result<()>
    pub fn open_or_create_segment(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile>

Add a non-truncating constructor in `crates/1-primitives/chronicle-core/src/mmap.rs`:

    pub fn create_new(path: &Path, len: usize) -> Result<Self>

In `crates/1-primitives/chronicle-core/src/writer.rs`, add a `PreparedSegment` type and store it in a shared slot (e.g., `Arc<Mutex<Option<PreparedSegment>>>`) along with an `AtomicU64` expected id used to discard stale prealloc results. The sidecar thread holds a clone of the slot `Arc` and writes into it; the writer consumes it during roll.

Plan revision note: Initial ExecPlan written to address preallocation truncation and safety issues raised in review, and to add a contention probe; no implementation started yet.
Plan revision note: Updated context to avoid assuming a specific preallocation method name, and clarified the worker-to-writer handoff via a shared slot after review feedback.
Plan revision note: Updated progress, decisions, and artifacts after implementation and test/bench runs; adjusted test locations and open-or-create details to match the code.
