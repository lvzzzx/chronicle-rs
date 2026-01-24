# Phase 10: CLI Tooling (chronicle-cli)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

After this change, operators can use a single command-line tool to inspect queue health, tail messages for debugging, diagnose stale locks and retention candidates across a bus root, and run a small local throughput/latency benchmark. The tool reads the same on-disk metadata that the runtime uses, so it can validate queue state without needing any running services.

## Progress

- [x] (2026-01-17 23:59Z) Drafted Phase 10 ExecPlan for chronicle-cli.
- [x] (2026-01-17 04:14Z) Implemented chronicle-cli crate, workspace wiring, and subcommand behavior.
- [x] (2026-01-17 04:14Z) Added core helpers for lock inspection and retention candidate listing.
- [x] (2026-01-17 04:14Z) Updated README and ROADMAP to mention the CLI and Phase 10 status.
- [x] (2026-01-17 04:15Z) Validated bench/inspect/tail/doctor against a local queue and captured output.

## Surprises & Discoveries

None yet.

## Decision Log

- Decision: Implement `chronicle-cli` as a new workspace crate under `crates/4-app/chronicle-cli` with a binary name of `chron-cli`.
  Rationale: Keeps tooling colocated with the rest of the workspace while matching the DESIGN.md command name.
  Date/Author: 2026-01-17, Codex
- Decision: Use `clap` for argument parsing rather than handwritten `std::env::args`.
  Rationale: Subcommands and flags would otherwise add a lot of error-prone parsing code; a small dependency is acceptable for tooling.
  Date/Author: 2026-01-17, Codex
- Decision: Implement `tail` using the existing `QueueReader` API with a unique reader name, even though it creates a reader metadata file.
  Rationale: This ensures correct parsing of segments and commit semantics without duplicating the reader logic. The CLI will namespace its reader name to minimize interference.
  Date/Author: 2026-01-17, Codex

## Outcomes & Retrospective

Implemented a new `chronicle-cli` crate that provides inspect, tail, doctor, and bench commands using core metadata and reader APIs. Added core helpers for lock inspection and retention candidate listing, and validated the commands against a local queue. The CLI is now usable for basic operational inspection and debugging. Remaining gaps: lock owner inspection is Linux-only (non-Linux reports the lock as present but cannot validate PID liveness).

## Context and Orientation

The queue on-disk format lives in `crates/1-primitives/chronicle-core`. A queue directory contains `control.meta` (writer position and segment size), `index.meta` (last flushed segment/offset), `writer.lock` (lock owner identity), `readers/<name>.meta` files (reader positions + heartbeat), and one or more segment files named `000000000.q`, `000000001.q`, etc. Segment headers and message headers are defined in `crates/1-primitives/chronicle-core/src/segment.rs` and `crates/1-primitives/chronicle-core/src/header.rs`. Reader metadata is loaded via `load_reader_meta` in `crates/1-primitives/chronicle-core/src/segment.rs`. Writer lock metadata is currently handled in `crates/1-primitives/chronicle-core/src/writer_lock.rs` but not exposed publicly, so CLI support will require either public helpers or mirrored parsing logic in the CLI.

The bus layout helpers live in `crates/2-infra/chronicle-bus/src/layout.rs` and describe the `orders/queue/<strategy>/orders_out` and `orders/queue/<strategy>/orders_in` directories. The `doctor` subcommand will traverse a bus root and find queue directories by locating `control.meta` files rather than relying on a single fixed layout.

There is no existing CLI crate, so this phase must add a new workspace member and a new binary crate.

## Plan of Work

Introduce a new crate at `crates/4-app/chronicle-cli` with a binary entrypoint named `chron-cli`. Wire it into the workspace in the root `Cargo.toml`. Implement a `main.rs` that uses `clap` to parse subcommands. The CLI will depend on `chronicle-core` (always) and optionally `chronicle-bus` (for consistent path handling when helpful). Keep all output plain text with stable key=value fields so it can be parsed by scripts if needed.

