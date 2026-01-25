# Implement Raw Archiver Loop for Sealed Segments

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

Follow `PLANS.md` from the repository root. This document must be maintained in accordance with `PLANS.md`.

## Purpose / Big Picture

After this change, operators can run a dedicated raw archiver that watches sealed IPC segments and copies them into a single-root raw archive layout, optionally compressing and writing metadata. This enables durable, replayable raw storage without adding latency to the feed handler. The behavior is observable by running the new archiver binary in `--once` mode against a test queue and confirming that the expected `raw/v1/.../raw/000000123.q` files appear with `meta.json`.

## Progress

- [x] (2026-01-24 00:00Z) Drafted ExecPlan with scope, interfaces, and validation strategy.
- [x] Implement shared metadata utilities and refactor tiering code to use them.
- [x] Implement raw archiver core loop and checkpointing.
- [x] Add CLI binary and wire configuration.
- [x] Add integration test and document expected outputs.

## Surprises & Discoveries

- Observation: Existing tiering code already scans segment timestamps and writes `meta.json`.
  Evidence: `src/storage/tier.rs` contains `scan_segment_timestamps` and `write_meta_if_missing`.

## Decision Log

- Decision: Use a single archive root with `raw/v1/<venue>/<date>/raw/`.
  Rationale: Matches the current layout helper and keeps raw vs clean clearly separated under one root.
  Date/Author: 2026-01-24 / Codex

- Decision: Partition raw archive by the earliest timestamp in the segment (UTC date).
  Rationale: Deterministic partitioning that reflects event time while tolerating cross-midnight spillover, with exact range recorded in `meta.json`.
  Date/Author: 2026-01-24 / Codex

- Decision: Archiver behaves like a normal reader and advances only after a segment is archived.
  Rationale: Ensures retention does not delete raw segments before archive is safely written.
  Date/Author: 2026-01-24 / Codex

- Decision: If a sealed segment has no timestamps, partition it by file mtime (UTC) instead of "now".
  Rationale: Keeps archive partitioning deterministic across reruns.
  Date/Author: 2026-01-24 / Codex

## Outcomes & Retrospective

No outcomes yet. This section will be updated when milestones complete and when the feature is verified end-to-end.

## Context and Orientation

The raw queue lives under `streams/raw/<venue>/queue/` and is composed of 9-digit `.q` segment files with a sealed flag in the segment header. Segment sealing is implemented in `src/core/segment.rs`, and reader behavior plus retention metadata live in `src/core/reader.rs`. The layout helper for raw archive paths is `RawArchiveLayout` in `src/layout/mod.rs`, which currently points to `archive_root/raw/v1/<venue>/<date>/raw/`.

The storage tiering logic in `src/storage/tier.rs` already handles compression and `meta.json` writing for archived segments. This plan reuses those utilities for consistency and avoids duplication.

A “sealed segment” means the segment header has the `SEG_FLAG_SEALED` bit set. Sealed segments are complete and no longer written to, but they may still be read. Deletion is governed by retention, not sealing.

## Plan of Work

First, extract metadata and timestamp scanning helpers from `src/storage/tier.rs` into a new module `src/storage/meta.rs` so they can be shared by tiering and the new archiver. This module should define a `MetaFile` struct, a `scan_segment_timestamps` function, and a `write_meta` helper that writes `meta.json` atomically. Refactor `TierManager` to call the shared helpers, preserving existing behavior.

Next, implement a raw archiver module at `src/storage/raw_archiver.rs`. It should expose a `RawArchiverConfig` and a `RawArchiver` with `run_once()` and `run_loop()` methods. The archiver should:

- Scan the raw queue directory for `.q` segments in order.
- For each candidate segment, verify the sealed flag by reading the segment header. Skip unsealed files and `.tmp` artifacts.
- Determine the archive partition date from the earliest timestamp in the segment (UTC). If no timestamps are present, fall back to the system date, but still record `event_time_range: null` in `meta.json`.
- Copy the `.q` to `archive_root/raw/v1/<venue>/<date>/raw/000000123.q.tmp`, `fsync`, then rename to `.q`, then `fsync` the parent directory.
- If compression is enabled, produce `.q.zst` and `.q.zst.idx` using the existing `compress_q_to_zst` function, via temp files and atomic renames.
- Write or update `meta.json` in the destination directory, including `venue`, `ingest_time_ns`, `event_time_range`, and `completeness: "sealed"`.
- Only after a segment is successfully archived should the archiver advance its reader commit position.

