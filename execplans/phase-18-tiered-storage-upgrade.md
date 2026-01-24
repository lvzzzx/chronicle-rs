# Tiered Storage Upgrade (Seekable Zstd + SCD2 Symbols + Remote Cold)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

After this upgrade, Chronicle stores and reads historical data from a tiered, partitioned archive that supports fast random access even when cold data is compressed. Operators can ingest raw live data, refine it into structured streams, and archive those streams into a stable on-disk layout keyed by venue, symbol, and date. Researchers can open a logical request (venue, symbol, date, stream) and get the correct tier (hot, warm, cold, or remote cold) without knowing where the file lives. Symbol identities are stable over time by resolving against a Slowly Changing Dimension (SCD) symbol catalog as-of the event time. There is no backward compatibility with the older archive queue layout; existing data must be re-imported if needed.

## Progress

- [x] (2026-01-24 00:20Z) Author ExecPlan for tiered storage upgrade and align with existing ETL plans.
- [x] (2026-01-24 00:35Z) Implement Delta-backed symbol catalog with cached index and add fixture-based tests.
- [x] (2026-01-24 00:40Z) Wire `chronicle-etl refine` to resolve symbols by `(venue_id, market_id)` and inject snapshots with correct IDs.
- [x] (2026-01-24 01:05Z) Implement seekable Zstd writer and index format for cold storage.
- [x] (2026-01-24 01:35Z) Refactor `chronicle-storage` into a TierManager with warm->cold and cold->remote transitions.
- [x] (2026-01-24 02:35Z) Add tier-aware reader that resolves hot/warm/cold/remote paths and reads `.q` or `.q.zst`.
- [ ] (2026-01-24 00:00Z) Add integration tests and update documentation to reflect the new storage model.

## Surprises & Discoveries

No surprises yet. Update this section if any file-format, compression, or platform behaviors differ from expectations.

## Decision Log

- Decision: No backward compatibility with the old archive queue layout.
  Rationale: The new layout and cold compression semantics are simpler and more reliable if we treat this as a clean break.
  Date/Author: 2026-01-24 / Codex
- Decision: Use seekable Zstd with an explicit `.q.zst.idx` file and fixed-size blocks (default 1 MiB uncompressed).
  Rationale: Provides predictable random access without requiring a central manifest database.
  Date/Author: 2026-01-24 / Codex
- Decision: Symbol normalization uses an SCD2 catalog resolved as-of event time.
  Rationale: Symbols can rename, delist, or change tick sizes; event-time resolution preserves historical truth.
  Date/Author: 2026-01-24 / Codex
- Decision: Resolve symbols by `(exchange_id, market_id, event_time_us)`, where `market_id` is the Chronicle hash of `exchange_symbol`. Treat `valid_until_ts` as exclusive.
  Rationale: The live feed encodes `market_id` as a hash of `exchange_symbol` in `BookEventHeader`; resolution must use that hash to map back to the SCD2 row.
  Date/Author: 2026-01-24 / Codex
- Decision: Remote cold tier uses a local stub marker (`*.remote.json`) rather than a centralized manifest.
  Rationale: Object storage lacks atomic rename; the stub keeps tier resolution deterministic without a DB.
  Date/Author: 2026-01-24 / Codex
- Decision: Tier-aware reader uses per-tier roots with `.q` preference before `.q.zst`, and remote stubs download to `/tmp/chronicle-cache` keyed by a stable FNV-1a hash of the remote URI.
  Rationale: Keeps tier resolution deterministic without a manifest and provides a stable, repeatable cache location for remote cold reads.
  Date/Author: 2026-01-24 / Codex

## Outcomes & Retrospective

Pending. This section will be updated after milestone completion.

## Context and Orientation

