# Implement Queue Discovery for Multi-Process Fan-In (Phase 7)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Implement queue discovery (scan + watch + READY markers) in `crates/2-infra/chronicle-bus` so a router can attach/detach strategy queues without restarts. This ExecPlan tracks the Phase 7 implementation in this workspace after the control-plane split in `docs/DESIGN.md`.

## Progress

- [x] (2026-01-15 23:40Z) ExecPlan drafted for Phase 7 queue discovery.
- [x] (2026-01-16 21:25Z) Scope moved to `chronicle-bus` per `docs/DESIGN.md`.
- [x] (2026-01-17 23:25Z) Implemented `RouterDiscovery::poll()` with scan + watch + dedup in `crates/2-infra/chronicle-bus`.
- [x] (2026-01-17 23:26Z) Added Linux inotify watcher and periodic rescan fallback; non-Linux uses scan-only.

## Surprises & Discoveries

- Non-Linux platforms rely on periodic scans only; add kqueue support if event-driven discovery is required on macOS.

## Decision Log

- Decision: Queue discovery (READY markers, scan + watch, attach/detach) is control-plane behavior and should live in `chronicle-bus`.
  Rationale: `docs/DESIGN.md` explicitly splits the data plane (`chronicle-core`) from the control plane helper (`chronicle-bus`), and discovery falls under the control plane responsibilities.
  Date/Author: 2026-01-16, Codex
- Decision: Use Linux inotify to trigger rescans and fall back to periodic scan for missed events.
  Rationale: Best-effort discovery tolerates missed events; rescans keep correctness with low complexity.
  Date/Author: 2026-01-17, Codex

## Outcomes & Retrospective

`RouterDiscovery::poll()` now performs scan + watch + dedup in `crates/2-infra/chronicle-bus`, emitting Added/Removed events based on READY markers. Future enhancement: add kqueue watcher for macOS if event-driven discovery is required.

## Context and Orientation

`chronicle-core` provides the data plane: on-disk queue format, reader/writer semantics, segment rolling, and fan-in merge. `chronicle-bus` provides control-plane helpers (READY/LEASE markers, discovery, registration). Phase 7 is implemented in `crates/2-infra/chronicle-bus` within this workspace.

## Plan of Work

Completed:
- Implement `RouterDiscovery::poll()` to scan READY markers under `orders/queue`.
- Add Linux inotify watcher to mark discovery dirty and trigger rescans.
- Add periodic rescan fallback for missed events or non-Linux targets.

## Concrete Steps

- `cargo test -p chronicle-bus`

## Validation and Acceptance

- `RouterDiscovery::poll()` emits Added/Removed based on READY markers in `orders/queue/<strategy>/orders_out/READY`.
- `cargo test -p chronicle-bus` passes.

## Idempotence and Recovery

- Discovery is best-effort; periodic rescan restores correctness after missed events.

## Artifacts and Notes

- Implementation: `crates/2-infra/chronicle-bus/src/discovery.rs`

## Interfaces and Dependencies

- Linux builds add a `libc` dependency for inotify in `chronicle-bus`.

Change note: 2026-01-17 updated this ExecPlan to reflect Phase 7 implementation in `crates/2-infra/chronicle-bus`.
