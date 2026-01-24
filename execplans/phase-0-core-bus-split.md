# Split chronicle into core and bus crates

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept up to date as work proceeds.

This plan must be maintained in accordance with PLANS.md in the repository root.

## Purpose / Big Picture

The goal is to split the current single-crate repository into a Rust workspace with two crates: chronicle-core for the low-latency queue engine and chronicle-bus for the thin control-plane helpers. After this change, a developer can build or test the core queue independently, and the bus crate can evolve without bloating the data plane. The split is visible by running Cargo commands against the workspace and by seeing the new crate layout on disk.

## Progress

- [x] (2026-01-16 00:00Z) Capture current state and decisions for the core+bus split in this ExecPlan.
- [x] (2026-01-16 00:40Z) Convert the repository root into a workspace and move the current core crate into crates/1-primitives/chronicle-core.
- [x] (2026-01-16 00:45Z) Introduce crates/2-infra/chronicle-bus with the minimal module skeleton described in the design and a dependency on chronicle-core.
- [x] (2026-01-16 00:50Z) Validate the workspace builds and tests, and record evidence in this plan.

## Surprises & Discoveries

- Observation: chronicle-bus emitted dead-code warnings for unused fields in stub structs during the initial build.
  Evidence: cargo build -p chronicle-bus warned about unused RouterDiscovery.layout and ReaderRegistration.reader_name; resolved by adding accessor methods that read those fields.

## Decision Log

- Decision: Use a workspace-only root and two crates at crates/1-primitives/chronicle-core and crates/2-infra/chronicle-bus, with chronicle-bus depending on chronicle-core.
  Rationale: The design already specifies this layout, it keeps the hot path isolated, and shared types should not be duplicated.
  Date/Author: 2026-01-16, Codex
- Decision: Rename the core crate to chronicle-core and update tests to use chronicle_core paths.
  Rationale: The design names the data plane crate chronicle-core, and Rust crate naming conventions map to chronicle_core in code.
  Date/Author: 2026-01-16, Codex
- Decision: Define StrategyEndpoints as root/orders/queue/<strategy_id>/{orders_out,orders_in}.
  Rationale: Keeps the initial helper API concrete while staying aligned with the documented single-host layout; can be revised later if naming changes.
  Date/Author: 2026-01-16, Codex

## Outcomes & Retrospective

The repository is now a workspace with chronicle-core and chronicle-bus crates. Core sources and tests live under crates/1-primitives/chronicle-core, and the bus helper crate exists with minimal ready/lease/registration/discovery modules. The workspace builds and chronicle-core tests pass. No compatibility shim crate exists yet; add one later if external users require the chronicle crate name.

## Context and Orientation

The repository is now a workspace with a root Cargo.toml listing crates/1-primitives/chronicle-core and crates/2-infra/chronicle-bus. The chronicle-core crate contains the queue implementation under crates/1-primitives/chronicle-core/src and its tests under crates/1-primitives/chronicle-core/tests. The chronicle-bus crate lives under crates/2-infra/chronicle-bus/src and provides stubbed helper modules for layout, readiness, leases, discovery, and registration. The design document at docs/DESIGN.md still describes the split and matches the new layout.

Definitions:

A Rust workspace is a top-level Cargo.toml that lists member crates and allows building them together. A crate is a Rust package with its own Cargo.toml and src/ tree. In this plan, the root becomes a workspace and the existing crate moves to crates/1-primitives/chronicle-core.

## Plan of Work

First, create a workspace root by replacing the root Cargo.toml with a workspace manifest that lists crates/1-primitives/chronicle-core and crates/2-infra/chronicle-bus as members. Move the current crate into crates/1-primitives/chronicle-core, keeping its Cargo metadata and public API intact. Update any paths that assume src/ lives at the repository root, including tests and references in documentation or scripts. Next, create a new crates/2-infra/chronicle-bus crate with minimal module files matching the design section for layout, ready/lease helpers, discovery, and registration. The bus crate should declare a dependency on chronicle-core so it can open queues and use shared types. Finally, build and test the workspace to verify the split is correct, and record the command outputs in the Artifacts section.

## Concrete Steps

Work in the repository root: /Users/zjx/Documents/chronicle-rs.

1) Create the workspace structure and move the core crate.
   - Create crates/1-primitives/chronicle-core and move the current src/ there.
   - Move the current root Cargo.toml into crates/1-primitives/chronicle-core/Cargo.toml.
   - Replace the root Cargo.toml with a workspace manifest that lists crates/1-primitives/chronicle-core and crates/2-infra/chronicle-bus.
   - If there are tests under tests/, decide whether they are core tests; if so, move them under crates/1-primitives/chronicle-core/tests/ and update any path assumptions.

2) Add the chronicle-bus crate skeleton.
   - Create crates/2-infra/chronicle-bus/Cargo.toml with standard package metadata and a dependency on chronicle-core via a path dependency.
   - Create crates/2-infra/chronicle-bus/src/lib.rs and the module files layout.rs, ready.rs, lease.rs, discovery.rs, and registration.rs with empty or minimal stub implementations.
   - Ensure the module tree compiles by re-exporting or defining minimal structs and functions described in the design.

