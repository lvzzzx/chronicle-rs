# Phase 9: Docs, Examples, and Operational Guidance

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

After this change, a user can run three example processes (feed, strategy, router) that demonstrate the end-to-end topology described in `docs/DESIGN.md`, including READY markers and dynamic discovery. The documentation will explain the on-disk layout, the configuration knobs that matter for deployment, and how to run the examples to observe message flow in a reproducible way.

## Progress

- [x] (2026-01-17 23:40Z) Drafted Phase 9 ExecPlan with scope, milestones, and acceptance criteria.
- [x] (2026-01-17 23:58Z) Implemented example binaries in `crates/2-infra/chronicle-bus/examples` (`feed`, `strategy`, `router`).
- [x] (2026-01-17 23:59Z) Updated `README.md`, `docs/DESIGN.md`, and `docs/ROADMAP.md` to reflect examples and layout.
- [ ] Validate by running the three processes and capturing expected output for the docs (completed: `cargo test -p chronicle-bus`; remaining: run the three example binaries concurrently).

## Surprises & Discoveries

None yet.

## Decision Log

- Decision: Place the examples under `crates/2-infra/chronicle-bus/examples` instead of a workspace-root `examples/` directory.
  Rationale: The workspace root is not a Cargo package, so root-level examples would not compile. `chronicle-bus` already depends on `chronicle-core`, so example binaries can use both crates without adding new packages.
  Date/Author: 2026-01-17, Codex
- Decision: Use simple ASCII payloads (key=value text) instead of JSON to avoid adding dependencies.
  Rationale: Keep the examples minimal and dependency-free while still being human-readable in logs.
  Date/Author: 2026-01-17, Codex
- Decision: Provide defaults for `--strategy` and `--symbol` in the strategy example.
  Rationale: Keep the quickstart frictionless while still allowing overrides.
  Date/Author: 2026-01-17, Codex

## Outcomes & Retrospective

Pending implementation. This section will summarize what users can run, what documentation was updated, and any remaining gaps.

## Context and Orientation

The workspace contains two crates: `crates/1-primitives/chronicle-core` (data-plane queue storage and reader/writer APIs) and `crates/2-infra/chronicle-bus` (control-plane helpers like discovery and READY markers). `chronicle-bus` exposes `RouterDiscovery`, `BusLayout`, and `ReaderRegistration` in `crates/2-infra/chronicle-bus/src`. The queue API lives in `crates/1-primitives/chronicle-core/src/writer.rs` (`Queue::open_publisher`) and `crates/1-primitives/chronicle-core/src/reader.rs` (`Queue::open_subscriber`, `QueueReader::next`, `QueueReader::wait`, `QueueReader::commit`). The discovery helper uses READY files under `orders/queue/<strategy>/orders_out/READY`, so the examples must create that file with `BusLayout::mark_ready`.

The goal for Phase 9 is to add runnable example binaries and update docs so a new user can follow one set of instructions to watch messages flow from feed to strategy to router and observe discovery events.

## Plan of Work

Create three example binaries under `crates/2-infra/chronicle-bus/examples` named `feed.rs`, `strategy.rs`, and `router.rs`. Each will parse a small set of command-line arguments using `std::env::args` (no external CLI dependency). Use a shared bus root directory argument (`--bus-root`) with a default of `./demo_bus` so the example can run in a local checkout without special privileges. The examples will use simple text payloads formatted as `key=value` pairs to keep parsing trivial.

For `feed.rs`, open a publisher queue at `<bus_root>/market_data/queue/demo_feed` and append market data messages on a fixed interval (for example every 200ms). Each message should include a symbol and price, alternating among a small fixed list (BTC, ETH, SOL). Log each publish to stdout.

For `strategy.rs`, open a subscriber to the feed queue using a reader name based on the strategy id (for example `strategy_<id>`). Filter messages by a single symbol passed via `--symbol`. When a match occurs, open or reuse a publisher for the strategy’s orders_out queue at `BusLayout::strategy_endpoints`, append an order message, and print the order to stdout. After creating the orders_out publisher, call `BusLayout::mark_ready(&orders_out)` exactly once to create the READY marker that `RouterDiscovery` expects. The strategy should also attempt to open a subscriber on its orders_in queue to receive acks. Because the router creates the orders_in writer, wrap `Queue::open_subscriber` in a retry loop with a short sleep until the queue exists; once opened, check for acks each loop iteration and print any received.