The current storage tooling is in `crates/2-infra/chronicle-storage` and only provides an `ArchiveTap` that mirrors a live queue into an archive queue. `chronicle-core` (`crates/1-primitives/chronicle-core`) implements the memory-mapped queue, segment headers, and seek indexes for `.q` segments. The ETL design is described in `docs/DESIGN-etl.md` and the storage design is described in `docs/DESIGN-storage.md`. There is an existing ExecPlan for ETL in `execplans/phase-17-etl-refinery.md` that introduces `chronicle-etl refine`. This plan assumes the ETL refinement path exists or will be built in parallel, and it extends it with SCD2 symbol resolution and a new archive layout.

Terminology used in this plan:
The “TierManager” is a long-running background process that scans warm storage for sealed segments and moves them to cold storage. “Cold storage” means compressed `.q.zst` plus a seek index `.q.zst.idx` that allows random access by uncompressed offset. “Remote cold” means data moved to object storage with a local stub file that records the remote location.

## Plan of Work

This upgrade is delivered in five milestones. Each milestone has observable outcomes and keeps the repository in a working state.

Milestone 1 introduces a symbol catalog with SCD2 semantics and wires it into `chronicle-etl refine`. This plan assumes a Delta Lake table already exists at `~/data/lake/silver/dim_symbol/` and uses it directly as the source of truth. The catalog is loaded once at startup into an in-memory index for fast per-message lookups, with an optional periodic refresh if needed. This ensures that all downstream storage paths and metadata use stable symbol identities resolved as-of event time.

Milestone 2 introduces the seekable Zstd file format and index writer for cold storage. It provides a library function that can transcode a sealed `.q` segment into `.q.zst` and `.q.zst.idx`, plus a reader that can map an uncompressed offset to a compressed frame and return bytes for parsing.

Milestone 3 refactors `chronicle-storage` into a TierManager. The TierManager watches warm partitions, detects sealed `.q` files, transcodes them into `.q.zst`, writes `meta.json`, and moves them into the cold tier. It also supports a remote cold transition via a stub file for object storage.

Milestone 4 adds a tier-aware reader that resolves the correct tier path (hot, warm, cold, remote cold) and reads `.q` or `.q.zst` transparently. This reader is intended for research workloads and is separate from live IPC.

Milestone 5 adds integration tests and documentation updates and removes references to the old archive queue layout.

## Concrete Steps

Milestone 1: SCD2 symbol catalog and ETL integration (Delta Lake source)

1. Create a new module `crates/4-app/chronicle-etl/src/catalog.rs` (or a new crate `crates/2-infra/chronicle-catalog` if reuse is needed) that loads the Delta Lake table at `~/data/lake/silver/dim_symbol/` (or a user-provided path) once at startup and builds an in-memory index for lookups.
2. Add `deltalake` as a dependency for reading Delta tables and `arrow` to parse schema and rows into native Rust structs.
3. Define a `SymbolCatalog` that indexes entries by `(exchange_id, market_id)` where `market_id` is derived by hashing `exchange_symbol` using the same FNV-1a scheme as the feed. Store versions with `valid_from_ts` and `valid_until_ts` (both microseconds since epoch, with `valid_until_ts` exclusive).
4. Provide a `resolve(exchange_id, market_id, event_time_ns) -> SymbolIdentity` function. Convert `event_time_ns` to microseconds before comparison. Return `symbol_id`, `symbol_code`, and `symbol_version_id`.
5. Update `crates/4-app/chronicle-etl/src/main.rs` to accept `--catalog-delta <path>` and require it for `refine`. Default to `~/data/lake/silver/dim_symbol/` if not provided. Add `--catalog-refresh-secs <u64>` (optional) to reload the catalog on a timer; default is 0 (no refresh).
6. Update the refine pipeline to resolve symbol identity per message and to emit `symbol_id` into the clean stream (either in the message header or in metadata that the storage layer will capture). Use a normalized `symbol_code` derived from `exchange_symbol` for directory paths: lowercase, replace `/` and `:` with `-`, replace spaces with `-`, collapse repeated `-`, keep only `[a-z0-9._-]`, and trim leading/trailing `-`. Store the original `exchange_symbol` in `meta.json`.
7. Add a unit test that loads a minimal Delta table fixture (small test table under `crates/4-app/chronicle-etl/tests/data/dim_symbol/`) and verifies `resolve()` selects the correct SCD2 version for an event time.