To integrate with retention, the archiver must register as a normal reader on the raw queue. The simplest safe path is to use `QueueReader::next()` to advance through messages and detect when the reader has crossed into a new segment; once the segment id changes, archive the previous segment and call `commit()` immediately after successful archive. This ensures retention only deletes segments once the archiver has safely persisted them.

Finally, add a new binary `src/bin/chronicle_raw_archiver.rs` with `clap` flags:

- `--source` (queue path)
- `--archive-root`
- `--venue`
- `--compress`
- `--retain-q` (if false, delete `.q` after compress)
- `--once` (single pass)
- `--poll-ms` (sleep between scans in loop mode)

Add an integration test in `tests/raw_archiver.rs` that creates a temporary queue, forces a segment roll, runs `RawArchiver::run_once()`, and asserts that the raw archive file and `meta.json` are created under the expected path.

If date formatting requires a new dependency, add `time = "0.3"` to `Cargo.toml` and use `time::OffsetDateTime::from_unix_timestamp_nanos` to format `YYYY-MM-DD` in UTC. If a dependency is added, document this in the `Decision Log`.

## Concrete Steps

Work in the repository root `/Users/zjx/Documents/chronicle-rs`.

1) Create `src/storage/meta.rs` and move the metadata helpers from `src/storage/tier.rs`. Update `src/storage/mod.rs` and `src/storage/tier.rs` to use the new module. Ensure behavior is unchanged.

2) Implement `src/storage/raw_archiver.rs` with `RawArchiverConfig` and `RawArchiver` that can `run_once()` and `run_loop()`.

3) Add the CLI binary at `src/bin/chronicle_raw_archiver.rs` and wire it to `RawArchiver`.

4) Add `tests/raw_archiver.rs` that writes a small queue, rolls a segment, runs the archiver once, and asserts the archive output.

5) Update `docs/PATH-CONTRACT.md` to document the single-root raw archive layout: `archive_root/raw/v1/<venue>/<date>/raw/`.

Commands to run:

    cd /Users/zjx/Documents/chronicle-rs
    cargo test
    cargo run --bin chronicle_raw_archiver -- --source /tmp/bus/streams/raw/binance/queue --archive-root /tmp/archive --venue binance --once

Expected output excerpt:

    archived segment=000000000.q -> /tmp/archive/raw/v1/binance/2026-01-24/raw/000000000.q
    meta.json written for /tmp/archive/raw/v1/binance/2026-01-24/raw/

## Validation and Acceptance

The change is accepted when:

- `cargo test` passes, including the new `tests/raw_archiver.rs`.
- Running the archiver in `--once` mode against a queue with a sealed segment produces a corresponding file under `archive_root/raw/v1/<venue>/<date>/raw/`.
- The `meta.json` file exists in the destination directory and includes `event_time_range` when timestamps are present.

## Idempotence and Recovery

The archiver must be safe to rerun. It should skip segments whose destination `.q` (or `.q.zst`) already exists, and it should treat `.tmp` files as recoverable artifacts by overwriting or cleaning them before writing new output. If a crash occurs mid-copy, re-running `run_once()` should complete the rename and re-write `meta.json` without corrupting data.

## Artifacts and Notes

Example expected archive layout after one sealed segment:

    /tmp/archive/raw/v1/binance/2026-01-24/raw/
      000000000.q
      meta.json

Example log line:

    archived segment=000000000.q -> /tmp/archive/raw/v1/binance/2026-01-24/raw/000000000.q

## Interfaces and Dependencies

Add a new module `crate::storage::meta` with:

    pub struct MetaFile { venue: Option<String>, symbol_code: Option<String>, ingest_time_ns: u64, event_time_range: Option<[u64; 2]>, completeness: String }
    pub fn scan_segment_timestamps(path: &Path) -> Result<Option<[u64; 2]>>
    pub fn write_meta(path: &Path, meta: &MetaFile) -> Result<()>

Add a new module `crate::storage::raw_archiver` with:

    pub struct RawArchiverConfig { ... }
    pub struct RawArchiver { ... }
    impl RawArchiver { pub fn run_once(&mut self) -> Result<()>; pub fn run_loop(&mut self) -> Result<()>; }

Use existing `crate::core::reader::QueueReader`, `crate::core::segment::read_segment_header`, and `crate::storage::compress_q_to_zst` for correctness and consistency.

Plan change log: Initial plan drafted to implement a sealed-segment raw archiver loop with shared metadata utilities and CLI integration.
