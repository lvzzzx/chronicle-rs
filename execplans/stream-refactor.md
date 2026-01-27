# Unify Stream API for Live and Archive Readers

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan follows `PLANS.md` from the repository root and must be maintained in accordance with it.

## Purpose / Big Picture

After this change, end users interact with a single `stream` API for both live IPC queues and archived segment playback. Replay, reconstruction, and ETL use the same `stream` abstractions, and stream fan-in (merge) works for both live and offline sources. Live single-stream reads remain zero-copy, while merge for archived or mixed sources uses owned payloads. Users can observe the change by compiling and running existing replay/ETL tools against either a live queue or a recorded archive without changing the API they use.

## Progress

- [x] (2026-01-26 22:10Z) Create stream module and public API types with live/archive adapters.
- [x] (2026-01-26 22:10Z) Refactor replay/reconstruct/etl to use the stream API.
- [x] (2026-01-26 22:10Z) Move and split merge: zero-copy live fan-in and owned merge for archive.
- [x] (2026-01-26 22:10Z) Rename feature `stream_table` to `stream`, update lib/bin gating, and update docs.
- [x] (2026-01-26 22:10Z) Validate by building tests or representative binaries.

## Surprises & Discoveries

- Borrowing through a generic message trait caused self-referential borrow errors in `ReplayMessage`.
  Evidence: compiler errors E0597/E0505/E0515 when returning `ReplayMessage` with borrowed payloads.

## Decision Log

- Decision: `StreamReader` returns `StreamMessageRef<'a>` (borrowed) for both live and archive readers.
  Rationale: `StorageReader` already buffers payloads internally, so archive reads can remain borrowed; this avoids self-referential borrow issues and preserves zero-copy semantics.
  Date/Author: 2026-01-26 / Codex

- Decision: Keep `OwnedStreamReader` + `OwnedFanInReader` for merge cases that need owned payloads.
  Rationale: Fan-in across independent sources can require holding payloads beyond a single reader buffer lifetime.
  Date/Author: 2026-01-26 / Codex

- Decision: Provide two merge implementations: live zero-copy fan-in and owned merge for archive.
  Rationale: The existing queue fan-in relies on payload offsets and avoids copies; archive merging cannot use that API and should own payloads for safe ordering.
  Date/Author: 2026-01-26 / Codex

- Decision: Keep internal `Queue` naming and expose end-user API as `stream`.
  Rationale: The bus queue is a low-level primitive with commit semantics; the stream API is a higher-level domain replay layer. Keeping both names preserves clarity without exposing queue to end users.
  Date/Author: 2026-01-26 / Codex

## Outcomes & Retrospective

- (to be completed at milestone completion)

## Context and Orientation

The repository has a low-level bus in `src/core/` and domain replay logic in `src/replay/`, `src/reconstruct/`, and `src/etl/`. Live reads use `crate::core::QueueReader` and archives use `crate::storage::access::StorageReader`. Merge (fan-in) exists at `src/core/merge.rs` but is tied to live queues. The current public API exposes `replay`, `reconstruct`, and `etl` modules directly and uses the feature name `stream_table`.

Key files:
- `src/core/reader.rs` and `src/core/merge.rs` for live queue reading.
- `src/storage/access/reader.rs` for archive segment reading.
- `src/replay/mod.rs` for L2 replay engine.
- `src/reconstruct/szse_l3.rs` for L3 reconstruction.
- `src/etl/` for feature extraction and snapshot injection.
- `Cargo.toml` and `src/lib.rs` for feature gating and public API.

The goal is to introduce a new `src/stream/` module that hides whether data comes from live queues or archives, and to move the stream-table logic under this new module. The `stream` feature replaces `stream_table`.

## Plan of Work

First, introduce a new `src/stream/` module that defines the public stream API. The core of this module is a `StreamReader` trait that exposes `next`, `wait`, `commit`, and `set_wait_strategy`, plus message types. `StreamReader` always returns `StreamMessageRef<'a>` (borrowed), leveraging the internal buffers of `QueueReader` and `StorageReader`. Provide `LiveStream` and `ArchiveStream` adapters that wrap `QueueReader` and `StorageReader` respectively. Provide `OwnedStreamReader` as a separate trait for cases that need owned payloads (fan-in).