3) Update references and documentation.
   - Update any documentation that assumes a single crate, including docs/DESIGN.md if it references root src/ paths.
   - Ensure the new workspace layout matches the tree shown in the design document.

4) Build and test the workspace.
   - Run cargo build -p chronicle-core and cargo test -p chronicle-core.
   - Run cargo build -p chronicle-bus to ensure the new crate compiles.

Example commands (from the repo root):

    cargo build -p chronicle-core
    cargo test -p chronicle-core
    cargo build -p chronicle-bus

Record short output snippets in Artifacts and update Progress.

## Validation and Acceptance

Acceptance is met when:

The workspace root Cargo.toml lists chronicle-core and chronicle-bus as members, the chronicle-core crate builds and tests cleanly with cargo test -p chronicle-core, and the chronicle-bus crate builds with cargo build -p chronicle-bus. The source tree should show crates/1-primitives/chronicle-core/src containing the existing queue code, and crates/2-infra/chronicle-bus/src containing the new helper modules.

If tests are not present, document that cargo test -p chronicle-core completes with zero tests run, and that cargo build -p chronicle-core succeeds. Capture those outputs in Artifacts.

## Idempotence and Recovery

All steps are safe to re-run. If a move goes wrong, revert by moving files back to the original locations and restoring the original root Cargo.toml. Avoid deleting any files until the workspace build succeeds. If a Cargo command fails due to paths, correct the paths and rerun without needing to delete artifacts.

## Artifacts and Notes

Command transcripts from validation:

    $ cargo build -p chronicle-core
       Compiling chronicle-core v0.1.0 (/Users/zjx/Documents/chronicle-rs/crates/1-primitives/chronicle-core)
        Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.47s

    $ cargo test -p chronicle-core
       Compiling chronicle-core v0.1.0 (/Users/zjx/Documents/chronicle-rs/crates/1-primitives/chronicle-core)
        Finished `test` profile [unoptimized + debuginfo] target(s) in 0.46s
         Running unittests src/lib.rs (target/debug/deps/chronicle_core-7caf67f903ec7936)

    running 2 tests
    test header::tests::crc_matches_known_payload ... ok
    test header::tests::header_size_and_alignment ... ok

    test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

         Running tests/append_single_writer.rs (target/debug/deps/append_single_writer-ca23a00876b4b590)

    running 1 test
    test append_two_messages_and_recover ... ok

    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s

         Running tests/merge_fanin.rs (target/debug/deps/merge_fanin-260dcab1936e1f4e)

    running 2 tests
    test merge_orders_by_timestamp_and_source ... ok
    test merge_returns_none_when_empty ... ok

    test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s

         Running tests/notifier_wake.rs (target/debug/deps/notifier_wake-75394af6b7d98dd2)

    running 0 tests

    test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

         Running tests/reader_recovery.rs (target/debug/deps/reader_recovery-3a57d33e3ee76b9c)

    running 1 test
    test read_commit_and_recover ... ok

    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s

         Running tests/retention_cleanup.rs (target/debug/deps/retention_cleanup-39434c22f3af893a)

    running 1 test
    test retention_deletes_only_when_all_readers_advance ... ok

    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s

         Running tests/retention_lag.rs (target/debug/deps/retention_lag-e0262d82a4c495a6)

    running 1 test
    test retention_ignores_lagging_reader ... ok

    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

         Running tests/round_trip.rs (target/debug/deps/round_trip-2d0ad5565c2ada62)

    running 1 test
    test append_read_round_trip ... ok

    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

         Running tests/segment_rollover.rs (target/debug/deps/segment_rollover-facbe90c1d8ccfd1)

    running 1 test
    test segment_rollover_and_read_across ... ok

    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s

         Running tests/smoke.rs (target/debug/deps/smoke-7402982e9cff4786)

    running 1 test
    test placeholder_smoke_test ... ok

    test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

       Doc-tests chronicle_core

    running 0 tests

    test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

    $ cargo build -p chronicle-bus
       Compiling chronicle-bus v0.1.0 (/Users/zjx/Documents/chronicle-rs/crates/2-infra/chronicle-bus)
        Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.21s

## Interfaces and Dependencies

chronicle-core should continue to export the core API under crates/1-primitives/chronicle-core/src/lib.rs, including Queue, QueueWriter, QueueReader, MessageView, and WaitStrategy. chronicle-bus should declare a dependency on chronicle-core in crates/2-infra/chronicle-bus/Cargo.toml using a path dependency.

At minimum, chronicle-bus/src/lib.rs should declare the modules layout, ready, lease, discovery, and registration so downstream code can import them as chronicle_bus::layout, chronicle_bus::ready, and so on. The module contents can start as minimal stubs that compile but must align with the names in docs/DESIGN.md to avoid future renames.

Plan change note (2026-01-16): Updated Progress, Context, and Artifacts after implementing the workspace split and validating builds/tests; recorded decisions and findings from the build.
