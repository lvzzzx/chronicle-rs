# Hot IPC + Archive Storage Refactor (Two-Tier Chronicle)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan is authored to comply with `PLANS.md` at the repository root (`PLANS.md`). Maintain this document according to those rules.

## Purpose / Big Picture

After this refactor, the system supports low-latency live IPC and long-term durable storage without them contending for retention or durability tradeoffs. Operators can run a hot queue for live processes and an archive queue for replay, ETL, and backtests, with a single-threaded bridge that preserves strict order. Historical imports can be written directly to the archive queue. The result is faster cold starts (snapshots plus indexes) and a stable log that remains canonical for research.

## Progress

- [x] (2026-01-23 00:00Z) Drafted ExecPlan for two-tier hot/archive refactor.
- [ ] Implement archive queue layout and config plumbing.
- [ ] Implement ArchiveTap process (hot to archive).
- [ ] Add snapshot production pipeline (L2 now, L3 optional).
- [ ] Add index flush policy for active segment in archive.
- [ ] Add historical import CLI for vendor data into archive.
- [ ] Add tests and update documentation.

## Surprises & Discoveries

Observation: seek index files exist but only flush on segment roll, so active segments can still require linear scan on cold start. Evidence: `crates/chronicle-core/src/writer.rs` only flushes `SeekIndexBuilder` in `roll_segment`.

Observation: snapshot support is implemented but only L2 apply is wired and no production snapshot writer runs in a live path. Evidence: `crates/chronicle-replay/src/snapshot.rs` provides a writer but there is no runtime integration outside tests/benchmarks.

## Decision Log

- Decision: split live IPC and archive storage into two queues (hot and archive) to avoid retention and durability conflicts. Rationale: IPC wants minimal latency and short retention while archive wants long retention, heavier indexing, and snapshots. Date/Author: 2026-01-23 / Codex
- Decision: implement a single-threaded ArchiveTap that re-appends events from hot to archive. Rationale: simplifies correctness while preserving order and avoids segment-copy complexity. Date/Author: 2026-01-23 / Codex
- Decision: historical vendor imports write directly to archive, bypassing hot IPC. Rationale: no need to affect live latency; archive is canonical for replay. Date/Author: 2026-01-23 / Codex

## Outcomes & Retrospective

Not executed yet. This section will be updated after implementation milestones complete.

## Context and Orientation

Chronicle currently provides a persisted mmap queue with per-reader offsets, segment rolling, and retention. Core data plane lives in `crates/chronicle-core` (writer/reader, seek index, retention). Replay and snapshots live in `crates/chronicle-replay`, but only L2 replay is implemented. The bus layer in `crates/chronicle-bus` defines directory layouts and discovery helpers for live processes. The protocol for book events is defined in `crates/chronicle-protocol`.

Relevant files include `crates/chronicle-core/src/writer.rs`, `crates/chronicle-core/src/reader.rs`, `crates/chronicle-core/src/seek_index.rs`, `crates/chronicle-core/src/retention.rs`, `crates/chronicle-replay/src/lib.rs`, `crates/chronicle-replay/src/snapshot.rs`, `crates/chronicle-bus/src/layout.rs`, and `crates/chronicle-protocol/src/lib.rs`.

## Plan of Work

The refactor is implemented in five milestones. Each milestone is independently verifiable and keeps the system working at every step.

Milestone 1 introduces a two-tier layout and a new ArchiveTap process that copies ordered records from a live queue to an archive queue. It preserves ordering and isolates hot IPC retention from archive retention.

Milestone 2 adds snapshot production for the archive queue using existing snapshot structures. L2 snapshots are required; L3 snapshots are optional but can be added with a new payload definition. Snapshots are written periodically and used for warm starts.

Milestone 3 adds an index flush policy for the active archive segment so cold-start seeks do not require scanning large tails.

Milestone 4 adds a historical import CLI that reads vendor data and writes directly to the archive queue using the chronicle protocol. This is the primary path for backfilling or offline vendor datasets.

Milestone 5 adds tests and documentation updates across `docs/` and `README.md` to reflect the two-tier architecture and new tools.

## Concrete Steps

Milestone 1: archive layout and ArchiveTap

Update `crates/chronicle-bus/src/layout.rs` to support an archive root. Add a new layout path like `<bus-root>/archive/<stream>/queue/`. Create a new crate `crates/chronicle-archive` with a binary `archive-tap`. Implement `archive-tap` as a single-threaded loop that opens a hot queue reader and an archive queue writer, writes type_id and payload into the archive, and commits the reader offset after successful writes. Add CLI flags `--live`, `--archive`, and `--start` to control queue paths and reader start mode.

Expected command (run from repo root):
    cargo run -p chronicle-archive -- --live ./demo_bus/live/market_data/queue/demo_feed --archive ./demo_bus/archive/market_data/queue/demo_feed

