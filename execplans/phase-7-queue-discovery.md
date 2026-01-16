# Implement Queue Discovery for Multi-Process Fan-In (Phase 7)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `PLANS.md` at the repository root.

## Purpose / Big Picture

Queue discovery (scan + watch + READY markers) is now classified as control-plane behavior and belongs in `chronicle-bus`, not in this `chronicle-core` repository. The purpose of this document is to preserve the previously drafted Phase 7 plan and to record that implementation has moved out of scope for this repo after the design split in `docs/DESIGN.md`.

## Progress

- [x] (2026-01-15 23:40Z) ExecPlan drafted for Phase 7 queue discovery.
- [x] (2026-01-16 21:25Z) Scope moved to `chronicle-bus` per `docs/DESIGN.md`; no Phase 7 implementation planned in `chronicle-core`.
- [ ] Implement queue discovery in the `chronicle-bus` repository (out of scope for this repo).

## Surprises & Discoveries

None yet. Capture any watcher behavior differences (macOS vs Linux), or missed-event edge cases with evidence, in the `chronicle-bus` plan.

## Decision Log

- Decision: Queue discovery (READY markers, scan + watch, attach/detach) is control-plane behavior and should live in `chronicle-bus`.
  Rationale: `docs/DESIGN.md` explicitly splits the data plane (`chronicle-core`) from the control plane helper (`chronicle-bus`), and discovery falls under the control plane responsibilities.
  Date/Author: 2026-01-16, Codex

## Outcomes & Retrospective

Phase 7 is intentionally not implemented in this repository after the design split. The work should be carried out in a `chronicle-bus` repo with its own ExecPlan.

## Context and Orientation

`chronicle-core` provides the data plane: on-disk queue format, reader/writer semantics, segment rolling, and fan-in merge. `docs/DESIGN.md` now assigns queue discovery, READY/LEASE markers, and router attachment logic to `chronicle-bus` as a thin control-plane helper. As a result, Phase 7 work is not appropriate to land in this repo.

## Plan of Work

No implementation work is planned in this repository. If queue discovery work resumes, it should be moved to a `chronicle-bus` repository with a fresh ExecPlan that mirrors the design responsibilities outlined in `docs/DESIGN.md`.

## Concrete Steps

No commands to run in this repository for Phase 7. Future work should be done in the `chronicle-bus` repository.

## Validation and Acceptance

Acceptance criteria for queue discovery should be defined and validated in the `chronicle-bus` repository. There are no acceptance tests in this repo for Phase 7.

## Idempotence and Recovery

Not applicable for `chronicle-core`, as no changes are planned.

## Artifacts and Notes

Not applicable in this repository. Maintain discovery artifacts (tests, traces) with the `chronicle-bus` implementation.

## Interfaces and Dependencies

No new interfaces or dependencies are required in `chronicle-core` for Phase 7. Discovery interfaces should be defined by `chronicle-bus`.

Change note: 2026-01-16 updated this ExecPlan to mark Phase 7 as moved to `chronicle-bus` after the design split in `docs/DESIGN.md`.
