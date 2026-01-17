# Chronicle-RS

![Build Status](https://img.shields.io/badge/build-passing-brightgreen)
![Version](https://img.shields.io/badge/version-0.1.0-blue)
![License](https://img.shields.io/badge/license-MIT-green)
![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS-lightgrey)

**A low-latency, persisted messaging framework for high-frequency trading (HFT) systems in Rust.**

---

## 📖 Table of Contents

- [Overview](#overview)
- [Key Features](#key-features)
- [Architecture](#architecture)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Configuration](#configuration)
- [Development](#development)
- [Roadmap](#roadmap)
- [Contributing](#contributing)
- [License](#license)
- [Support](#support)

---

## Overview

Chronicle-RS is a specialized Inter-Process Communication (IPC) framework designed for single-host HFT deployments. It enables ultra-low latency communication between decoupled processes (Market Feeds, Strategies, Routers) using memory-mapped queues.

Unlike general-purpose message brokers (RabbitMQ, Kafka) or network-based IPC (gRPC, TCP), Chronicle-RS optimizes for **zero-copy reads**, **microsecond-level latency**, and **deterministic replay** by treating the filesystem as the shared memory bus.

### Why Chronicle-RS?
*   **Performance:** Avoiding syscalls on the hot path via `mmap` and atomic synchronization.
*   **Resilience:** Strategies can crash and restart without losing data or affecting the feed.
*   **Replayability:** Every message is persisted. Debugging a production crash is as simple as replaying the queue locally.

---

## Key Features

*   **🚀 Ultra-Low Latency:** Lock-free Single-Producer/Multi-Consumer (SPMC) queues using shared memory.
*   **💾 Durable Persistence:** Messages are stored in immutable segment files on disk (NVMe recommended).
*   **🔄 Zero-Copy Reads:** Readers access data directly from the OS page cache.
*   **📡 Broadcast & Fan-In:** Native support for "Market Data" (1-to-N) and "Order Routing" (N-to-1) topologies.
*   **🔍 Independent Readers:** Each consumer tracks its own progress; one slow reader never blocks the writer.

---

## Architecture

The system is split into two layers to ensure the critical data path remains lightweight:

1.  **Data Plane (`chronicle-core`):** The heavy lifter. Handles the binary queue format, atomic appending, segment rolling, and `futex`/spin waiting.
2.  **Control Plane (`chronicle-bus`):** The glue. Handles directory structures, process discovery, and RAII resource cleanup.

### Topology Example
Chronicle-RS enforces a **Multi-Process Architecture**:

```text
[Binance Feed] --(write)--> [market_data/queue/binance] --(read)--> [Strategy A]
                                                    --(read)--> [Strategy B]
                                                                    |
[Strategy A] --(write)--> [orders/queue/strategy_A/orders_out] --(read)--> [Router]
[Strategy B] --(write)--> [orders/queue/strategy_B/orders_out] --(read)--> [Router]
[Router] --(write)--> [orders/queue/strategy_*/orders_in] --(read)--> [Strategy *]
```

See [DESIGN.md](docs/DESIGN.md) for the complete architectural specification.

---

## Installation

### Prerequisites
*   **Rust:** Stable toolchain (`rustup install stable`).
*   **OS:** Linux (Production) or macOS (Development). Windows is not supported due to reliance on `eventfd`/`mmap` semantics.
*   **Disk:** NVMe SSD recommended for production latency.

### Add to Cargo.toml
*Currently, Chronicle-RS is a library framework to be included in your workspace.*

```toml
[dependencies]
chronicle-core = { path = "crates/chronicle-core" }
chronicle-bus = { path = "crates/chronicle-bus" }
```

---

## Quick Start

### 1. The Writer (Publisher)
Create a queue and write a message.

```rust
use chronicle_core::Queue;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open a queue for writing at ./data/my_queue
    let mut writer = Queue::open_publisher("./data/my_queue")?;

    // Write a simple byte payload (Type ID 1)
    let payload = b"Hello HFT World";
    writer.append(1, payload)?;
    
    // Ensure it's committed to listeners
    // (In strict mode, you might call flush_async() here)
    
    println!("Message written!");
    Ok(())
}
```

### 2. The Reader (Subscriber)
Read messages from the queue.

```rust
use chronicle_core::Queue;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open the same queue as a reader named "strategy_1"
    let mut reader = Queue::open_subscriber("./data/my_queue", "strategy_1")?;

    loop {
        // Blocks until a message is available (Hybrid Wait: Spin -> Futex)
        reader.wait(None)?; 

        if let Some(msg) = reader.next()? {
            println!("Received [Seq: {}]: {:?}", msg.seq, msg.payload);
            
            // Mark processed (persists offset to disk)
            reader.commit()?;
        }
    }
}
```

### 3. End-to-End Demo (Feed → Strategy → Router)
Run the demo processes in separate terminals:

```bash
# Terminal 1: market data feed
cargo run -p chronicle-bus --example feed -- --bus-root ./demo_bus

# Terminal 2: strategy filtering BTC and emitting orders
cargo run -p chronicle-bus --example strategy -- --bus-root ./demo_bus --strategy strategy_a --symbol BTC

# Terminal 3: router discovery + ack loop
cargo run -p chronicle-bus --example router -- --bus-root ./demo_bus
```

Expected log excerpts:
```
feed: published symbol=BTC price=...
strategy strategy_a: order order_id=0 symbol=BTC qty=1 side=BUY
router: discovered strategy_a (orders_out=..., orders_in=...)
router: order from strategy_a (order_id=0 symbol=BTC qty=1 side=BUY)
strategy strategy_a: ack order_id=0 status=ACK
```

---

## Configuration

Chronicle-RS uses convention over configuration for directory layouts.

### Bus Layout
Standard deployment structure (configurable root):
```text
/var/lib/hft_bus/
├── market_data/
│   └── queue/
│       └── binance/ ...
└── orders/
    └── queue/
        ├── strategy_a/
        │   ├── orders_out/ ...
        │   └── orders_in/ ...
        └── strategy_b/
            ├── orders_out/ ...
            └── orders_in/ ...
```

### Tuning Parameters
While most defaults are sane, you can tune `chronicle-core` constants if building from source:
*   `SPIN_US`: Microseconds to busy-loop before sleeping (default: `10us`).
*   `SEGMENT_SIZE`: Size of queue files (default: `128MB` or `1GB`).

---

## Development

### Setting Up
```bash
git clone https://github.com/your-org/chronicle-rs.git
cd chronicle-rs
cargo build --workspace
```

### Running Tests
The suite includes rigorous recovery and concurrency tests.
```bash
# Run standard unit tests
cargo test --workspace

# Run specialized integration tests (e.g., recovery)
cargo test --test reader_recovery
```

### Benchmarking
Measure the raw throughput of your filesystem/memory subsystem.
```bash
cargo bench -p chronicle-core
```

---

## Roadmap

*   [x] **Phase 1:** Core mmap engine & Single-Writer Protocol.
*   [x] **Phase 2:** Reader Persistence & Recovery.
*   [x] **Phase 3:** Discovery & Bus Logic.
*   [ ] **Phase 4:** **Tooling (chronicle-cli)** - Inspection and repair tools.
*   [ ] **Phase 5:** **Archival Sidecar** - Zstd compression to S3.
*   [ ] **Phase 6:** **Python Bindings** - For data analysis integration.

See [ROADMAP.md](docs/ROADMAP.md) for details.

---

## Contributing

We welcome contributions! Please follow these steps:
1.  Read [DESIGN.md](docs/DESIGN.md) carefully. We are strict about the architecture.
2.  Open an issue to discuss your proposed change.
3.  Submit a PR with tests covering your new functionality.

**Note:** This is an HFT framework. Performance regressions are treated as bugs.

---

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

---

## Support

*   **Issues:** Please report bugs via GitHub Issues.
*   **Discussion:** Join our [Discord Server](#) (Link Pending).
*   **Docs:** Read the [Design Doc](docs/DESIGN.md) for normative behavior.

---

*Built with 🦀 in Rust.*
