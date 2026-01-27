//! Publish-Subscribe (broadcast) pattern.
//!
//! This module provides a clean API for the single-writer, multiple-independent-readers
//! (SPMC broadcast) pattern commonly used for market data distribution.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────┐
//! │  Publisher   │
//! │  (1 Writer)  │
//! └──────┬───────┘
//!        │ writes to queue
//!        ▼
//! ┌──────────────────────┐
//!  │   Queue Directory    │
//!  │  (mmap'd segments)   │
//!  └──┬────────────┬──────┘
//!     │            │
//!     ▼            ▼
//! ┌─────────┐  ┌─────────┐
//! │Subscriber│  │Subscriber│
//! │(Reader 1)│  │(Reader 2)│
//! └─────────┘  └─────────┘
//! ```
//!
//! Each subscriber maintains independent read offsets and can consume at their own pace.
//!
//! # Example
//!
//! ```no_run
//! use chronicle::ipc::pubsub::{Publisher, Subscriber};
//!
//! // Feed process (publisher)
//! let mut feed = Publisher::open("./data/market/binance_spot")?;
//! loop {
//!     let market_data = fetch_market_data();
//!     feed.publish(&market_data)?;
//! }
//!
//! // Strategy process (subscriber)
//! let mut strategy = Subscriber::open("./data/market/binance_spot", "strategy_momentum")?;
//! strategy.set_wait_strategy(chronicle::core::WaitStrategy::SpinThenPark { spin_us: 10 });
//!
//! loop {
//!     while let Some(msg) = strategy.recv()? {
//!         process_market_data(&msg);
//!         strategy.commit()?;
//!     }
//!     strategy.wait(None)?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```

use std::path::Path;
use std::time::Duration;

use crate::core::{
    Clock, DisconnectReason, MessageView, Queue, QueueReader, QueueWriter, ReaderConfig,
    Result, SystemClock, WaitStrategy, WriterConfig, WriterStatus,
};

/// A publisher (writer) for broadcast messaging.
///
/// Wraps `QueueWriter` with a pub/sub-oriented API. Only one publisher may be active
/// per queue directory (enforced by writer.lock).
///
/// # Type Safety
///
/// The `Publisher` type encodes the intent: this is a broadcast writer. Downstream
/// code can rely on the single-writer guarantee and independent reader semantics.
pub struct Publisher<C: Clock = SystemClock> {
    writer: QueueWriter<C>,
}

impl Publisher<SystemClock> {
    /// Opens a publisher with default configuration.
    ///
    /// Creates the queue directory if it doesn't exist. Acquires an exclusive writer lock.
    ///
    /// # Errors
    ///
    /// Returns `Error::WriterAlreadyActive` if another live writer holds the lock.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_config(path, WriterConfig::default())
    }

    /// Opens a publisher with custom configuration.
    pub fn open_with_config(path: impl AsRef<Path>, config: WriterConfig) -> Result<Self> {
        let writer = Queue::open_publisher_with_config(path, config)?;
        Ok(Self { writer })
    }
}

impl<C: Clock> Publisher<C> {
    /// Opens a publisher with a custom clock source (e.g., TSC-based).
    pub fn open_with_clock(path: impl AsRef<Path>, config: WriterConfig, clock: C) -> Result<Self> {
        let writer = Queue::open_publisher_with_clock(path, config, clock)?;
        Ok(Self { writer })
    }

    /// Publishes a message to all subscribers.
    ///
    /// The message is appended to the queue with a monotonic sequence number and
    /// timestamp. All subscribers will observe the same sequence and timestamp.
    ///
    /// # Arguments
    ///
    /// - `type_id`: Application-defined message type (see `crate::protocol`)
    /// - `payload`: Message bytes (max `MAX_PAYLOAD_LEN`)
    ///
    /// # Errors
    ///
    /// - `Error::PayloadTooLarge`: Payload exceeds maximum size
    /// - `Error::QueueFull`: Configured capacity limit reached
    /// - `Error::Io`: Segment creation or write failure
    ///
    /// # Performance
    ///
    /// Hot path: ~200ns (memcpy to mmap, atomic commit, optional futex wake)
    pub fn publish(&mut self, type_id: u16, payload: &[u8]) -> Result<()> {
        self.writer.append(type_id, payload)
    }

    /// Asynchronously flushes recent writes to disk (best-effort durability).
    ///
    /// Calls `msync(MS_ASYNC)`. Does not block for completion.
    pub fn flush_async(&mut self) -> Result<()> {
        self.writer.flush_async()
    }

    /// Synchronously flushes recent writes to disk (power-loss safe).
    ///
    /// Calls `msync(MS_SYNC)`. Blocks until data is on disk.
    /// **Warning**: Can add milliseconds of latency. Use sparingly.
    pub fn flush_sync(&mut self) -> Result<()> {
        self.writer.flush_sync()
    }
}

/// A subscriber (reader) for broadcast messaging.
///
/// Wraps `QueueReader` with a pub/sub-oriented API. Many subscribers can independently
/// consume from the same queue, each tracking their own position.
///
/// # Lifetime
///
/// Subscribers must periodically call `commit()` to persist their position. If a
/// subscriber crashes, it resumes from the last committed position (or earliest/latest
/// depending on `StartMode`).
pub struct Subscriber {
    reader: QueueReader,
}

