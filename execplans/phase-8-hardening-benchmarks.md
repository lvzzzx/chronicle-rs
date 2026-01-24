# Harden Data Plane With Stress Tests, Soak Tests, and Benchmarks (Phase 8)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Phase 8 hardens the data plane by exercising it under high load, long runtimes, and controlled performance measurement. After this change, a developer can run fast stress tests during normal `cargo test`, run longer soak tests on demand, and collect reproducible throughput/latency benchmarks with `cargo bench` to establish baselines and detect regressions. This makes the queue trustworthy under sustained writer/reader activity and provides concrete numbers for append and read performance.

## Progress

- [x] (2026-01-16 13:39Z) ExecPlan created for Phase 8 hardening and benchmarks.
- [x] (2026-01-16 13:46Z) Added fast stress test and ignored heavy stress test in `crates/1-primitives/chronicle-core/tests/stress_single_writer.rs`.
- [x] (2026-01-16 13:46Z) Added ignored soak test in `crates/1-primitives/chronicle-core/tests/soak_writer_reader.rs`.
- [x] (2026-01-16 13:46Z) Added Criterion benchmarks in `crates/1-primitives/chronicle-core/benches/append.rs` and `crates/1-primitives/chronicle-core/benches/read.rs`.
- [x] (2026-01-16 13:46Z) Updated `crates/1-primitives/chronicle-core/Cargo.toml` with Criterion dev-dependency and bench harness entries.
- [x] (2026-01-16 13:46Z) Documented commands and expected outputs for stress, soak, and benchmark runs.

## Surprises & Discoveries

None yet. Capture any data corruption, performance cliffs, or platform-specific timing behavior with evidence once stress/soak runs are implemented.

## Decision Log

- Decision: Implement stress and soak tests as integration tests under `crates/1-primitives/chronicle-core/tests`, using `#[ignore]` for long runs and environment variables to tune message counts and durations.
  Rationale: This keeps the tests close to the public API, avoids polluting core code, and lets CI run fast tests while humans can trigger heavier runs intentionally.
  Date/Author: 2026-01-16, Codex
- Decision: Use Criterion benchmarks with `harness = false` for `cargo bench` to avoid nightly-only benches.
  Rationale: Criterion is stable, provides statistically meaningful measurements, and integrates with `cargo bench` without relying on unstable Rust features.
  Date/Author: 2026-01-16, Codex
- Decision: Favor deterministic payloads and timestamp injection in stress/bench code to reduce time-based nondeterminism.
  Rationale: Deterministic inputs make it easier to spot data loss or ordering errors and to compare benchmark runs over time.
  Date/Author: 2026-01-16, Codex

## Outcomes & Retrospective

Implemented stress tests, soak test scaffolding, and Criterion benchmarks for append and read throughput. Validation is pending; run `cargo test -p chronicle-core` and `cargo bench -p chronicle-core` to confirm behavior and record baseline numbers.

## Context and Orientation

The data plane lives in `crates/1-primitives/chronicle-core`. Writers append to memory-mapped segment files via `Queue::open_publisher` and `QueueWriter::append` in `crates/1-primitives/chronicle-core/src/writer.rs`. Readers consume committed messages via `Queue::open_subscriber` and `QueueReader::next` in `crates/1-primitives/chronicle-core/src/reader.rs`. Segments roll at `SEGMENT_SIZE` in `crates/1-primitives/chronicle-core/src/segment.rs`, and retention cleanup is handled by `crates/1-primitives/chronicle-core/src/retention.rs`. Integration tests already live in `crates/1-primitives/chronicle-core/tests` and use temporary directories from `tempfile`.

Terminology used in this plan:
“Stress test” means a short, high-volume run that pushes append/read throughput and segment rolling to catch edge cases quickly.
“Soak test” means a longer run that validates stability over time (no corruption, no stalls, no retention regressions).
“Benchmark” means a controlled measurement of throughput or latency that can be repeated to detect regressions.

## Plan of Work

Add a new fast stress test integration file (for example, `crates/1-primitives/chronicle-core/tests/stress_single_writer.rs`) that spawns one writer thread and one or more reader threads. The stress test should write enough messages to roll multiple segments, then verify that readers observe monotonically increasing `seq` values, correct payload contents, and the expected total count. Keep one test unignored with a modest message count so it can run during `cargo test`, and add a heavier `#[ignore]` variant that scales using an environment variable (for example, `CHRONICLE_STRESS_MESSAGES`). Include a periodic call to `QueueWriter::cleanup` during the writer loop to ensure retention cleanup does not panic or corrupt active segments.