For `inspect <queue_path>`, open the control file with `ControlFile::open` and `wait_ready()`, then read the seqlock-protected segment index (`segment_index`), segment size, writer epoch, and heartbeat. Read `index.meta` if present to show last flushed segment/offset. Enumerate reader metadata files under `<queue_path>/readers`, call `load_reader_meta` for each, and compute lag in bytes and segments relative to the head position (head = current_segment * segment_size + write_offset). For process liveness, expose a small public helper in `crates/1-primitives/chronicle-core/src/writer_lock.rs` that reads and reports the lock record (`pid`, `start_time`, `writer_epoch`) and checks `/proc/<pid>/stat` to determine if the owner is alive on Linux. Mark the lock as stale if the record exists but the owner is not alive.

For `tail <queue_path> [-f] [--reader <name>] [--limit <n>]`, open a `QueueReader` using a default reader name like `cli_tail_<pid>` (overrideable by `--reader`). Drain messages using `next()`. For each message, print a header line with `seq`, `timestamp_ns`, `type_id`, and `payload_len`, followed by a hex dump of the payload (16 bytes per line, offset-prefixed). If `-f` is set, call `wait(None)` when no message is available and continue; otherwise exit when the queue is caught up or `--limit` messages have been printed. Call `commit()` periodically so the reader metadata advances (for example after each message).

For `doctor <bus_root>`, recursively scan the directory tree for folders that contain `control.meta` files and treat each as a queue root. For each queue root, run the same lock check as `inspect` and report stale locks. Compute retention candidates by calling a new helper in `crates/1-primitives/chronicle-core/src/retention.rs` that returns the list of segment IDs eligible for deletion without deleting them. This helper should be a pure read-only variant of `cleanup_segments`, using `min_live_reader_segment` and a scan of segment filenames to return candidates. Print results in a grouped format per queue root, including counts and the range of candidate segment IDs.

For `bench`, implement a local write/read microbenchmark. The command should create a queue under a user-provided path (`--queue-path`) or a unique directory under the system temp directory. Write N messages (default 100_000) of a fixed payload size (default 256 bytes) using `Queue::open_publisher`. Record the elapsed time and print throughput (messages/sec and MB/sec). If `--read` is specified, open a reader and drain all messages to measure end-to-end throughput. If `--keep` is not set, delete the benchmark directory on completion so it does not pollute the filesystem.

Update `docs/ROADMAP.md` to mark Phase 10 as in progress or complete once validation is done, and add a brief README section listing the CLI commands with sample invocations.

## Concrete Steps

Work from the repo root.

1) Add the new crate and workspace wiring.

    - Create `crates/4-app/chronicle-cli/Cargo.toml` with dependencies on `chronicle-core` and `clap`.
    - Create `crates/4-app/chronicle-cli/src/main.rs`.
    - Add `crates/4-app/chronicle-cli` to the workspace `Cargo.toml` members list.

2) Add public lock/retention helpers in core for CLI use.

    - In `crates/1-primitives/chronicle-core/src/writer_lock.rs`, add a public `WriterLockInfo` struct and public functions:
        - `pub fn read_lock_info(path: &Path) -> Result<Option<WriterLockInfo>>`
        - `pub fn lock_owner_alive(info: &WriterLockInfo) -> Result<bool>` (Linux) or a simple `true` on non-Linux.
    - In `crates/1-primitives/chronicle-core/src/retention.rs`, add:
        - `pub fn retention_candidates(root: &Path, head_segment: u64, head_offset: u64, segment_size: u64) -> Result<Vec<u64>>`
      Implement it by copying the scan logic from `cleanup_segments` but without deletions.

3) Implement CLI subcommands.

    - In `crates/4-app/chronicle-cli/src/main.rs`, define the CLI struct with `clap` subcommands: `inspect`, `tail`, `doctor`, `bench`.
    - Implement `inspect` by reading control/index/readers and printing a summary.
    - Implement `tail` by streaming messages using `Queue::open_subscriber` with a unique reader name.
    - Implement `doctor` by scanning for queue roots and printing stale locks and retention candidates.
    - Implement `bench` by writing (and optionally reading) messages under a temp or user-provided path.