Second, refactor replay, reconstruction, and ETL to depend on `stream` rather than directly on `QueueReader` or `StorageReader`. `ReplayEngine` becomes generic over `StreamReader`, and `ReplayMessage` carries a `StreamMessageRef<'a>` so replay remains zero-copy for both live and archive readers.

Third, move merge into the `stream` module and split it into two fan-in implementations: a zero-copy live fan-in that stays close to the existing `core::merge` logic, and an owned merge that works with any `StreamReader` (including archives). Expose both under `stream::merge` and document when to use each.

Fourth, rename the feature `stream_table` to `stream` in `Cargo.toml` and update all bin `required-features`, plus `src/lib.rs` exports. Remove or hide old module exports so users must go through `stream`.

Finally, update design docs to reflect the new `stream` naming and run build/tests for key binaries. Capture any surprises in this plan.

## Concrete Steps

1) Create `src/stream/mod.rs` with the public stream API types and submodules. Add `src/stream/live.rs`, `src/stream/archive.rs`, and `src/stream/merge.rs`.
2) Implement `StreamMessageRef`, `StreamMessageOwned`, and `StreamReader` trait returning borrowed messages; keep `OwnedStreamReader` for owned payloads.
3) Implement `LiveStream` and `ArchiveStream` adapters.
4) Refactor `src/replay/`, `src/reconstruct/`, and `src/etl/` to use the `stream` API.
5) Move `src/core/merge.rs` to `src/stream/merge.rs`, keep zero-copy fan-in for live, and add owned merge for archive.
6) Update `Cargo.toml` features and bin gating; update `src/lib.rs` exports.
7) Update `docs/DESIGN.md` and any other doc references to `stream_table`.
8) Run `cargo build` and optionally targeted tests/binaries.

## Validation and Acceptance

- Build: run `cargo build` from the repo root and ensure compilation succeeds.
- API behavior: verify that `ReplayEngine` can be constructed using both a live queue and an archive reader through the new `stream` API.
- Merge behavior: verify that live fan-in still uses zero-copy by construction (no owned payload allocation path), and that archive merge compiles and returns ordered messages.

## Idempotence and Recovery

Edits are source-only and can be re-run safely. If a build fails, revert the last change or adjust the adapter implementations to match the queue/storage reader interfaces. No data migration is required.

## Artifacts and Notes

- The stream API will live under `src/stream/` and should be the only public entry point for replay/reconstruct/etl.
- `core::merge` will be removed or made private; a note will be left in `stream::merge` about live vs archive fan-in.

## Interfaces and Dependencies

Define in `src/stream/mod.rs`:

    pub struct StreamMessageRef<'a> {
        pub seq: u64,
        pub timestamp_ns: u64,
        pub type_id: u16,
        pub payload: &'a [u8],
    }

    pub struct StreamMessageOwned {
        pub seq: u64,
        pub timestamp_ns: u64,
        pub type_id: u16,
        pub payload: Vec<u8>,
    }

    pub trait StreamReader {
        fn next<'a>(&'a mut self) -> anyhow::Result<Option<StreamMessageRef<'a>>>;
        fn wait(&mut self, timeout: Option<std::time::Duration>) -> anyhow::Result<()>;
        fn commit(&mut self) -> anyhow::Result<()>;
        fn set_wait_strategy(&mut self, strategy: crate::core::WaitStrategy);
    }

Provide adapters:
- `LiveStream` wrapping `QueueReader` and yielding `StreamMessageRef<'a>`.
- `ArchiveStream` wrapping `StorageReader` and yielding `StreamMessageOwned`.

Provide merge types in `src/stream/merge.rs`:
- `LiveFanInReader` reusing zero-copy logic for multiple `LiveStream` instances.
- `OwnedMerger` merging multiple `StreamReader` instances that yield owned messages.

Update `ReplayEngine` to be generic over `StreamReader` and provide type aliases for live and archive use.