For `router.rs`, create a `RouterDiscovery` using the same bus root. Maintain a map from `StrategyId` to a reader for each orders_out queue and a writer for each orders_in queue. Each poll cycle should: (a) call `discovery.poll()` to find Added and Removed strategies, (b) for each Added strategy, open a subscriber on `orders_out` and open a publisher on `orders_in`, call `mark_ready` on `orders_in`, and log the attachment, (c) for each Removed strategy, drop the reader and writer and log detachment, and (d) iterate existing readers to drain available orders using `next()`; for each order read, write an ack message to the corresponding orders_in writer and log it. If no orders are read in a cycle, sleep briefly (for example 50ms) to avoid a busy loop.

Update `README.md` to add a Quickstart section that shows how to run the three example binaries from separate terminals, including the bus root argument and the expected log messages. Update `docs/DESIGN.md` where it describes the orders layout so the directory tree matches the current `BusLayout` implementation (`orders/queue/<strategy>/orders_out` and `orders/queue/<strategy>/orders_in`). Add a short configuration guidance paragraph in either `README.md` or `docs/DESIGN.md` describing how to choose a writable bus root (for example `/var/lib/hft_bus` in production and `./demo_bus` or `/tmp/chronicle_demo` for local runs).

When the examples are complete, update `docs/ROADMAP.md` Phase 9 status to completed, referencing the examples and the README quickstart. Record the validation commands in this ExecPlan as evidence of success.

## Concrete Steps

Work from the repo root.

1) Create the example binaries.

    - Add `crates/2-infra/chronicle-bus/examples/feed.rs`.
    - Add `crates/2-infra/chronicle-bus/examples/strategy.rs`.
    - Add `crates/2-infra/chronicle-bus/examples/router.rs`.

2) Update documentation.

    - Edit `README.md` to include a Quickstart for running the examples.
    - Edit `docs/DESIGN.md` to align the orders layout and discovery notes with actual paths.
    - Edit `docs/ROADMAP.md` to mark Phase 9 complete after validation.

3) Build and run.

    - Build: `cargo build -p chronicle-bus`
    - Terminal 1: `cargo run -p chronicle-bus --example feed -- --bus-root ./demo_bus`
    - Terminal 2: `cargo run -p chronicle-bus --example strategy -- --bus-root ./demo_bus --strategy strategy_a --symbol BTC`
    - Terminal 3: `cargo run -p chronicle-bus --example router -- --bus-root ./demo_bus`

Expected sample output (illustrative, keep short):

    feed: published symbol=BTC price=...
    strategy strategy_a: order symbol=BTC qty=1
    router: discovered strategy strategy_a
    router: order from strategy_a symbol=BTC -> acked
    strategy strategy_a: ack order_id=...

## Validation and Acceptance

Acceptance is met when the example processes run concurrently and demonstrate the message flow. The router must log Added events when the strategy READY marker appears. The strategy must show at least one order emitted and at least one ack received. The bus root directory should contain `orders/queue/<strategy>/orders_out/READY` and `orders/queue/<strategy>/orders_in/READY` files after both sides are running. To observe a Removed event, delete `orders/queue/<strategy>/orders_out/READY` while the router runs. Run `cargo test -p chronicle-bus` and expect it to pass with no failures.

## Idempotence and Recovery

The examples should be safe to rerun. If the demo bus directory already exists, delete it before rerunning:

    rm -rf ./demo_bus

If a queue directory is left in a partially initialized state, removing the bus root and restarting the examples is sufficient to recover.

## Artifacts and Notes

Include short excerpts of the example output in the README Quickstart. Capture any log lines that prove discovery (Added/Removed) and the order-to-ack round trip.

    cargo test -p chronicle-bus
    test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

## Interfaces and Dependencies

Use only standard library utilities for argument parsing. Example code should import:

    chronicle_bus::{BusLayout, RouterDiscovery}
    chronicle_core::Queue

No new external dependencies should be introduced for Phase 9. If a future enhancement needs structured payloads, prefer adding it after the basic examples are complete.

Change note: 2026-01-17 created Phase 9 ExecPlan to cover docs, examples, and operational guidance.
Change note: 2026-01-17 updated progress and acceptance criteria after implementing examples, doc updates, and initial test validation.