Expected behavior: the archive queue grows even if no replay or ETL processes are running, and hot queue retention is unaffected by archive readers.

Milestone 2: snapshot production (archive)

Add a Snapshotter module to `crates/chronicle-replay` or a new `crates/chronicle-snapshot` crate. The Snapshotter reads the archive queue, rebuilds the L2 book, and writes snapshots using `SnapshotWriter`. Trigger snapshots via `SnapshotPlanner` based on minimum interval, records, or bytes. Add a CLI binary that takes `--archive`, `--venue`, `--market`, and `--interval`. L2 snapshots are required and are implemented using `L2Snapshot` from `crates/chronicle-protocol/src/lib.rs`.

Expected command:
    cargo run -p chronicle-replay --example snapshotter -- --archive ./demo_bus/archive/market_data/queue/demo_feed --venue 1 --market 1

Expected behavior: snapshots appear under `<archive-root>/snapshots/<venue>/<market>/snapshot_<seq>.bin`, and `ReplayEngine::warm_start_*` succeeds against the archive queue.

Milestone 3: index flush policy for active segment

Extend `WriterConfig` in `crates/chronicle-core/src/writer.rs` with a new flush policy (interval or record count). Update the writer loop to flush `SeekIndexBuilder` periodically, not only on segment roll. Use conservative defaults for hot IPC and aggressive defaults for archive.

Expected behavior: `seek_seq` and `seek_timestamp` converge faster without scanning large active tails.

Milestone 4: historical import path

Add a `chronicle-import` binary (new crate or inside `crates/chronicle-archive`). Provide adapters that map vendor data into `BookEventHeader` plus payload, then write directly to the archive queue using the durable writer profile. Enforce gap checks and sequence continuity during import. This path bypasses hot IPC.

Expected behavior: a historical file can be ingested into archive without touching the live queue, and replay from earliest reconstructs the book deterministically.

Milestone 5: tests and docs

Add integration tests for ArchiveTap ordering, snapshot warm start, and archive seek. Update `docs/DESIGN.md`, `docs/DESIGN-research.md`, `docs/DESIGN-etl.md`, and `README.md` to describe the hot/archive split and the new tools.

## Validation and Acceptance

Run these commands from repo root.

1) Live to archive bridge works:
    cargo run -p chronicle-bus --example feed -- --bus-root ./demo_bus
    cargo run -p chronicle-archive -- --live ./demo_bus/live/market_data/queue/demo_feed --archive ./demo_bus/archive/market_data/queue/demo_feed
    cargo run -p chronicle-cli -- tail ./demo_bus/archive/market_data/queue/demo_feed --limit 3

Accept if `chronicle-cli -- tail` shows new records in the archive queue.

2) Snapshot warm start:
    cargo run -p chronicle-replay --example snapshotter -- --archive ./demo_bus/archive/market_data/queue/demo_feed --venue 1 --market 1
    cargo run -p chronicle-replay --example warm_start -- --archive ./demo_bus/archive/market_data/queue/demo_feed --venue 1 --market 1

Accept if warm start reports a snapshot header and immediately starts replay from the snapshot boundary.

3) Index seek behavior:
    cargo test -p chronicle-core seek

Accept if tests pass and seek operations use index entries to avoid full scans.

## Idempotence and Recovery

All new processes are additive and safe to rerun. ArchiveTap uses reader meta files, so restart resumes from last committed offset. Snapshot files are written to a temp path and atomically renamed, so partial files are not visible. If the archive queue is corrupt, delete only archive paths and re-run ArchiveTap or import to rebuild, leaving hot IPC untouched.

## Artifacts and Notes

Example ArchiveTap log snippet:
    archive-tap: opened live queue ./demo_bus/live/market_data/queue/demo_feed
    archive-tap: opened archive queue ./demo_bus/archive/market_data/queue/demo_feed
    archive-tap: appended type=0x1000 bytes=128

## Interfaces and Dependencies

Define a new `crates/chronicle-archive` crate that depends on `chronicle-core` and uses `QueueReader` and `QueueWriter`. The crate exports `ArchiveTap` in `crates/chronicle-archive/src/lib.rs` with `pub fn run(&mut self) -> Result<()>` and a CLI binary in `crates/chronicle-archive/src/main.rs` that accepts `--live`, `--archive`, and `--start` flags.

Extend `WriterConfig` in `crates/chronicle-core/src/writer.rs` with an index flush policy and implement a `flush_index` helper to write `.idx` files without rolling segments. Use this in the archive writer profile and keep the hot IPC profile minimal.

Snapshot production uses `SnapshotWriter`, `SnapshotPlanner`, and `L2Snapshot` in `crates/chronicle-replay/src/snapshot.rs` and `crates/chronicle-protocol/src/lib.rs`. No external services are required; all operations are single-host and filesystem-based.