Add a soak test integration file (for example, `crates/1-primitives/chronicle-core/tests/soak_writer_reader.rs`) that runs for a configurable duration (for example, `CHRONICLE_SOAK_SECS` with a reasonable default like 30). This test should keep a writer and reader running concurrently, with occasional sleeps to mimic real workloads, and assert that the reader never observes invalid headers or CRC failures and that the final count matches what was written. Mark the soak test `#[ignore]` so it only runs when explicitly requested.

Add Criterion benchmarks under `crates/1-primitives/chronicle-core/benches`. Provide at least two benchmarks: one for append throughput (`append.rs`) that measures steady-state appends with fixed payload sizes, and one for read throughput (`read.rs`) that measures sequential consumption of a prefilled segment set. Use deterministic payloads, `criterion::black_box`, and `iter_batched` or `iter_batched_ref` to ensure each iteration uses a fresh temporary queue directory. Update `crates/1-primitives/chronicle-core/Cargo.toml` to include the `criterion` dev-dependency and `[[bench]]` entries with `harness = false` for each benchmark file.

Document the commands to run the fast stress test, the ignored stress/soak tests, and the benchmarks, including the working directory and any environment variables. Provide short expected output snippets to help confirm success.

## Concrete Steps

Edit or add the following files:

    crates/1-primitives/chronicle-core/tests/stress_single_writer.rs
    crates/1-primitives/chronicle-core/tests/soak_writer_reader.rs
    crates/1-primitives/chronicle-core/benches/append.rs
    crates/1-primitives/chronicle-core/benches/read.rs
    crates/1-primitives/chronicle-core/Cargo.toml

Run fast tests from the repository root:

    (cwd: /Users/zjx/Documents/chronicle-rs)
    cargo test -p chronicle-core

Run stress tests explicitly (examples):

    (cwd: /Users/zjx/Documents/chronicle-rs)
    CHRONICLE_STRESS_MESSAGES=200000 cargo test -p chronicle-core --test stress_single_writer -- --ignored

Run soak tests explicitly (examples):

    (cwd: /Users/zjx/Documents/chronicle-rs)
    CHRONICLE_SOAK_SECS=120 cargo test -p chronicle-core --test soak_writer_reader -- --ignored

Run benchmarks from the repository root:

    (cwd: /Users/zjx/Documents/chronicle-rs)
    cargo bench -p chronicle-core

## Validation and Acceptance

Acceptance is met when:

1) The fast stress test runs during `cargo test -p chronicle-core` and completes quickly while validating message count, ordering, and payload correctness across multiple segment rolls.
2) The ignored stress test can be run with `CHRONICLE_STRESS_MESSAGES` and completes without data corruption or panics.
3) The ignored soak test can be run for a chosen duration and completes with matching write/read counts and no CRC or header validation failures.
4) `cargo bench -p chronicle-core` runs the append and read benchmarks, producing Criterion output for each payload size.

Expected output snippets (examples):

    running 1 test
    test stress_single_writer_fast ... ok

    running 1 test
    test soak_writer_reader ... ok

    bench: append/64b ...
    bench: read/64b ...

## Idempotence and Recovery

All stress, soak, and benchmark runs use temporary directories and are safe to rerun. If a long-running test fails, re-run it with a smaller `CHRONICLE_STRESS_MESSAGES` or `CHRONICLE_SOAK_SECS` value to reproduce quickly, then increase the value after fixes. Because tests use isolated temp directories, no manual cleanup is required.

## Artifacts and Notes

Capture any unexpected stalls, CRC failures, or retention edge cases with short logs and note them in `Surprises & Discoveries`. If benchmarks regress, record the before/after Criterion summaries in this section.

## Interfaces and Dependencies

Add or confirm the following configuration and dependencies:

- Environment variables: `CHRONICLE_STRESS_MESSAGES` (message count for ignored stress), `CHRONICLE_SOAK_SECS` (duration for soak).
- Dev dependency in `crates/1-primitives/chronicle-core/Cargo.toml`: `criterion = "0.5"` (or the latest compatible 0.x version).
- Bench harness entries in `crates/1-primitives/chronicle-core/Cargo.toml` with `harness = false` for `append` and `read`.

The tests and benchmarks should use only the public API (`Queue`, `QueueWriter`, `QueueReader`, and constants like `SEGMENT_SIZE`) and should not require new public interfaces in the core library.

Change note: 2026-01-16 created the Phase 8 ExecPlan for hardening and benchmarks.
Change note: 2026-01-16 updated Progress and Outcomes after implementing stress/soak tests and Criterion benchmarks.
