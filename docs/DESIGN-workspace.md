# Chronicle-RS Workspace Architecture (v2.0 Proposal)

**Status:** Draft / Proposal
**Date:** 2026-01-23

## 1. Objective

To transition Chronicle-RS from a collection of low-level primitives into a cohesive **High-Frequency Trading Operating System**. The "OS" framing is the north star: stable kernel primitives + a managed runtime + a standard strategy API. This architecture aims to standardize the development lifecycle, ensuring that a strategy written once can be deployed in a low-latency live environment or a high-fidelity simulation environment without code changes.

## 2. Layered Architecture

The workspace is organized into four logical layers, separating stable primitives from dynamic application logic. This diagram reflects the physical layout under `crates/`.

```text
chronicle-rs/
└── crates/
    ├── 1-primitives/           # Foundations (Stable, unsafe, zero-allocation)
    │   ├── chronicle-core      # (Existing) The Queue Engine (mmap, append, read)
    │   └── chronicle-protocol  # (Existing) Wire format, SBE structs, TypeIds
    │
    ├── 2-infra/                # System Plumbing (Topology & Storage)
    │   ├── chronicle-bus       # (Existing) IPC Topology, Discovery, Shm layout
    │   └── chronicle-storage   # (Refactor) Tiered storage access (Live vs Archive)
    │
    ├── 3-engine/               # Logic Processors (The "Brains")
    │   ├── chronicle-replay    # (Existing) Event reconstruction, Ordering, Snapshots
    │   └── chronicle-sim       # (NEW) Matching Engine Simulator, Latency models, PnL accounting
    │
    └── 4-app/                  # Application SDKs (The "User Interface")
        ├── chronicle-api       # (NEW) Shared Traits (Context, Strategy) defining the contract
        ├── chronicle-framework # (NEW) Live Runtime (Pinning, Signals, Busy-Loops)
        ├── chronicle-cli       # (Existing) CLI tooling and diagnostics
        ├── chronicle-feed-binance # (Existing) Exchange feed adapter
        └── chronicle-etl       # (Existing) Batch processing & feature extraction
```

### 2.1 Dependency Rules

- A layer may depend only on lower-numbered layers (e.g., `4-app` can depend on `1-primitives`..`3-engine`).
- `1-primitives` must never depend on higher layers or optional features.
- Binaries live in their owning crate; CLI-only entrypoints should not be used as shared libraries.

---

## 3. Crate Breakdown

### Layer 1: Primitives (The "Kernel")

*   **`chronicle-core`**
    *   **Role:** Single-writer, multi-reader, persistent ring-buffer over mmap.
    *   **Key Features:** CAS-based commits, hybrid wait strategies, retention policies.
    *   **Mandate:** Zero syscalls on the hot path.

*   **`chronicle-protocol`**
    *   **Role:** The "ABI" of the system. Defines message schemas (SBE/FlatBuffers) and `TypeId` enums.
    *   **Mandate:** Backward compatibility.

### Layer 2: Infrastructure (The "Plumbing")

*   **`chronicle-bus`**
    *   **Role:** Defines how queues are named and discovered on disk (e.g., `/dev/shm/queue/strategy/alpha_1/orders`).
    *   **Key Features:** Router discovery, channel negotiation.

*   **`chronicle-storage`** (Rename of `chronicle-archive`)
    *   **Role:** Abstraction over long-term data access.
    *   **Key Features:**
        *   **`ArchiveTap`:** Binary to bridge Live Queues -> Archive.
        *   **`Import`:** Tooling to ingest vendor CSV/PCAP into Archive.
        *   **Tiering:** Policies for moving data from NVMe (Hot) to HDD/S3 (Cold).

### Layer 3: Engine (The "Simulation")

*   **`chronicle-replay`**
    *   **Role:** The iterator that reconstructs the "World State" (L2/L3 Books) from a log stream.
    *   **Key Features:** Deterministic ordering, warm-start from snapshots.

