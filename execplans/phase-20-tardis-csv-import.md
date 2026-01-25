# Archive-only Tardis CSV Book Importer (src/import/tardis_csv.rs)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

After this change, a user can ingest a Tardis `incremental_book_L2` CSV (gzipped or plain) directly into the Chronicle archive without using IPC. The importer writes sealed `.q` segment files under the archive stream directory so downstream consumers (replay, refinery, extractors) can read it. The behavior is observable by running a new CLI command on the provided CSV and seeing segment files and an import ledger under the archive root.

## Progress

- [x] (2026-01-24 18:05Z) Read protocol, segment, and layout code to confirm record format and archive stream paths.
- [x] (2026-01-24 18:30Z) Implement `src/import/tardis_csv.rs` with streaming parse, grouping, book-event encoding, and ledger logic.
- [x] (2026-01-24 18:45Z) Add archive writer, CLI entrypoint, tests, and documentation; validate end-to-end.

## Surprises & Discoveries

- Observation: `storage::meta` and `storage::archive_writer` needed re-exports for cross-module use and tests.
  Evidence: `cargo test -q --test csv_import` initially failed with E0603 until re-exports were added.

## Decision Log

- Decision: Implement the importer in `src/import/tardis_csv.rs` and keep it self-contained.
  Rationale: User preference and keeps CSV import logic isolated from live feed code.
  Date/Author: 2026-01-24 / Codex

- Decision: Write archive segments directly (segment header + message header + payload) instead of using `QueueWriter`.
  Rationale: Archive streams do not use `control.meta` or queue reader metadata; direct segment writing avoids creating IPC-only files.
  Date/Author: 2026-01-24 / Codex

- Decision: Group CSV rows by `local_timestamp` to form one book event per group.
  Rationale: Tardis schema defines grouping by `local_timestamp`; this maps naturally to message boundaries.
  Date/Author: 2026-01-24 / Codex

- Decision: If any row in a group has `is_snapshot=true`, emit a Snapshot event and reset state.
  Rationale: Mixed flags are likely malformed; favor snapshot semantics to preserve correctness.
  Date/Author: 2026-01-24 / Codex

- Decision: Derive `price_scale` and `size_scale` per group from maximum decimal precision in that group (clamped to 18).
  Rationale: Matches existing fixed-point parsing used in live feed code; avoids losing precision.
  Date/Author: 2026-01-24 / Codex

- Decision: Use synthetic monotonic update ids for `L2Diff` when CSV does not provide them.
  Rationale: Keeps diffs ordered and allows replay logic that expects monotonic update ids.
  Date/Author: 2026-01-24 / Codex

- Decision: Add an idempotency ledger keyed by input file hash.
  Rationale: Batch imports should be safe to rerun without duplicating segments.
  Date/Author: 2026-01-24 / Codex

## Outcomes & Retrospective

Implemented an archive-only Tardis CSV importer with a dedicated archive writer, a CLI entrypoint, and coverage via `tests/csv_import.rs`. The importer streams CSV/GZ input, groups rows by `local_timestamp`, emits `BookEvent` payloads, writes sealed segments, and records a ledger for idempotency. Remaining work is limited to any future enhancements (e.g., richer meta fields or additional CSV schemas).

## Context and Orientation

Chronicle stores IPC queues under `src/core` and archive layout helpers in `src/layout/mod.rs`. Archive streams live under `<archive_root>/v1/<venue>/<symbol>/<date>/<stream>/` and contain `.q` segment files. Each segment file begins with a 64-byte segment header (`src/core/segment.rs`) and contains records; each record has a 64-byte message header (`src/core/header.rs`) followed by payload aligned to 64 bytes. Book events are defined in `src/protocol/mod.rs` using `BookEventHeader`, `L2Snapshot`, `L2Diff`, and `PriceLevelUpdate`. The importer must write these book event payloads with `TypeId::BookEvent`.

The CSV file provided is a Tardis `incremental_book_L2` dataset. The schema includes: `exchange`, `symbol`, `timestamp`, `local_timestamp`, `is_snapshot`, `side`, `price`, `amount`. Rows are grouped by `local_timestamp`, and `is_snapshot` indicates snapshot rows. `amount=0` removes a price level. The importer converts each group into a BookEvent payload.

## Plan of Work

Implement a CSV importer module at `src/import/tardis_csv.rs` that streams rows, groups by `local_timestamp`, encodes `BookEvent` payloads, and writes them to archive segments via a minimal archive writer. Add an import ledger to make runs idempotent. Expose a CLI to run the importer against an input file and write to the archive. Add tests to verify segment creation and sealing.

## Concrete Steps

1) Create the importer module at `src/import/tardis_csv.rs`.

   Define a row struct and parsing logic. Use the `csv` crate to parse the stream. Support `.csv` and `.csv.gz` (use `flate2::read::GzDecoder` when the input ends in `.gz`).

   Group rows by `local_timestamp_us`: maintain `current_group_ts` and finalize the current group when the timestamp changes.

   Implement decimal helpers in this module (copied in simplified form from `src/feed_binance/binance.rs`): `decimal_scale`, `parse_fixed`, `max_scales`.

