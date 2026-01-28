//! Generic bidirectional SPSC communication pattern.
//!
//! This module provides a clean API for paired queues enabling bidirectional
//! single-producer-single-consumer (SPSC) communication. Each endpoint has an
//! inbox (receiving queue) and outbox (sending queue).
//!
//! # Architecture
//!
//! ```text
//! Process A                       Process B
//! ┌───────────────────┐          ┌───────────────────┐
//! │  BidirectionalChannel        │  BidirectionalChannel
//! │  ┌─────────────┐  │          │  ┌─────────────┐  │
//! │  │   Outbox    │──┼─────────►│  │   Inbox     │  │
//! │  │  (Writer)   │  │          │  │  (Reader)   │  │
//! │  └─────────────┘  │          │  └─────────────┘  │
//! │                   │          │                   │
//! │  ┌─────────────┐  │          │  ┌─────────────┐  │
//! │  │   Inbox     │◄─┼──────────┼──│   Outbox    │  │
//! │  │  (Reader)   │  │          │  │  (Writer)   │  │
//! │  └─────────────┘  │          │  └─────────────┘  │
//! └───────────────────┘          └───────────────────┘
//! ```
//!
//! Each side has:
//! - **Outbox** (writer): Sends messages to the peer
//! - **Inbox** (reader): Receives messages from the peer
//!
//! # Example
//!
//! ```no_run
//! use chronicle::ipc::BidirectionalChannel;
//! use chronicle::core::{WriterConfig, ReaderConfig};
//!
//! // Process A
//! let mut channel_a = BidirectionalChannel::open(
//!     "./queue_a_to_b",  // A's outbox (B will read from this)
//!     "./queue_b_to_a",  // A's inbox (B will write to this)
//!     "reader_a",
//!     WriterConfig::default(),
//!     ReaderConfig::default(),
//! )?;
//!
//! // Send message to B
//! channel_a.send_outbound(0x01, b"Hello B")?;
//!
//! // Receive message from B
//! if let Some(msg) = channel_a.recv_inbound()? {
//!     println!("Received: {:?}", msg.payload);
//!     channel_a.commit_inbound()?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```

use std::path::Path;
use std::time::Duration;

use crate::core::{
    Clock, DisconnectReason, MessageView, Queue, QueueReader, QueueWriter, ReaderConfig,
    Result, SystemClock, WaitStrategy, WriterConfig, WriterStatus,
};

/// Generic bidirectional SPSC communication channel (inbox + outbox).
///
/// Provides send/receive semantics over two underlying queues. One side's outbox
/// is the other side's inbox, forming a bidirectional communication pair.
///
/// This is a pure infrastructure abstraction with no domain-specific knowledge.
pub struct BidirectionalChannel<C: Clock = SystemClock> {
    /// Receives messages from the peer (reader)
    inbox: QueueReader,
    /// Sends messages to the peer (writer)
    outbox: QueueWriter<C>,
}

impl BidirectionalChannel<SystemClock> {
    /// Opens a bidirectional channel from explicit queue paths.
    ///
    /// # Arguments
    ///
    /// * `outbox_path` - Path to the queue this endpoint writes to (peer reads from)
    /// * `inbox_path` - Path to the queue this endpoint reads from (peer writes to)
    /// * `reader_name` - Unique identifier for this reader
    /// * `writer_config` - Configuration for the outbox writer
    /// * `reader_config` - Configuration for the inbox reader
    ///
    /// # Errors
    ///
    /// - `Error::WriterAlreadyActive`: Another writer is active on the outbox queue
    /// - `Error::Io`: Failed to open or create queue directories
    ///
    /// # Example
    ///
    /// ```no_run
    /// use chronicle::ipc::BidirectionalChannel;
    /// use chronicle::core::{WriterConfig, ReaderConfig};
    ///
    /// let channel = BidirectionalChannel::open(
    ///     "./my_outbox",
    ///     "./my_inbox",
    ///     "my_reader",
    ///     WriterConfig::default(),
    ///     ReaderConfig::default(),
    /// )?;
    /// # Ok::<(), chronicle::core::Error>(())
    /// ```
    pub fn open(
        outbox_path: impl AsRef<Path>,
        inbox_path: impl AsRef<Path>,
        reader_name: impl AsRef<str>,
        writer_config: WriterConfig,
        reader_config: ReaderConfig,
    ) -> Result<Self> {
        let outbox = Queue::open_publisher_with_config(outbox_path, writer_config)?;
        let inbox = Queue::open_subscriber_with_config(inbox_path, reader_name.as_ref(), reader_config)?;

        Ok(Self { inbox, outbox })
    }