*   **`chronicle-sim`** (NEW)
    *   **Role:** A simulated exchange environment for backtesting.
    *   **Key Features:**
        *   **`MockExchange`:** Matches strategy orders against historical book updates.
        *   **`LatencyModel`:** Simulates wire delay and internal processing time.
        *   **`Accountant`:** Tracks PnL, fees, and margin.

### Layer 4: Application (The "SDK")

*   **`chronicle-api`** (NEW)
    *   **Role:** The interface layer that decouples Strategy Logic from the Runtime.
    *   **Key Concepts:**
        *   **`Context` Trait:** Abstract interface for `send_order`, `now()`, `log()`.
        *   **`Strategy` Trait:** Abstract interface for `on_book_update`, `on_fill`.

*   **`chronicle-framework`** (NEW)
    *   **Role:** The opinionated "Container" for running strategies in production.
    *   **Responsibilities:**
        *   **Thread Pinning:** Isolating the process to a specific core.
        *   **Lifecycle:** Initialization, Heartbeats, Signal Handling (SIGTERM).
        *   **Wait Strategy:** Busy-spin with eventfd wakeups; futex is a future optimization.

*   **`chronicle-etl`**
    *   **Role:** Batch processing framework for generating training data (Parquet/Arrow).

*   **`chronicle-cli`**
    *   **Role:** Operational CLI for inspecting queues and diagnosing bus health.

*   **`chronicle-feed-binance`**
    *   **Role:** Live market data ingestion adapter for Binance feeds.

---

## 4. The "Write Once, Run Anywhere" Pattern

Strategies are implemented against the `chronicle-api`, making them agnostic to whether they are trading real money or running a simulation.

### 4.1 The Shared API (`chronicle-api`)

```rust
// Defined in chronicle-api
pub trait Context {
    /// Returns the current time (SystemTime in Live, EventTime in Sim)
    fn now(&self) -> u64;
    
    /// Sends an order (To Bus in Live, To MockExchange in Sim)
    fn send_order(&mut self, order: OrderRequest) -> Result<(), OrderError>;
    
    /// Returns the position for a given instrument
    fn position(&self, symbol: &str) -> i64;
}

pub trait Strategy {
    fn on_book_update(&mut self, ctx: &mut dyn Context, book: &L2Book);
    fn on_trade(&mut self, ctx: &mut dyn Context, trade: &Trade);
}
```

### 4.2 Implementation Example

**The User's Code:**
```rust
struct AlphaStrategy;

impl Strategy for AlphaStrategy {
    fn on_book_update(&mut self, ctx: &mut dyn Context, book: &L2Book) {
        if book.spread() > 5.0 {
            // "ctx" handles the complexity of where this order goes
            ctx.send_order(OrderRequest::limit(Side::Buy, 100, book.bid_price()));
        }
    }
}
```

### 4.3 Runtime 1: Production (`chronicle-framework`)

```rust
fn main() {
    // Configures core pinning, queue paths, and bus connection
    ChronicleApp::new()
        .app_name("alpha_v1")
        .pin_core(4)
        .subscribe("binance_btcusdt")
        .run(AlphaStrategy::new());
}
```

### 4.4 Runtime 2: Backtest (`chronicle-sim`)

```rust
fn main() {
    // Configures historical data source, latency simulation, and starting capital
    BacktestRunner::new()
        .data_source("/archive/binance/2025/01")
        .latency_model(ConstantLatency::micros(50))
        .initial_cash(100_000.0)
        .run(AlphaStrategy::new());
}
```

## 5. Benefits

1.  **Safety:** Strategy code is isolated behind a small API surface; blocking operations and allocations are discouraged and can be enforced with linting/feature flags if needed.
2.  **Velocity:** Infrastructure boilerplate (bus connection, pinning) is solved once in the Framework, not re-implemented per strategy.
3.  **Correctness:** Simulation uses the exact same logic as production, eliminating "implementation drift" where the backtest code differs from the live code.
