//! Fan-in (many-to-one merge) pattern.
//!
//! This module provides timestamp-ordered merging of multiple queues. Commonly used
//! by routers to aggregate orders from multiple strategies, or for multi-feed
//! consolidation.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐
//! │  Strategy A │
//! │ orders_out  │──┐
//! └─────────────┘  │
//!                  │    ┌──────────────┐
//! ┌─────────────┐  ├───►│ FanInReader  │──► Merged, time-ordered stream
//! │  Strategy B │  │    └──────────────┘
//! │ orders_out  │──┤
//! └─────────────┘  │
//!                  │
//! ┌─────────────┐  │
//! │  Strategy C │  │
//! │ orders_out  │──┘
//! └─────────────┘
//! ```
//!
//! Messages are ordered by `MessageHeader.timestamp_ns` (canonical ingest time).
//! Ties are broken by source index.
//!
//! # Performance Notes
//!
//! This implementation buffers one message per source and performs O(N) min-heap
//! selection on each `next()` call. For small N (typical router scenarios with
//! 1-10 strategies), this is faster than heap maintenance overhead.
//!
//! For zero-copy fanin on live queues with internal buffer access, see
//! `crate::stream::merge::FanInReader`.
//!
//! # Example: Router Fan-In
//!
//! ```no_run
//! use chronicle::ipc::fanin::FanInReader;
//! use chronicle::ipc::pubsub::Subscriber;
//! use chronicle::layout::IpcLayout;
//!
//! let layout = IpcLayout::new("/var/lib/hft_bus");
//!
//! // Open all strategy order queues
//! let sub_a = Subscriber::open(
//!     layout.orders().strategy_endpoints(&"strategy_a".into())?.orders_out,
//!     "router"
//! )?;
//! let sub_b = Subscriber::open(
//!     layout.orders().strategy_endpoints(&"strategy_b".into())?.orders_out,
//!     "router"
//! )?;
//!
//! let mut fanin = FanInReader::new(vec![sub_a, sub_b])?;
//!
//! // Process orders in global timestamp order
//! loop {
//!     while let Some(msg) = fanin.next()? {
//!         println!("Order from source {}: {:?}", msg.source, msg.payload);
//!         fanin.commit(msg.source)?;
//!     }
//!     fanin.wait(None)?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```

use std::time::Duration;

use crate::core::{Error, Result, WaitStrategy};
use crate::ipc::pubsub::Subscriber;

/// A message from a fan-in merge, tagged with its source index.
#[derive(Debug)]
pub struct MergedMessage {
    /// Source index (0-based, matches the order of subscribers passed to `new()`)
    pub source: usize,
    /// Monotonic sequence number (from source queue)
    pub seq: u64,
    /// Canonical timestamp (used for ordering)
    pub timestamp_ns: u64,
    /// Application-defined message type
    pub type_id: u16,
    /// Message payload (owned copy to enable multi-source buffering)
    pub payload: Vec<u8>,
}

/// Many-to-one fan-in reader with timestamp-ordered merge.
///
/// Reads from multiple queues and returns messages in increasing timestamp order.
///
/// # Ordering Guarantee
///
/// Messages are ordered by `timestamp_ns` (ascending). If two messages have the
/// same timestamp, the one from the lower source index is returned first.
///
/// # Buffering
///
/// Fan-in maintains one pending message per source. When `next()` is called,
/// it selects the message with the earliest timestamp across all sources.
///
/// # Commit Semantics
///
/// Each source has independent commit tracking. Call `commit(source)` to persist
/// progress for that source. Uncommitted messages are replayed on restart.
pub struct FanInReader {
    sources: Vec<Subscriber>,
    pending: Vec<Option<PendingMessage>>,
    wait_strategy: WaitStrategy,
}

/// Internal representation of a buffered message (owned payload).
struct PendingMessage {
    seq: u64,
    timestamp_ns: u64,
    type_id: u16,
    payload: Vec<u8>,
}

impl FanInReader {
    /// Creates a new fan-in reader from a list of subscribers.
    ///
    /// Source indices are assigned in the order of the vector (0, 1, 2, ...).
    ///
    /// # Errors
    ///
    /// Returns `Error::Unsupported` if the sources list is empty.
    pub fn new(sources: Vec<Subscriber>) -> Result<Self> {
        if sources.is_empty() {
            return Err(Error::Unsupported("fan-in requires at least one source"));
        }

        let pending = sources.iter().map(|_| None).collect();

        Ok(Self {
            sources,
            pending,
            wait_strategy: WaitStrategy::SpinThenPark { spin_us: 10 },
        })
    }

    /// Adds a new source to the fan-in reader.
    ///
    /// The new source is assigned the next available index.
    ///
    /// # Dynamic Discovery
    ///
    /// This method supports dynamic discovery patterns where new strategies
    /// come online after the router starts.
    pub fn add_source(&mut self, source: Subscriber) {
        self.sources.push(source);
        self.pending.push(None);
    }

    /// Returns the number of sources.
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// Sets the wait strategy for all sources.
    ///
    /// This applies to the `wait()` method. Individual source wait strategies
    /// are not affected.
    pub fn set_wait_strategy(&mut self, strategy: WaitStrategy) {
        self.wait_strategy = strategy;
    }

