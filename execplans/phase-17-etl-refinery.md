# ExecPlan: Chronicle ETL - The Universal Ingest Gateway

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md`.

## Purpose / Big Picture

We are transforming `chronicle-etl` from a simple Parquet extractor into the **Universal Ingest Gateway** for the Chronicle ecosystem. Currently, it only supports exporting data *out* of Chronicle (to Parquet). After this change, it will support ingestion *into* Chronicle, acting as the "Refinery" that converts raw, multiplexed feed data (Hot Path) into clean, structured, and snapshotted data (Warm Path).

The user will be able to run:
*   `chronicle-etl refine`: To tail a live raw queue and produce a clean, structured queue with snapshots.
*   `chronicle-etl extract`: To run the existing Parquet extraction (backward compatibility).

This enables the **Unified Storage Architecture**, where all historical data—whether from live trading or offline import—shares the exact same high-performance structure.

## Progress

- [ ] (2026-01-24) **Milestone 1:** CLI Refactoring & Dual Mode Support.
- [ ] (2026-01-24) **Milestone 2:** The Refinery Logic (Snapshot Injection).
- [ ] (2026-01-24) **Milestone 3:** Symbol Mapping & Configuration.
- [ ] (2026-01-24) **Milestone 4:** End-to-End Validation (Raw -> Refinery -> Clean).

## Surprises & Discoveries

*   (None yet)

## Decision Log

- **Decision:** Use `clap` subcommands for mode selection.
  **Rationale:** Allows a single binary to handle diverse ETL tasks (Refine, Extract, Import) without code duplication.
  **Date:** 2026-01-24
- **Decision:** Use `chronicle-replay`'s `ReplayEngine` as the shared input driver.
  **Rationale:** It already handles gap detection and order book reconstruction, preventing logic duplication between the Replayer and the Refinery.
  **Date:** 2026-01-24

## Outcomes & Retrospective

*   (Pending completion)

## Context and Orientation

*   **`crates/4-app/chronicle-etl/`**: The crate being modified.
*   **`chronicle-core`**: The underlying queue primitive (Reader/Writer).
*   **`chronicle-replay`**: Provides the `ReplayEngine` and `L2Book` logic.
*   **`chronicle-protocol`**: Defines the wire format (`BookEvent`, `Snapshot`, `Diff`).

The `Refinery` sits between the Raw Queue (Zone 1) and the Clean Queue (Zone 2). It must read raw events, maintain state, inject snapshots, and write clean events.

## Plan of Work

### Milestone 1: CLI Refactoring & Dual Mode Support

We need to break the existing `main.rs` which is hardcoded for `Extractor`.

1.  **Refactor `main.rs`:**
    *   Introduce `clap` `Parser` with subcommands: `Extract`, `Refine`.
    *   Move existing extraction logic into a `run_extract` function.
    *   Create a placeholder `run_refine` function.

2.  **Define `RefineArgs`:**
    *   `--source <path>`: Input Raw Queue.
    *   `--sink <path>`: Output Clean Queue.
    *   `--symbol <id>`: Symbol ID to filter/tag.
    *   `--snapshot-interval <seconds>`: Frequency of synthetic snapshots.

### Milestone 2: The Refinery Logic

We need to implement the `Refinery` struct in `src/refinery.rs` (created previously) and expose it via `lib.rs`.

1.  **Finalize `src/refinery.rs`:**
    *   Ensure it uses `ReplayEngine` to track state.
    *   Implement `inject_snapshot` using `QueueWriter::append_in_place`.
    *   Implement the `run` loop that checks `last_snapshot_time`.

2.  **Integrate in `main.rs`:**
    *   Wire `run_refine` to instantiate `Refinery` and call `run()`.

### Milestone 3: Symbol Mapping & Configuration

The protocol uses `u32` Market IDs, but humans use strings ("BTC-USDT").

1.  **Create `src/config.rs`:**
    *   Define `SymbolMap` struct (loadable from JSON/TOML).
    *   Simple mapping: `String -> u32`.
2.  **Update CLI:**
    *   Add `--config <path>` to load the map.
    *   Resolve symbol names to IDs before starting the Refinery.

### Milestone 4: End-to-End Validation

Prove that data flows correctly.

1.  **Test Scenario:**
    *   **Writer:** Write 1000 Raw `Diff` messages to `q_raw`.
    *   **Refinery:** Run `chronicle-etl refine --source q_raw --sink q_clean --interval 1s`.
    *   **Validation:** Read `q_clean`. Assert it contains `Snapshot` messages that did not exist in `q_raw`. Assert the `Diff` messages follow the snapshots.

## Concrete Steps

1.  **Modify `crates/4-app/chronicle-etl/src/main.rs`**: Replace with `clap` structure.
2.  **Modify `crates/4-app/chronicle-etl/src/refinery.rs`**: Ensure full implementation of snapshot injection.
3.  **Run `cargo build -p chronicle-etl`**: Verify compilation.
4.  **Create Integration Test**: `tests/refinery_integration.rs` in the `chronicle-etl` crate.

## Validation and Acceptance

1.  **Command:** `cargo run -p chronicle-etl -- --help`
    *   **Expect:** Help text showing `extract` and `refine` subcommands.
2.  **Command:** `cargo test -p chronicle-etl`
    *   **Expect:** All tests pass, including the new integration test showing snapshot injection.

## Interfaces and Dependencies

*   **`clap`**: For CLI parsing.
*   **`chronicle-replay::ReplayEngine`**: For state reconstruction.
*   **`chronicle-core::QueueWriter`**: For writing the clean stream.

## Idempotence and Recovery

*   The Refinery tracks `last_snapshot_ns` in memory. If it restarts, it will immediately emit a snapshot on the first message (because `last_snapshot_ns` resets to 0). This is safe and desired behavior (self-healing).
