# Repository Guidelines

## Project Structure & Module Organization
- Cargo workspace at the repo root (`Cargo.toml`).
- Rust crates live under `crates/` (currently `crates/chronicle-core` and `crates/chronicle-bus`).
- Architecture documentation lives in `docs/` (notably `docs/DESIGN.md`).
- Persistent queue data is designed to live under paths like `/var/lib/hft_bus/<bus>/queue/` with per-writer segments and per-reader metadata.

## Architecture Overview
- The system is a low-latency, memory-mapped, append-only queue with durable persistence.
- Each message uses a 64-byte header with a valid/commit flag; writers set the flag with Release ordering, readers acquire it.
- Readers track progress independently via per-reader metadata files; notification uses Linux `eventfd`.
- Segments roll at a fixed size and can be reclaimed once all readers have passed their ranges.

## Build, Test, and Development Commands
- `cargo build` (workspace build)
- `cargo test` (workspace tests)
- `cargo test -p chronicle-core` (crate-local tests)
- `cargo bench -p chronicle-core` (benchmarks)

## Coding Style & Naming Conventions
- Use standard Rust formatting (`rustfmt` defaults) and idiomatic naming (`snake_case` for functions/modules, `CamelCase` for types).
- Keep concurrency-critical code well documented, especially memory ordering and cross-thread invariants.

## Testing Guidelines
- Prefer `#[cfg(test)]` unit tests near implementation and integration tests in each crate’s `tests/` directory.
- Follow existing naming patterns (e.g., `reader_recovery.rs` under `crates/chronicle-core/tests/`).

## Commit & Pull Request Guidelines
- Current history uses short, imperative messages; one commit uses a `docs:` prefix. Follow that pattern when appropriate (e.g., `docs: clarify segment rolling`).
- PRs should explain intent, list key changes, and call out any behavior or performance impacts. Include design updates in `docs/DESIGN.md` when architecture changes.

## Security & Configuration Tips
- The design expects OS-level resources (`mmap`, `eventfd`, `inotify`) and a writable data directory. Keep paths configurable and avoid hard-coding `/var/lib/...` for local dev.

# NOTES.md

Use `NOTES.md` as a lightweight, durable knowledge cache for:
- Domain knowledge specific to this project.
- Codebase gotchas or footguns that are easy to forget.
- Known issues or dead ends to avoid re-suggesting.

Operational guidance:
- At the start of any non-trivial task, read `NOTES.md`.
- Before proposing a design or fix, check `NOTES.md` for known pitfalls and ruled-out approaches.
- Only update `NOTES.md` when a durable, reusable fact is discovered (domain invariant, codebase gotcha, or known issue).
- Do not duplicate planning or progress logs from `PLANS.md` or ExecPlans.

# ExecPlans

When writing complex features or significant refactors, use an ExecPlan (as described in `PLANS.md`) from design to implementation.