    /// Try to open a bidirectional channel, returning `None` if the inbox doesn't exist yet.
    ///
    /// Useful for discovery patterns where an endpoint polls for peer readiness.
    ///
    /// # Arguments
    ///
    /// Same as `open()`.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(channel))` if both queues opened successfully
    /// - `Ok(None)` if the inbox queue doesn't exist yet (peer not ready)
    /// - `Err(...)` on other errors
    pub fn try_open(
        outbox_path: impl AsRef<Path>,
        inbox_path: impl AsRef<Path>,
        reader_name: impl AsRef<str>,
        writer_config: WriterConfig,
        reader_config: ReaderConfig,
    ) -> Result<Option<Self>> {
        // Try to open inbox first (may not exist if peer hasn't started)
        let inbox = match Queue::try_open_subscriber_with_config(
            inbox_path,
            reader_name.as_ref(),
            reader_config,
        )? {
            Some(r) => r,
            None => return Ok(None),
        };

        // Open outbox (creates if needed)
        let outbox = Queue::open_publisher_with_config(outbox_path, writer_config)?;

        Ok(Some(Self { inbox, outbox }))
    }
}

impl<C: Clock> BidirectionalChannel<C> {
    /// Opens a bidirectional channel with a custom clock source.
    ///
    /// Most users should use `open()` instead, which uses the system clock.
    pub fn open_with_clock(
        outbox_path: impl AsRef<Path>,
        inbox_path: impl AsRef<Path>,
        reader_name: impl AsRef<str>,
        writer_config: WriterConfig,
        reader_config: ReaderConfig,
        clock: C,
    ) -> Result<Self> {
        let outbox = Queue::open_publisher_with_clock(outbox_path, writer_config, clock)?;
        let inbox = Queue::open_subscriber_with_config(inbox_path, reader_name.as_ref(), reader_config)?;

        Ok(Self { inbox, outbox })
    }

    // ========== Outbound (Send) Operations ==========

    /// Sends a message to the peer (via outbox).
    ///
    /// # Arguments
    ///
    /// * `type_id` - Application-defined message type
    /// * `payload` - Message bytes
    ///
    /// # Errors
    ///
    /// Same as `QueueWriter::append` (PayloadTooLarge, QueueFull, Io)
    pub fn send_outbound(&mut self, type_id: u16, payload: &[u8]) -> Result<()> {
        self.outbox.append(type_id, payload)
    }

    /// Flushes outbound writes asynchronously.
    ///
    /// Metadata is written, but data may not be durable yet.
    pub fn flush_outbound_async(&mut self) -> Result<()> {
        self.outbox.flush_async()
    }

    /// Flushes outbound writes synchronously (durable).
    ///
    /// Ensures all writes are persisted to disk before returning.
    pub fn flush_outbound_sync(&mut self) -> Result<()> {
        self.outbox.flush_sync()
    }

    // ========== Inbound (Receive) Operations ==========

    /// Receives a message from the peer (from inbox, zero-copy).
    ///
    /// Returns `Ok(None)` if no new messages are available.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use chronicle::ipc::BidirectionalChannel;
    /// # use chronicle::core::{WriterConfig, ReaderConfig};
    /// # let mut channel = BidirectionalChannel::open("./a", "./b", "r", WriterConfig::default(), ReaderConfig::default())?;
    /// while let Some(msg) = channel.recv_inbound()? {
    ///     println!("Type: {}, Payload: {:?}", msg.type_id, msg.payload);
    ///     channel.commit_inbound()?;
    /// }
    /// # Ok::<(), chronicle::core::Error>(())
    /// ```
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
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait (None = indefinite)
    pub fn wait_inbound(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.inbox.wait(timeout)
    }