Milestone 2: Seekable Zstd and index format

1. Add a new module `crates/1-primitives/chronicle-core/src/zstd_seek.rs` with:
   - `pub struct ZstdSeekIndex { block_size: u32, entries: Vec<ZstdSeekEntry> }`
   - `pub fn write_seek_index(path: &Path, block_size: usize, entries: &[ZstdSeekEntry]) -> Result<()>`
   - `pub fn read_seek_index(path: &Path) -> Result<ZstdSeekIndex>`
2. Define the index file format in code to match `docs/DESIGN-storage.md`:
   - Header: magic `QZSTIDX1`, version, block_size, frame_count.
   - Entry: `u64 uncompressed_offset`, `u64 compressed_offset`, `u32 compressed_size`, `u32 uncompressed_size`.
3. Implement `compress_q_to_zst(input_q, output_zst, output_idx, block_size)` in `crates/2-infra/chronicle-storage/src/lib.rs` (or a new module) using `zstd` and the index writer.
4. Implement a `ZstdBlockReader` that reads a given uncompressed offset by loading the appropriate frame from `.q.zst` via `.q.zst.idx` and returning the block bytes.
5. Add unit tests for the index header and a round-trip test that writes sample data, compresses it, and verifies random reads at multiple offsets.

Milestone 3: TierManager and metadata

1. Refactor `crates/2-infra/chronicle-storage`:
   - Replace `ArchiveTap` with a `TierManager` library and binary `chronicle-storage tier`.
   - Keep `archive-import` as an optional tool for one-off imports.
2. Implement partition scanning for layout `v1/{venue}/{symbol}/{date}/...` and detect sealed `.q` files.
   - Use `crates/1-primitives/chronicle-core/src/segment.rs` helpers to read segment headers and `SEG_FLAG_SEALED`.
3. Implement warm->cold move:
   - Copy `.q` to a temp `.q.zst.tmp`, compress, write `.q.zst.idx.tmp`.
   - Fsync temp files, then rename to `.q.zst` and `.q.zst.idx`.
   - Write `meta.json` with symbol identity, event time range, and completeness flags.
4. Implement cold->remote move:
   - Upload to object storage (or a local “remote” root) and verify checksum or size.
   - Write `book.q.zst.remote.json` containing `remote_uri`, `etag`, and sizes.
   - Remove local `.q.zst` and `.q.zst.idx`.
5. Add CLI flags for tiering:
   - `chronicle-storage tier --root <path> --once` (single scan).
   - `chronicle-storage tier --root <path> --interval <seconds>` (daemon loop).

Milestone 4: Tier-aware reader

1. Add a new crate `crates/2-infra/chronicle-access` (or a module in `chronicle-storage`) that provides:
   - `StorageResolver` to map `(venue, symbol_code, date, stream)` to a file path by tier priority.
   - `StorageReader` that returns an iterator of `MessageView` and supports `seek_timestamp`.
2. Implement tier priority: `hot -> warm -> cold -> remote cold` and ensure readers stop at the first hit to avoid double reads.
3. Implement `.q` reads by reusing `chronicle-core` segment parsing. Implement `.q.zst` reads via `ZstdBlockReader`.
4. Add a remote cold reader that downloads to a temp file on first access (simple, deterministic behavior) and caches it in `/tmp/chronicle-cache`.

Milestone 5: Tests and documentation

1. Add integration tests in `crates/2-infra/chronicle-storage/tests/`:
   - `tier_manager_moves_sealed_segments`: verifies `.q` -> `.q.zst` + `.idx` + `meta.json`.
   - `tier_resolution_prefers_warm_over_cold`: verifies no double reads.
   - `remote_stub_downloads_once`: verifies remote cold stub behavior.
