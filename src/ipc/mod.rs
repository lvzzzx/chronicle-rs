//! High-level IPC communication patterns built on Chronicle queues.
//!
//! This module provides reusable communication patterns (pub/sub, inbox/outbox, fan-in)
//! as thin, zero-cost abstractions over the low-level queue primitives in `crate::core`.
//!
//! # Design Philosophy
//!
//! - **Zero-Cost Abstractions**: All types are thin wrappers with no runtime overhead
//! - **Type Safety**: Communication patterns are encoded in types (Publisher vs Subscriber)
//! - **Intent-Revealing API**: Code clearly expresses the communication pattern being used
//! - **Hot Path Preservation**: No additional syscalls or allocations on the message path
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │  Applications (feeds, strategies, routers)               │
//! ├──────────────────────────────────────────────────────────┤
//! │  IPC Patterns (pub/sub, inbox/outbox, fan-in) ← YOU ARE HERE
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
//! ## InboxOutbox (Bidirectional SPSC)
//!
//! Paired queues for request/response or bidirectional communication.
//! Used for strategy ↔ router communication.
//!
//! ```no_run
//! use chronicle::ipc::inbox_outbox::InboxOutbox;
//! use chronicle::layout::IpcLayout;
//!
//! let layout = IpcLayout::new("/var/lib/hft_bus");
//!
//! // Strategy side
//! let mut channel = InboxOutbox::open_strategy(&layout, "strategy_a")?;
//! channel.send_outbound(b"new order")?;  // orders_out
//! if let Some(msg) = channel.recv_inbound()? {  // orders_in
//!     // Process fill/ack
//!     channel.commit_inbound()?;
//! }
//!
//! // Router side
//! let mut router_channel = InboxOutbox::open_router(&layout, "strategy_a")?;
//! if let Some(order) = router_channel.recv_inbound()? {  // reads orders_out
//!     // Process order
//!     router_channel.send_outbound(b"fill")?;  // writes orders_in
//!     router_channel.commit_inbound()?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```
//!
//! ## FanIn (Many-to-One Merge)
//!
//! Zero-copy merge of multiple queues, ordered by timestamp.
//! Used by router to aggregate orders from multiple strategies.
//!
//! ```no_run
//! use chronicle::ipc::fanin::FanInReader;
//! use chronicle::core::Queue;
//!
//! // Open multiple strategy order queues
//! let strategy_a = Queue::open_subscriber("./orders/strategy_a/orders_out", "router")?;
//! let strategy_b = Queue::open_subscriber("./orders/strategy_b/orders_out", "router")?;
//!
//! let mut fanin = FanInReader::new(vec![strategy_a, strategy_b])?;
//!
//! // Reads messages in timestamp order across all sources
//! while let Some(msg) = fanin.next()? {
//!     // msg is the earliest message across all queues
//!     fanin.commit()?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```
//!
//! # Pattern Selection Guide
//!
//! | Use Case | Pattern | Writer | Reader | Ordering |
//! |----------|---------|--------|--------|----------|
//! | Market data broadcast | PubSub | 1 | Many | Arrival |
//! | Strategy ↔ Router | InboxOutbox | 1 each way | 1 each way | N/A |
//! | Router fan-in | FanIn | Many | 1 | Timestamp |
//! | Control/RPC | InboxOutbox | 1 each way | 1 each way | N/A |
//!
//! # Performance Characteristics
//!
//! - **Latency**: Same as `core::Queue` (~200ns zero-syscall path, ~1-2µs wake path)
//! - **Memory**: Zero additional allocations on hot path
//! - **Throughput**: No degradation vs direct queue usage
//! - **Abstractions**: Compile-time only, zero runtime cost

pub mod bidirectional;
pub mod fanin;
pub mod inbox_outbox;
pub mod pubsub;

// Re-export core types needed by IPC users
pub use crate::core::{
    BackpressurePolicy, DisconnectReason, MessageView, ReaderConfig, StartMode, WaitStrategy,
    WriterConfig, WriterStatus,
};

// Re-export pattern types
pub use bidirectional::BidirectionalChannel;
pub use fanin::FanInReader;
pub use inbox_outbox::InboxOutbox;
pub use pubsub::{Publisher, Subscriber};