impl Subscriber {
    /// Opens a subscriber with default configuration.
    ///
    /// # Arguments
    ///
    /// - `path`: Queue directory path
    /// - `name`: Unique subscriber name (used for offset persistence and retention tracking)
    ///
    /// # Errors
    ///
    /// Returns `Error::NotFound` if the queue doesn't exist or isn't ready.
    pub fn open(path: impl AsRef<Path>, name: &str) -> Result<Self> {
        Self::open_with_config(path, name, ReaderConfig::default())
    }

    /// Opens a subscriber with custom configuration.
    pub fn open_with_config(
        path: impl AsRef<Path>,
        name: &str,
        config: ReaderConfig,
    ) -> Result<Self> {
        let reader = Queue::open_subscriber_with_config(path, name, config)?;
        Ok(Self { reader })
    }

    /// Tries to open a subscriber, returning `Ok(None)` if the queue isn't ready.
    ///
    /// Useful for discovery patterns where you poll for queue availability.
    pub fn try_open(path: impl AsRef<Path>, name: &str) -> Result<Option<Self>> {
        Self::try_open_with_config(path, name, ReaderConfig::default())
    }

    /// Tries to open a subscriber with custom configuration.
    pub fn try_open_with_config(
        path: impl AsRef<Path>,
        name: &str,
        config: ReaderConfig,
    ) -> Result<Option<Self>> {
        match Queue::try_open_subscriber_with_config(path, name, config)? {
            Some(reader) => Ok(Some(Self { reader })),
            None => Ok(None),
        }
    }

    /// Receives the next message (zero-copy).
    ///
    /// Returns `Ok(None)` if no new messages are available.
    ///
    /// # Borrowing
    ///
    /// The returned `MessageView` borrows from the internal mmap. It must be dropped
    /// before calling `recv()` again.
    ///
    /// # Performance
    ///
    /// Hot path: ~50ns (atomic load, pointer arithmetic)
    pub fn recv(&mut self) -> Result<Option<MessageView<'_>>> {
        self.reader.next()
    }

    /// Commits the current read position to disk.
    ///
    /// Subscribers should commit periodically (e.g., every 100 messages or every 10ms).
    /// Uncommitted progress is lost on crash.
    ///
    /// # Retention Impact
    ///
    /// The committed position determines retention eligibility. Segments are only
    /// deleted after all live subscribers have committed past them.
    pub fn commit(&mut self) -> Result<()> {
        self.reader.commit()
    }

    /// Blocks until a new message arrives or timeout expires.
    ///
    /// Uses the configured `WaitStrategy` (busy-spin, hybrid, or sleep).
    ///
    /// # Arguments
    ///
    /// - `timeout`: Maximum wait duration. `None` = wait indefinitely.
    ///
    /// # Returns
    ///
    /// - `Ok(())`: Wake condition met (new data or timeout)
    /// - `Err(Error::Timeout)`: Timeout expired (if timeout was `Some`)
    pub fn wait(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.reader.wait(timeout)
    }

    /// Sets the wait strategy (busy-spin, hybrid, or sleep).
    ///
    /// - `BusySpin`: 100% CPU, lowest latency (~200ns wake)
    /// - `SpinThenPark { spin_us }`: Hybrid (spin then futex, ~1-2µs wake)
    /// - `Sleep(dur)`: Periodic polling, ~ms latency
    pub fn set_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.reader.set_wait_strategy(strategy)
    }

    /// Checks publisher liveness.
    ///
    /// Returns `WriterStatus` indicating whether the publisher is alive and when it
    /// last updated its heartbeat.
    ///
    /// # Arguments
    ///
    /// - `ttl`: Time-to-live duration. Publisher is considered stale if heartbeat is older.
    pub fn writer_status(&self, ttl: Duration) -> Result<WriterStatus> {
        self.reader.writer_status(ttl)
    }

    /// Detects publisher disconnection.
    ///
    /// Returns `Some(reason)` if the publisher is dead/stale.
    ///
    /// # Arguments
    ///
    /// - `ttl`: Time-to-live duration. Publisher is considered stale if heartbeat is older.
    ///
    /// # Use Case
    ///
    /// Subscribers can detect feed failures and trigger alerts or failover logic.
    pub fn detect_disconnect(&self, ttl: Duration) -> Result<Option<DisconnectReason>> {
        self.reader.detect_disconnect(ttl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_pubsub_basic() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        // Publisher writes
        let mut pub_ = Publisher::open(path).unwrap();
        pub_.publish(1, b"hello").unwrap();
        pub_.publish(1, b"world").unwrap();

        // Subscriber reads
        let mut sub = Subscriber::open(path, "test_reader").unwrap();
        let msg1 = sub.recv().unwrap().unwrap();
        assert_eq!(msg1.payload, b"hello");
        sub.commit().unwrap();

        let msg2 = sub.recv().unwrap().unwrap();
        assert_eq!(msg2.payload, b"world");
        sub.commit().unwrap();

        assert!(sub.recv().unwrap().is_none());
    }

    #[test]
    fn test_multiple_subscribers() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        let mut pub_ = Publisher::open(path).unwrap();
        pub_.publish(1, b"data").unwrap();

        // Two independent subscribers
        let mut sub1 = Subscriber::open(path, "sub1").unwrap();
        let mut sub2 = Subscriber::open(path, "sub2").unwrap();

        let msg1 = sub1.recv().unwrap().unwrap();
        let msg2 = sub2.recv().unwrap().unwrap();

        assert_eq!(msg1.payload, b"data");
        assert_eq!(msg2.payload, b"data");
        assert_eq!(msg1.seq, msg2.seq); // Same message

        sub1.commit().unwrap();
        sub2.commit().unwrap();
    }
}
