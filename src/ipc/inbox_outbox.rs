//! Inbox/Outbox bidirectional SPSC communication pattern.
//!
//! This module provides a clean API for paired queues enabling bidirectional
//! single-producer-single-consumer (SPSC) communication. Commonly used for
//! strategy ↔ router communication where each side sends and receives.
//!
//! # Architecture
//!
//! ```text
//! Strategy Process                Router Process
//! ┌───────────────────┐          ┌───────────────────┐
//! │  InboxOutbox      │          │  InboxOutbox      │
//! │  ┌─────────────┐  │          │  ┌─────────────┐  │
//! │  │   Outbox    │──┼─────────►│  │   Inbox     │  │
//! │  │  (orders_out│  │          │  │ (orders_out)│  │
//! │  │   Writer)   │  │          │  │  Reader)    │  │
//! │  └─────────────┘  │          │  └─────────────┘  │
//! │                   │          │                   │
//! │  ┌─────────────┐  │          │  ┌─────────────┐  │
//! │  │   Inbox     │◄─┼──────────┼──│   Outbox    │  │
//! │  │ (orders_in  │  │          │  │  (orders_in │  │
//! │  │   Reader)   │  │          │  │   Writer)   │  │
//! │  └─────────────┘  │          │  └─────────────┘  │
//! └───────────────────┘          └───────────────────┘
//! ```
//!
//! Each side has:
//! - **Outbox** (writer): Sends messages to the peer
//! - **Inbox** (reader): Receives messages from the peer
//!
//! # Directory Layout
//!
//! ```text
//! orders/queue/
//! └── strategy_A/
//!     ├── orders_out/     ← Strategy writes, Router reads
//!     │   ├── control.meta
//!     │   ├── 000000000.q
//!     │   └── readers/
//!     │       └── router.meta
//!     └── orders_in/      ← Router writes, Strategy reads
//!         ├── control.meta
//!         ├── 000000000.q
//!         └── readers/
//!             └── strategy_A.meta
//! ```
//!
//! # Example: Strategy Side
//!
//! ```no_run
//! use chronicle::ipc::inbox_outbox::InboxOutbox;
//! use chronicle::layout::IpcLayout;
//!
//! let layout = IpcLayout::new("/var/lib/hft_bus");
//! let mut channel = InboxOutbox::open_strategy(&layout, "strategy_momentum")?;
//!
//! // Send order to router
//! channel.send_outbound(0x01, b"new order BTC")?;
//!
//! // Wait for fill/ack from router
//! channel.wait_inbound(None)?;
//! if let Some(msg) = channel.recv_inbound()? {
//!     println!("Received fill: {:?}", msg.payload);
//!     channel.commit_inbound()?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```
//!
//! # Example: Router Side
//!
//! ```no_run
//! use chronicle::ipc::inbox_outbox::InboxOutbox;
//! use chronicle::layout::IpcLayout;
//!
//! let layout = IpcLayout::new("/var/lib/hft_bus");
//! let mut channel = InboxOutbox::open_router(&layout, "strategy_momentum")?;
//!
//! // Receive order from strategy
//! if let Some(order) = channel.recv_inbound()? {
//!     println!("Received order: {:?}", order.payload);
//!
//!     // Send fill back to strategy
//!     channel.send_outbound(0x02, b"filled")?;
//!     channel.commit_inbound()?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```

use std::path::Path;
use std::time::Duration;

use crate::core::{
    Clock, DisconnectReason, Error, MessageView, Queue, QueueReader, QueueWriter, ReaderConfig,
    Result, SystemClock, WaitStrategy, WriterConfig, WriterStatus,
};
use crate::layout::{IpcLayout, StrategyId};

/// Bidirectional SPSC communication channel (inbox + outbox).
///
/// Provides send/receive semantics over two underlying queues. One side's outbox
/// is the other side's inbox, forming a bidirectional communication pair.
///
/// # Type Safety
///
/// The `InboxOutbox` type encodes the paired queue semantics. Each instance knows
/// whether it's the strategy or router side, ensuring proper reader/writer roles.
pub struct InboxOutbox<C: Clock = SystemClock> {
    /// Receives messages from the peer (reader)
    inbox: QueueReader,
    /// Sends messages to the peer (writer)
    outbox: QueueWriter<C>,
    /// Role identifier (for debugging/logging)
    role: Role,
    /// Strategy ID (for debugging/logging)
    strategy_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Strategy,
    Router,
}

impl InboxOutbox<SystemClock> {
    /// Opens a strategy-side channel with default configuration.
    ///
    /// Creates the strategy directory and both queue subdirectories if they don't exist.
    ///
    /// # Layout
    ///
    /// - Outbox: `orders/queue/{strategy_id}/orders_out` (strategy writes)
    /// - Inbox: `orders/queue/{strategy_id}/orders_in` (strategy reads)
    ///
    /// # Errors
    ///
    /// - `Error::WriterAlreadyActive`: Another strategy process is already running
    /// - `Error::InvalidInput`: Invalid strategy ID (empty or contains `/`)
    pub fn open_strategy(layout: &IpcLayout, strategy_id: &str) -> Result<Self> {
        Self::open_strategy_with_config(
            layout,
            strategy_id,
            WriterConfig::default(),
            ReaderConfig::default(),
        )
    }

