# Chronicle-RS

**Chronicle-RS** is a low-latency, persisted messaging framework for high-frequency trading (HFT) systems, written in Rust. It is designed for single-host deployments where multiple processes (e.g., market feed, strategy, order router) communicate via shared memory-mapped queues.

## Project Overview

The system is architecturally split into two main components:

-   **Data Plane (`chronicle-core`):** The core queue engine. It handles:
    -   On-disk segment format and alignment.
    -   Single-writer, lock-free append protocol (with atomic commit).
    -   Multiple independent readers with persisted offsets.
    -   Segment rolling and retention.
    -   Hybrid busy-spin / futex waiting strategies.

-   **Control Plane Helper (`chronicle-bus`):** A thin utility layer for coordination. It handles:
    -   Standard directory layout (`/queue/<strategy>/...`).
    -   Service readiness (`READY`) and liveness (`LEASE`) signals.
    -   Router discovery (scanning and watching for new queues).
    -   RAII registration for clean shutdowns.

**Key Design Principles:**
-   **Single-Host:** Optimized for inter-process communication (IPC) on a single machine.
-   **One-Writer-Per-Queue:** Avoiding contention; scale by adding more queues (fan-in).
-   **Persistence:** Readers resume from their last committed offset after restarts.
-   **Zero-Copy (Read):** Readers access data directly from the page cache via mmap.

## Building and Running

This is a Rust workspace. Standard Cargo commands apply.

### Prerequisites
-   Rust (stable)
-   Linux or macOS (Darwin is supported for dev, but production targets Linux/eventfd).

### Common Commands

*   **Build Project:**
    ```bash
    cargo build --workspace
    ```

*   **Run All Tests:**
    ```bash
    cargo test --workspace
    ```
    *   *Note:* Integration tests are located in `crates/chronicle-core/tests/` and cover scenarios like recovery, fan-in, and retention.

*   **Run Benchmarks:**
    ```bash
    cargo bench -p chronicle-core
    ```

*   **Check Formatting & Linting:**
    ```bash
    cargo fmt --all -- --check
    cargo clippy --workspace -- -D warnings
    ```

## Project Structure

```text
chronicle-rs/
├── crates/
│   ├── chronicle-core/       # Data plane: Queue engine, storage format, append/read logic
│   └── chronicle-bus/        # Control plane: Directory layout, discovery, registration
├── docs/
│   ├── DESIGN.md             # Normative architectural specification (READ THIS FIRST)
│   └── ROADMAP.md            # Project milestones and status
├── execplans/                # Detailed implementation plans for each phase
└── tests/                    # Integration tests (if applicable outside crates)
```

## Development Conventions

1.  **Follow the Design:** The `docs/DESIGN.md` file is the source of truth. Any changes to the protocol, file format, or memory model must be reflected there.
2.  **ExecPlans:** Complex features or refactors should be planned in `execplans/` before implementation. See `PLANS.md` for the template.
3.  **Safety First:** This project uses `unsafe` for mmap and atomic operations. Comments must justify safety. Miri checks are encouraged where feasible.
4.  **Testing:**
    -   **Unit Tests:** Inside `src/` modules.
    -   **Integration Tests:** In `crates/chronicle-core/tests/`. Focus on resilience (recovery, disk full, concurrent rolling).
5.  **Code Style:** Standard Rust style. No unique formatting rules.

## Documentation

*   **[DESIGN.md](docs/DESIGN.md):** The comprehensive technical specification.
*   **[ROADMAP.md](docs/ROADMAP.md):** Current progress and future phases.
*   **[ExecPlans](execplans/):** History of implementation decisions.
