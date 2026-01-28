//! High-level IPC communication patterns built on Chronicle queues.
//!
//! This module provides reusable communication patterns (pub/sub, bidirectional, fan-in)
//! as thin, zero-cost abstractions over the low-level queue primitives in `crate::core`.
//!
//! # Design Philosophy
//!
//! - **Zero-Cost Abstractions**: All types are thin wrappers with no runtime overhead
//! - **Type Safety**: Communication patterns are encoded in types (Publisher vs Subscriber)
//! - **Intent-Revealing API**: Code clearly expresses the communication pattern being used
//! - **Hot Path Preservation**: No additional syscalls or allocations on the message path
//! - **Domain-Agnostic**: Pure infrastructure with no business logic
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │  Applications (feeds, strategies, routers)               │
//! ├──────────────────────────────────────────────────────────┤
//! │  Domain Layer (trading/, etc.)                           │
//! ├──────────────────────────────────────────────────────────┤
//! │  IPC Patterns (pub/sub, bidirectional, fan-in) ← YOU ARE HERE
//! ├──────────────────────────────────────────────────────────┤
//! │  Core Queue (segments, reader, writer)                   │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! # Patterns
//!
//! ## PubSub (Broadcast)
//!
//! One writer, many independent readers. Used for market data feeds.
//!
//! ```no_run
//! use chronicle::ipc::pubsub::{Publisher, Subscriber};
//!
//! // Feed process
//! let mut publisher = Publisher::open("./data/market/binance")?;
//! publisher.publish(b"market data")?;
//!
//! // Strategy process
//! let mut subscriber = Subscriber::open("./data/market/binance", "strategy_a")?;
//! while let Some(msg) = subscriber.recv()? {
//!     // Process message
//!     subscriber.commit()?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```
//!
//! ## BidirectionalChannel (SPSC Duplex)
//!
//! Paired queues for bidirectional communication.
//! Generic infrastructure - no domain knowledge.
//!
//! ```no_run
//! use chronicle::ipc::BidirectionalChannel;
//! use chronicle::core::{WriterConfig, ReaderConfig};
//!
//! // Process A
//! let mut channel = BidirectionalChannel::open(
//!     "./queue_a_to_b",  // A's outbox
//!     "./queue_b_to_a",  // A's inbox
//!     "reader_a",
//!     WriterConfig::default(),
//!     ReaderConfig::default(),
//! )?;
//!
//! // Send message
//! channel.send_outbound(0x01, b"hello")?;
//!
//! // Receive message
//! if let Some(msg) = channel.recv_inbound()? {
//!     channel.commit_inbound()?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```
//!
//! ## FanIn (Many-to-One Merge)
//!
//! Zero-copy merge of multiple queues, ordered by timestamp.
//!
//! ```no_run
//! use chronicle::ipc::fanin::FanInReader;
//! use chronicle::core::Queue;
//!
//! // Open multiple queues
//! let queue_a = Queue::open_subscriber("./queue_a", "reader")?;
//! let queue_b = Queue::open_subscriber("./queue_b", "reader")?;
//!
//! let mut fanin = FanInReader::new(vec![queue_a, queue_b])?;
//!
//! // Reads messages in timestamp order across all sources
//! while let Some(msg) = fanin.next()? {
//!     fanin.commit()?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```
//!
//! # Pattern Selection Guide
//!
//! | Use Case | Pattern | Writer | Reader | Ordering |
//! |----------|---------|--------|--------|----------|
//! | Broadcast feeds | PubSub | 1 | Many | Arrival |
//! | Duplex communication | BidirectionalChannel | 1 each way | 1 each way | N/A |
//! | Multi-source merge | FanIn | Many | 1 | Timestamp |
//!
//! # Performance Characteristics
//!
//! - **Latency**: Same as `core::Queue` (~200ns zero-syscall path, ~1-2µs wake path)
//! - **Memory**: Zero additional allocations on hot path
//! - **Throughput**: No degradation vs direct queue usage
//! - **Abstractions**: Compile-time only, zero runtime cost

pub mod bidirectional;
pub mod fanin;
pub mod pubsub;

// Re-export core types needed by IPC users
pub use crate::core::{
    BackpressurePolicy, DisconnectReason, MessageView, ReaderConfig, StartMode, WaitStrategy,
    WriterConfig, WriterStatus,
};

// Re-export pattern types
pub use bidirectional::BidirectionalChannel;
pub use fanin::FanInReader;
pub use pubsub::{Publisher, Subscriber};