    /// Sets the inbound wait strategy.
    ///
    /// Controls how `wait_inbound()` behaves (blocking, spinning, yielding).
    pub fn set_inbound_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.inbox.set_wait_strategy(strategy)
    }

    /// Checks peer liveness (inbound writer status).
    ///
    /// # Arguments
    ///
    /// * `ttl` - Time-to-live duration. Peer is considered stale if heartbeat is older.
    ///
    /// # Returns
    ///
    /// Writer status indicating if peer is alive and writing.
    pub fn inbound_writer_status(&self, ttl: Duration) -> Result<WriterStatus> {
        self.inbox.writer_status(ttl)
    }

    /// Detects peer disconnection (inbound).
    ///
    /// # Arguments
    ///
    /// * `ttl` - Time-to-live duration. Peer is considered stale if heartbeat is older.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(reason))` if peer is disconnected
    /// - `Ok(None)` if peer is still connected
    pub fn detect_inbound_disconnect(&self, ttl: Duration) -> Result<Option<DisconnectReason>> {
        self.inbox.detect_disconnect(ttl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_bidirectional_channel_basic() {
        let dir = TempDir::new().unwrap();

        let outbox_a = dir.path().join("a_to_b");
        let inbox_a = dir.path().join("b_to_a");
        let outbox_b = dir.path().join("b_to_a");
        let inbox_b = dir.path().join("a_to_b");

        // Pre-create both queues so both sides can open
        Queue::open_publisher(&outbox_a).unwrap();
        Queue::open_publisher(&outbox_b).unwrap();

        // Open both sides
        let mut channel_a = BidirectionalChannel::open(
            &outbox_a,
            &inbox_a,
            "reader_a",
            WriterConfig::default(),
            ReaderConfig::default(),
        )
        .unwrap();

        let mut channel_b = BidirectionalChannel::open(
            &outbox_b,
            &inbox_b,
            "reader_b",
            WriterConfig::default(),
            ReaderConfig::default(),
        )
        .unwrap();

        // A sends to B
        channel_a.send_outbound(0x01, b"Hello B").unwrap();

        // B receives from A
        let msg = channel_b.recv_inbound().unwrap().unwrap();
        assert_eq!(msg.type_id, 0x01);
        assert_eq!(msg.payload, b"Hello B");
        channel_b.commit_inbound().unwrap();

        // B sends to A
        channel_b.send_outbound(0x02, b"Hello A").unwrap();

        // A receives from B
        let msg = channel_a.recv_inbound().unwrap().unwrap();
        assert_eq!(msg.type_id, 0x02);
        assert_eq!(msg.payload, b"Hello A");
        channel_a.commit_inbound().unwrap();
    }

    #[test]
    fn test_try_open_not_ready() {
        let dir = TempDir::new().unwrap();

        let outbox = dir.path().join("outbox");
        let inbox = dir.path().join("inbox");

        // Try to open when inbox doesn't exist
        let result = BidirectionalChannel::try_open(
            &outbox,
            &inbox,
            "reader",
            WriterConfig::default(),
            ReaderConfig::default(),
        )
        .unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_try_open_ready() {
        let dir = TempDir::new().unwrap();

        let outbox_a = dir.path().join("a_to_b");
        let inbox_a = dir.path().join("b_to_a");

        // Create inbox first (simulate peer being ready)
        Queue::open_publisher(&inbox_a).unwrap();

        // Now try_open should succeed
        let result = BidirectionalChannel::try_open(
            &outbox_a,
            &inbox_a,
            "reader_a",
            WriterConfig::default(),
            ReaderConfig::default(),
        )
        .unwrap();

        assert!(result.is_some());
    }
}