    /// Opens a strategy-side channel with custom configuration.
    pub fn open_strategy_with_config(
        layout: &IpcLayout,
        strategy_id: &str,
        writer_config: WriterConfig,
        reader_config: ReaderConfig,
    ) -> Result<Self> {
        let orders = layout.orders();
        let endpoints = match orders.strategy_endpoints(&StrategyId(strategy_id.to_string())) {
            Ok(e) => e,
            Err(_) => return Err(Error::Unsupported("invalid strategy_id")),
        };

        // Strategy writes to orders_out, reads from orders_in
        let outbox = Queue::open_publisher_with_config(&endpoints.orders_out, writer_config)?;
        let inbox =
            Queue::open_subscriber_with_config(&endpoints.orders_in, strategy_id, reader_config)?;

        Ok(Self {
            inbox,
            outbox,
            role: Role::Strategy,
            strategy_id: strategy_id.to_string(),
        })
    }

    /// Opens a router-side channel with default configuration.
    ///
    /// # Layout
    ///
    /// - Inbox: `orders/queue/{strategy_id}/orders_out` (router reads)
    /// - Outbox: `orders/queue/{strategy_id}/orders_in` (router writes)
    ///
    /// # Errors
    ///
    /// - `Error::WriterAlreadyActive`: Another router is already writing to this strategy's inbox
    /// - `Error::NotFound`: Strategy hasn't created the orders_out queue yet
    pub fn open_router(layout: &IpcLayout, strategy_id: &str) -> Result<Self> {
        Self::open_router_with_config(
            layout,
            strategy_id,
            WriterConfig::default(),
            ReaderConfig::default(),
        )
    }

    /// Opens a router-side channel with custom configuration.
    pub fn open_router_with_config(
        layout: &IpcLayout,
        strategy_id: &str,
        writer_config: WriterConfig,
        reader_config: ReaderConfig,
    ) -> Result<Self> {
        let orders = layout.orders();
        let endpoints = match orders.strategy_endpoints(&StrategyId(strategy_id.to_string())) {
            Ok(e) => e,
            Err(_) => return Err(Error::Unsupported("invalid strategy_id")),
        };

        // Router reads from orders_out, writes to orders_in
        let inbox =
            Queue::open_subscriber_with_config(&endpoints.orders_out, "router", reader_config)?;
        let outbox = Queue::open_publisher_with_config(&endpoints.orders_in, writer_config)?;

        Ok(Self {
            inbox,
            outbox,
            role: Role::Router,
            strategy_id: strategy_id.to_string(),
        })
    }

    /// Tries to open a router-side channel, returning `Ok(None)` if not ready.
    ///
    /// Useful for discovery patterns where the router polls for new strategies.
    pub fn try_open_router(layout: &IpcLayout, strategy_id: &str) -> Result<Option<Self>> {
        Self::try_open_router_with_config(
            layout,
            strategy_id,
            WriterConfig::default(),
            ReaderConfig::default(),
        )
    }

    /// Tries to open a router-side channel with custom configuration.
    pub fn try_open_router_with_config(
        layout: &IpcLayout,
        strategy_id: &str,
        writer_config: WriterConfig,
        reader_config: ReaderConfig,
    ) -> Result<Option<Self>> {
        let orders = layout.orders();
        let endpoints = match orders.strategy_endpoints(&StrategyId(strategy_id.to_string())) {
            Ok(e) => e,
            Err(_) => return Err(Error::Unsupported("invalid strategy_id")),
        };

        // Try to open inbox (orders_out); if it doesn't exist, strategy isn't ready
        let inbox = match Queue::try_open_subscriber_with_config(
            &endpoints.orders_out,
            "router",
            reader_config,
        )? {
            Some(r) => r,
            None => return Ok(None),
        };

        // Create outbox (orders_in)
        let outbox = Queue::open_publisher_with_config(&endpoints.orders_in, writer_config)?;

        Ok(Some(Self {
            inbox,
            outbox,
            role: Role::Router,
            strategy_id: strategy_id.to_string(),
        }))
    }

    /// Opens from explicit paths (advanced usage).
    ///
    /// Most users should use `open_strategy` or `open_router` instead.
    pub fn open_with_paths(
        inbox_path: impl AsRef<Path>,
        outbox_path: impl AsRef<Path>,
        reader_name: &str,
        writer_config: WriterConfig,
        reader_config: ReaderConfig,
    ) -> Result<Self> {
        let outbox = Queue::open_publisher_with_config(outbox_path, writer_config)?;
        let inbox = Queue::open_subscriber_with_config(inbox_path, reader_name, reader_config)?;

        Ok(Self {
            inbox,
            outbox,
            role: Role::Strategy, // Default assumption
            strategy_id: reader_name.to_string(),
        })
    }
}