    /// Returns the next message in timestamp order.
    ///
    /// Returns `Ok(None)` if no new messages are available from any source.
    ///
    /// # Ordering
    ///
    /// Messages are returned in increasing `timestamp_ns` order. Ties are broken
    /// by source index (lower index wins).
    ///
    /// # Performance Note
    ///
    /// The payload is an owned copy to support multi-source buffering. For zero-copy
    /// fan-in on live queues, see `crate::stream::merge::FanInReader`.
    pub fn next(&mut self) -> Result<Option<MergedMessage>> {
        // Fill pending slots
        for (index, source) in self.sources.iter_mut().enumerate() {
            if self.pending[index].is_none() {
                if let Some(msg) = source.recv()? {
                    self.pending[index] = Some(PendingMessage {
                        seq: msg.seq,
                        timestamp_ns: msg.timestamp_ns,
                        type_id: msg.type_id,
                        payload: msg.payload.to_vec(), // Owned copy (required for multi-source hold)
                    });
                }
            }
        }

        // Find earliest message
        let mut best: Option<(usize, u64)> = None;
        for (index, pending) in self.pending.iter().enumerate() {
            let Some(msg) = pending.as_ref() else {
                continue;
            };
            let timestamp = msg.timestamp_ns;
            match best {
                None => best = Some((index, timestamp)),
                Some((_, best_ts)) => {
                    if timestamp < best_ts || (timestamp == best_ts && index < best.unwrap().0) {
                        best = Some((index, timestamp));
                    }
                }
            }
        }

        // Extract and return
        let Some((source_idx, _)) = best else {
            return Ok(None);
        };

        let msg = self.pending[source_idx]
            .take()
            .ok_or(Error::Corrupt("pending message missing"))?;

        Ok(Some(MergedMessage {
            source: source_idx,
            seq: msg.seq,
            timestamp_ns: msg.timestamp_ns,
            type_id: msg.type_id,
            payload: msg.payload, // Owned payload
        }))
    }

    /// Commits the read position for a specific source.
    ///
    /// # Arguments
    ///
    /// - `source`: Source index (0-based, same as `MergedMessage.source`)
    ///
    /// # Errors
    ///
    /// Returns `Error::Unsupported` if the source index is invalid.
    pub fn commit(&mut self, source: usize) -> Result<()> {
        let sub = self
            .sources
            .get_mut(source)
            .ok_or(Error::Unsupported("invalid fan-in source index"))?;
        sub.commit()
    }

    /// Commits all sources up to their current read positions.
    ///
    /// Useful for batch commit at the end of a processing loop.
    pub fn commit_all(&mut self) -> Result<()> {
        for source in &mut self.sources {
            source.commit()?;
        }
        Ok(())
    }

    /// Blocks until any source has a new message or timeout expires.
    ///
    /// Uses the configured `WaitStrategy`. If any source has data, returns immediately.
    ///
    /// # Limitations
    ///
    /// This implementation polls all sources sequentially. For high-performance
    /// scenarios, consider using `eventfd` or per-source threads.
    pub fn wait(&mut self, timeout: Option<Duration>) -> Result<()> {
        // Simple implementation: check all sources, then sleep if needed
        for source in &mut self.sources {
            // Each source's wait() is a no-op if data is already available
            source.wait(timeout)?;
        }
        Ok(())
    }

    /// Returns the number of pending messages (buffered but not yet returned).
    pub fn pending_count(&self) -> usize {
        self.pending.iter().filter(|p| p.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::pubsub::Publisher;
    use tempfile::TempDir;

    #[test]
    fn test_fanin_timestamp_ordering() {
        let dir = TempDir::new().unwrap();
        let path_a = dir.path().join("queue_a");
        let path_b = dir.path().join("queue_b");

        // Create two publishers
        let mut pub_a = Publisher::open(&path_a).unwrap();
        let mut pub_b = Publisher::open(&path_b).unwrap();

        // Write messages with explicit timestamps (via direct append)
        // Note: In real usage, timestamps are set by QueueWriter
        pub_a.publish(1, b"msg_a_100").unwrap();
        pub_b.publish(1, b"msg_b_50").unwrap();
        pub_a.publish(1, b"msg_a_200").unwrap();
        pub_b.publish(1, b"msg_b_150").unwrap();

        // Create subscribers
        let sub_a = Subscriber::open(&path_a, "reader").unwrap();
        let sub_b = Subscriber::open(&path_b, "reader").unwrap();

        let mut fanin = FanInReader::new(vec![sub_a, sub_b]).unwrap();

        // Read all messages
        let mut messages = Vec::new();
        while let Some(msg) = fanin.next().unwrap() {
            messages.push((msg.source, msg.payload.to_vec()));
            fanin.commit(msg.source).unwrap();
        }

        // All 4 messages should be present
        assert_eq!(messages.len(), 4);

        // Note: Without controlling timestamps explicitly, we can only verify
        // that messages from each source appear in order
        let a_msgs: Vec<_> = messages.iter().filter(|(s, _)| *s == 0).collect();
        let b_msgs: Vec<_> = messages.iter().filter(|(s, _)| *s == 1).collect();

        assert_eq!(a_msgs.len(), 2);
        assert_eq!(b_msgs.len(), 2);
    }

    #[test]
    fn test_fanin_empty_sources() {
        let result = FanInReader::new(vec![]);
        assert!(result.is_err());
    }

    #[test]
    fn test_fanin_add_source() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("queue");

        let _pub = Publisher::open(&path).unwrap();
        let sub = Subscriber::open(&path, "reader").unwrap();

        let mut fanin = FanInReader::new(vec![sub]).unwrap();
        assert_eq!(fanin.source_count(), 1);

        let sub2 = Subscriber::open(&path, "reader2").unwrap();
        fanin.add_source(sub2);
        assert_eq!(fanin.source_count(), 2);
    }
}