4) Update docs.

    - `docs/ROADMAP.md`: mark Phase 10 status after validation.
    - `README.md`: add a short CLI section with example commands.

## Validation and Acceptance

Acceptance is met when the CLI can be built and the four subcommands produce correct, human-verifiable output.

1) Build the CLI:

    cargo build -p chronicle-cli

2) Create a queue and inspect it using the bench command:

    cargo run -p chronicle-cli -- bench --queue-path ./cli_demo --messages 1000 --payload-bytes 64 --keep
    cargo run -p chronicle-cli -- inspect ./cli_demo

Expected output should include the head segment/offset and at least one reader (if `bench --read` was used). The inspect output should show a valid control version and segment size.

3) Tail the queue:

    cargo run -p chronicle-cli -- tail ./cli_demo --limit 3

Expected output includes at least 3 message headers and hexdumps.

4) Doctor scan:

    cargo run -p chronicle-cli -- doctor ./cli_demo

Expected output shows the queue root discovered and either “no stale locks” or a stale lock report if you manually kill a writer process.

5) Optional full workspace test:

    cargo test

## Idempotence and Recovery

The CLI must be safe to run repeatedly. `bench --keep` will leave its queue directory in place; remove it with `rm -rf ./cli_demo` to reset. If `tail` creates a reader metadata file, it should use a unique name so repeated runs do not collide. The `doctor` command should never delete files; it only reports candidates.

## Artifacts and Notes

Keep short example outputs for README updates. Example (format is illustrative):

    chron-cli inspect ./cli_demo
    head_segment=0 head_offset=4096 segment_size=134217728 writer_epoch=1
    reader name=cli_tail_12345 segment=0 offset=4352 lag_bytes=256
    lock pid=12345 alive=true

    chron-cli tail ./cli_demo --limit 1
    seq=0 ts=1700000000000 type=1 len=64
    0000: 01 02 03 04 ...

Validation sample (from 2026-01-17):

    chron-cli bench --queue-path /tmp/chronicle_cli_demo --messages 1000 --payload-bytes 64 --keep
    queue_path=/tmp/chronicle_cli_demo
    write_elapsed_ms=5 write_msg_per_sec=175492.48 write_mb_per_sec=10.71
    cleanup=kept

    chron-cli inspect /tmp/chronicle_cli_demo
    queue=/tmp/chronicle_cli_demo
    head_segment=0 head_offset=128064 head_pos=128064
    segment_size=134217728 writer_epoch=1
    writer_heartbeat_ns=1768623326916468000
    index_segment=0 index_offset=128064
    lock pid=0 start_time=0 epoch=0 alive=true

## Interfaces and Dependencies

Use `clap` for CLI parsing (version 4.x). Add these dependencies in `crates/4-app/chronicle-cli/Cargo.toml`:

    [dependencies]
    clap = { version = "4", features = ["derive"] }
    chronicle-core = { path = "../chronicle-core" }
    chronicle-bus = { path = "../chronicle-bus" }

In `crates/1-primitives/chronicle-core/src/writer_lock.rs`, define:

    pub struct WriterLockInfo {
        pub pid: u32,
        pub start_time: u64,
        pub epoch: u64,
    }

    pub fn read_lock_info(path: &Path) -> Result<Option<WriterLockInfo>>

In `crates/1-primitives/chronicle-core/src/retention.rs`, define:

    pub fn retention_candidates(root: &Path, head_segment: u64, head_offset: u64, segment_size: u64) -> Result<Vec<u64>>

Change note: 2026-01-17 created Phase 10 ExecPlan for the chronicle-cli tooling work.
Change note: 2026-01-17 implemented the CLI crate, core helpers, and initial docs updates; validation remains.
Change note: 2026-01-17 recorded CLI validation runs and marked the plan complete.