impl<C: Clock> InboxOutbox<C> {
    /// Opens a channel with a custom clock source.
    pub fn open_strategy_with_clock(
        layout: &IpcLayout,
        strategy_id: &str,
        writer_config: WriterConfig,
        reader_config: ReaderConfig,
        clock: C,
    ) -> Result<Self> {
        let orders = layout.orders();
        let endpoints = match orders.strategy_endpoints(&StrategyId(strategy_id.to_string())) {
            Ok(e) => e,
            Err(_) => return Err(Error::Unsupported("invalid strategy_id")),
        };

        let outbox =
            Queue::open_publisher_with_clock(&endpoints.orders_out, writer_config, clock)?;
        let inbox =
            Queue::open_subscriber_with_config(&endpoints.orders_in, strategy_id, reader_config)?;

        Ok(Self {
            inbox,
            outbox,
            role: Role::Strategy,
            strategy_id: strategy_id.to_string(),
        })
    }

    // ========== Outbound (Send) Operations ==========

    /// Sends a message to the peer (via outbox).
    ///
    /// # Arguments
    ///
    /// - `type_id`: Application-defined message type
    /// - `payload`: Message bytes
    ///
    /// # Errors
    ///
    /// Same as `QueueWriter::append` (PayloadTooLarge, QueueFull, Io)
    pub fn send_outbound(&mut self, type_id: u16, payload: &[u8]) -> Result<()> {
        self.outbox.append(type_id, payload)
    }

    /// Flushes outbound writes asynchronously.
    pub fn flush_outbound_async(&mut self) -> Result<()> {
        self.outbox.flush_async()
    }

    /// Flushes outbound writes synchronously (durable).
    pub fn flush_outbound_sync(&mut self) -> Result<()> {
        self.outbox.flush_sync()
    }

    // ========== Inbound (Receive) Operations ==========

    /// Receives a message from the peer (from inbox, zero-copy).
    ///
    /// Returns `Ok(None)` if no new messages are available.
    pub fn recv_inbound(&mut self) -> Result<Option<MessageView<'_>>> {
        self.inbox.next()
    }

    /// Commits the current inbound read position.
    ///
    /// Should be called periodically to persist progress and enable retention.
    pub fn commit_inbound(&mut self) -> Result<()> {
        self.inbox.commit()
    }

    /// Blocks until a new inbound message arrives or timeout expires.
    pub fn wait_inbound(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.inbox.wait(timeout)
    }

    /// Sets the inbound wait strategy.
    pub fn set_inbound_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.inbox.set_wait_strategy(strategy)
    }

    /// Checks peer liveness (inbound writer status).
    ///
    /// # Arguments
    ///
    /// - `ttl`: Time-to-live duration. Peer is considered stale if heartbeat is older.
    pub fn inbound_writer_status(&self, ttl: Duration) -> Result<WriterStatus> {
        self.inbox.writer_status(ttl)
    }

    /// Detects peer disconnection (inbound).
    ///
    /// # Arguments
    ///
    /// - `ttl`: Time-to-live duration. Peer is considered stale if heartbeat is older.
    pub fn detect_inbound_disconnect(&self, ttl: Duration) -> Result<Option<DisconnectReason>> {
        self.inbox.detect_disconnect(ttl)
    }

    // ========== Metadata ==========

    /// Returns the strategy ID associated with this channel.
    pub fn strategy_id(&self) -> &str {
        &self.strategy_id
    }

    /// Returns the role (Strategy or Router) of this channel.
    pub fn is_strategy_side(&self) -> bool {
        self.role == Role::Strategy
    }

    /// Returns the role (Strategy or Router) of this channel.
    pub fn is_router_side(&self) -> bool {
        self.role == Role::Router
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_inbox_outbox_basic() {
        let dir = TempDir::new().unwrap();
        let layout = IpcLayout::new(dir.path());

        // Strategy creates channel
        let mut strategy = InboxOutbox::open_strategy(&layout, "test_strat").unwrap();
        assert!(strategy.is_strategy_side());

        // Router opens channel
        let mut router = InboxOutbox::open_router(&layout, "test_strat").unwrap();
        assert!(router.is_router_side());

        // Strategy sends order
        strategy.send_outbound(0x01, b"buy BTC").unwrap();

        // Router receives order
        let order = router.recv_inbound().unwrap().unwrap();
        assert_eq!(order.payload, b"buy BTC");
        router.commit_inbound().unwrap();

        // Router sends fill
        router.send_outbound(0x02, b"filled").unwrap();

        // Strategy receives fill
        let fill = strategy.recv_inbound().unwrap().unwrap();
        assert_eq!(fill.payload, b"filled");
        strategy.commit_inbound().unwrap();
    }

    #[test]
    fn test_try_open_router_not_ready() {
        let dir = TempDir::new().unwrap();
        let layout = IpcLayout::new(dir.path());

        // Router tries to open before strategy exists
        let result = InboxOutbox::try_open_router(&layout, "nonexistent");
        assert!(result.unwrap().is_none());
    }
}
