# Repository Guidelines

## Project Structure & Module Organization
- Current repository is documentation-only: `docs/DESIGN.md` captures the core architecture.
- When implementation lands, expect Rust sources under `src/` and any integration tests under `tests/`.
- Persistent queue data is designed to live under paths like `/var/lib/hft_bus/<bus>/queue/` with per-writer segments and per-reader metadata.

## Architecture Overview
- The system is a low-latency, memory-mapped, append-only queue with durable persistence.
- Each message uses a 64-byte header with a valid/commit flag; writers set the flag with Release ordering, readers acquire it.
- Readers track progress independently via per-reader metadata files; notification uses Linux `eventfd`.
- Segments roll at a fixed size and can be reclaimed once all readers have passed their ranges.

## Build, Test, and Development Commands
- No build or test commands are defined yet (no `Cargo.toml` present).
- If you add the Rust crate, document canonical commands in this section (e.g., `cargo build`, `cargo test`) and keep them in sync with project scripts.

## Coding Style & Naming Conventions
- Use standard Rust formatting (`rustfmt` defaults) and idiomatic naming (`snake_case` for functions/modules, `CamelCase` for types).
- Keep concurrency-critical code well documented, especially memory ordering and cross-thread invariants.

## Testing Guidelines
- There are no tests in the repository yet.
- When adding tests, prefer `#[cfg(test)]` unit tests near implementation and integration tests in `tests/` with descriptive names like `reader_recovery.rs`.

## Commit & Pull Request Guidelines
- Current history uses short, imperative messages; one commit uses a `docs:` prefix. Follow that pattern when appropriate (e.g., `docs: clarify segment rolling`).
- PRs should explain intent, list key changes, and call out any behavior or performance impacts. Include design updates in `docs/DESIGN.md` when architecture changes.

## Security & Configuration Tips
- The design expects OS-level resources (`mmap`, `eventfd`, `inotify`) and a writable data directory. Keep paths configurable and avoid hard-coding `/var/lib/...` for local dev.

# ExecPlans

When writing complex features or significant refactors, use an ExecPlan (as described in `PLANS.md`) from design to implementation.