2) Add an archive writer helper in `src/storage/archive_writer.rs`.

   Define `ArchiveWriter` that writes `.q` segments directly. Use `core::segment::prepare_segment_temp` to create a temp segment, write records, then `seal_segment` and `publish_segment` on roll. Maintain `segment_id`, `write_offset`, and `seq`. Provide `append_record(type_id, timestamp_ns, payload)` that writes a `MessageHeader` plus payload, aligns to 64 bytes, and rolls if needed. Provide `finish()` to seal and publish the current segment if it contains any records.

3) Encode book events in `src/import/tardis_csv.rs`.

   Add helpers: `encode_snapshot(...) -> Vec<u8>`, `encode_diff(...) -> Vec<u8>`, and `book_event_len<T>(bid_len, ask_len) -> Result<u16>` (copy from feed_binance). Set `event_type` to Snapshot or Diff, `book_mode` to L2, `flags` to `book_flags::ABSOLUTE`, `ingest_ts_ns` from `local_timestamp_us * 1000`, and `exchange_ts_ns` from `timestamp_us * 1000`. Increment `seq` per group. For diffs, set `update_id_first = seq`, `update_id_last = seq`, `update_id_prev = seq - 1`.

4) Add idempotency ledger.

   Hash the input file (e.g., with `blake3`) and write a ledger JSON under `imports/v1/tardis/<venue>/<symbol>/<date>/<hash>.json` with `source_path`, `file_hash`, `rows`, `groups`, `segments`, `min_ts_ns`, `max_ts_ns`, and `imported_at_ns`. If the ledger exists and `--force` is false, skip import.

5) Add the CLI.

   Create `src/bin/chronicle_csv_import.rs` with flags: `--input`, `--archive-root`, `--venue`, `--symbol`, `--date`, `--stream` (default `book`), `--venue-id`, `--market-id`, `--segment-size`, `--force`, `--compress`. If `market-id` is not provided, compute `market_id(symbol)` using `src/feed_binance/market.rs`.

   Add a bin entry to `Cargo.toml`.

6) Add tests.

   Add `tests/csv_import.rs`: build a small CSV string with snapshot rows then diff rows, gzip it to a temp file, run `import_tardis_incremental_book(...)`, assert the stream directory exists and contains a sealed `.q` segment (check segment header flags), and optionally read the first message header to confirm `TypeId::BookEvent`.

7) Documentation.

   Update `docs/PATH-CONTRACT.md` with an “Archive-only imports” section and ledger location. Add a short CLI usage example in `README.md`.

## Validation and Acceptance

Run the importer on the provided file and observe archive output:

  Working directory: `/Users/zjx/Documents/chronicle-rs`

  Command:

    cargo run --bin chronicle_csv_import -- \
      --input /Users/zjx/data/lake/bronze/tardis/exchange=binance-futures/type=incremental_book_L2/date=2024-05-01/symbol=BTCUSDT/binance-futures_incremental_book_L2_2024-05-01_BTCUSDT.csv.gz \
      --archive-root /Users/zjx/data/archive \
      --venue binance-futures \
      --symbol BTCUSDT \
      --date 2024-05-01 \
      --venue-id 1 \
      --stream book

Expected observations:

- Stream directory exists at `/Users/zjx/data/archive/v1/binance-futures/BTCUSDT/2024-05-01/book/`.
- At least one `.q` file exists and is sealed.
- A ledger JSON exists under `/Users/zjx/data/archive/imports/v1/tardis/binance-futures/BTCUSDT/2024-05-01/`.

Run tests:

  Command:

    cargo test -q --test csv_import

Expected results:

- Test passes and confirms that a sealed segment file exists for the imported CSV.

## Idempotence and Recovery

The importer writes `.q.tmp` files and publishes atomically; rerunning after crash is safe because temp files are ignored. The ledger prevents duplicates; use `--force` to re-import intentionally. If the stream directory is partially written, remove it or rerun with `--force`.

## Artifacts and Notes

Expected directory layout after import:

  /Users/zjx/data/archive/
    v1/binance-futures/BTCUSDT/2024-05-01/book/
      000000000.q
      000000001.q
      meta.json
    imports/v1/tardis/binance-futures/BTCUSDT/2024-05-01/<hash>.json

## Interfaces and Dependencies

New module: `src/import/tardis_csv.rs` with:

  pub struct ImportStats {
      pub rows: u64,
      pub groups: u64,
      pub segments: u64,
      pub min_ts_ns: u64,
      pub max_ts_ns: u64,
  }

  pub fn import_tardis_incremental_book(
      input: &Path,
      archive_root: &Path,
      venue: &str,
      symbol: &str,
      date: &str,
      stream: &str,
      venue_id: u16,
      market_id: u32,
      segment_size: usize,
      force: bool,
      compress: bool,
  ) -> Result<ImportStats>

New module: `src/storage/archive_writer.rs` with:

  pub struct ArchiveWriter { ... }

  impl ArchiveWriter {
      pub fn new(...) -> Result<Self>;
      pub fn append_record(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()>;
      pub fn finish(&mut self) -> Result<()>;
  }

Dependencies to add in `Cargo.toml`:

- `csv`
- `flate2`
- `blake3` (or `sha2` if preferred)

## Change Note

Updated progress, recorded a discovery about module re-exports, and summarized outcomes after implementation.