2. Update docs to remove archive-queue references and align with new design:
   - `docs/DESIGN-storage.md` (already updated, verify alignment).
   - `docs/DESIGN-etl.md` (remove manifest and add SCD2 symbol resolution).
   - `docs/DESIGN-workspace.md` and `README.md` (update component roles).
3. Add a migration note in `docs/README` or `docs/ROADMAP.md` describing re-import for legacy archives (no backward compatibility).

## Validation and Acceptance

Run the following from repo root (`/Users/zjx/Documents/chronicle-rs`):

1. Unit and integration tests:
   cargo test -p chronicle-core
   cargo test -p chronicle-storage
   cargo test -p chronicle-etl

2. Local tiering smoke test:
   - Create a warm partition with a sealed `.q` segment.
   - Run: `cargo run -p chronicle-storage -- tier --root ./tmp_storage --once`
   - Expect `.q.zst`, `.q.zst.idx`, and `meta.json` to appear under the partition.

3. Tier-aware reader smoke test:
   - Run: `cargo run -p chronicle-access -- example-read --root ./tmp_storage --venue binance-perp --symbol btc-usdt --date 2024-01-24 --stream book`
   - Expect output messages and correct timestamps; a second run should hit the cache or local file depending on tier.

Acceptance is met when all tests pass and the smoke tests demonstrate cold compression, tier resolution, and successful reads from cold or remote tiers.

## Idempotence and Recovery

All tiering operations are designed to be re-run safely. Compression and index creation write temp files and only rename on success. If a tier move fails, the temp files can be deleted and the operation retried. Remote cold moves leave local data intact until the remote stub is written, so retries are safe. There is no backward compatibility; existing data must be re-imported if needed.

## Artifacts and Notes

Example `meta.json` (indented to avoid nested code fences):

  {
    "symbol_id": 10123,
    "symbol_version_id": 8841,
    "symbol_code": "btc-usdt",
    "source": "chronicle-etl refine",
    "ingest_time_ns": 1711094400000000000,
    "event_time_range": [1711065600000000000, 1711152000000000000],
    "completeness": "complete",
    "late_policy": "reject"
  }

Example `book.q.zst.idx` header (binary fields in order):

  magic = "QZSTIDX1"
  version = 1
  block_size = 1048576
  frame_count = 128

## Interfaces and Dependencies

New or updated interfaces that must exist:

- `chronicle_etl::catalog::SymbolCatalog` with `resolve(venue, raw_symbol, event_time_ns) -> SymbolIdentity`.
- `chronicle_core::zstd_seek::ZstdSeekIndex` with `read_seek_index` and `write_seek_index`.
- `chronicle_storage::tier::TierManager` with `run_once()` and `run_loop(interval)`.
- `chronicle_access::StorageReader` with `open(root, venue, symbol, date, stream)` and `seek_timestamp`.

Dependencies:

- `zstd` crate for compression.
- `serde` and `serde_json` for `meta.json` and remote stub files.
- `deltalake` and `arrow` for reading the Delta Lake `dim_symbol` table.

If any of these dependencies are added, update the relevant `Cargo.toml` files and keep feature flags minimal.

## Plan Update Notes

2026-01-24: Updated Milestone 1 to resolve symbols via `(exchange_id, market_id)` using the feed hash, marked completed catalog implementation, and documented cached Delta loading to match the live message format.
2026-01-24: Completed Milestone 2 by adding the Zstd seek index format in `chronicle-core`, a block compressor/reader in `chronicle-storage`, and a round-trip test.
2026-01-24: Completed Milestone 3 by adding a TierManager with warm->cold compression, optional cold->remote moves, and a CLI binary for scheduled runs.
2026-01-24: Completed Milestone 4 by adding the `chronicle-access` crate with tier resolution, `.q`/`.q.zst` readers, and remote stub caching behavior.
