//! Strategy-side trading channel abstractions.
//!
//! This module provides domain-specific wrappers for strategy processes to communicate
//! with the router using Chronicle queues.

use crate::core::{MessageView, Result, ReaderConfig, WriterConfig};
use crate::ipc::BidirectionalChannel;
use crate::trading::paths::{strategy_orders_in_path, strategy_orders_out_path};
use std::path::Path;

/// Type ID for order messages (strategy → router).
pub const TYPE_ORDER: u16 = 0x2000;

/// Type ID for fill/ack messages (router → strategy).
pub const TYPE_FILL: u16 = 0x2001;

/// Strategy-side bidirectional communication channel with the router.
///
/// ```text
/// Strategy Process:
///   send_order()     → orders_out → Router
///   recv_fill()      ← orders_in  ← Router
/// ```
pub struct StrategyChannel {
    inner: BidirectionalChannel,
    strategy_id: String,
}

impl StrategyChannel {
    /// Connect to the router as a strategy.
    ///
    /// # Arguments
    ///
    /// * `root` - Bus root directory
    /// * `strategy_id` - Unique strategy identifier
    ///
    /// # Errors
    ///
    /// - `Error::WriterAlreadyActive`: Another instance of this strategy is running
    /// - `Error::Io`: Failed to create queue directories
    pub fn connect(root: &Path, strategy_id: impl Into<String>) -> Result<Self> {
        let strategy_id = strategy_id.into();

        let orders_out = strategy_orders_out_path(root, &strategy_id)
            .map_err(|e| crate::core::Error::Unsupported("invalid strategy_id"))?;
        let orders_in = strategy_orders_in_path(root, &strategy_id)
            .map_err(|e| crate::core::Error::Unsupported("invalid strategy_id"))?;

        // Strategy writes to orders_out, reads from orders_in
        let inner = BidirectionalChannel::open(
            &orders_out,
            &orders_in,
            &strategy_id,
            WriterConfig::default(),
            ReaderConfig::default(),
        )?;

        Ok(Self { inner, strategy_id })
    }

    /// Send an order to the router.
    ///
    /// Orders are written to the `orders_out` queue where the router will pick them up.
    pub fn send_order(&mut self, order: &[u8]) -> Result<()> {
        self.inner.send_outbound(TYPE_ORDER, order)
    }

    /// Receive a fill/ack from the router (non-blocking).
    ///
    /// Returns `None` if no messages are available.
    /// Call `commit_fill()` after processing each message.
    pub fn recv_fill(&mut self) -> Result<Option<MessageView<'_>>> {
        self.inner.recv_inbound()
    }

    /// Commit the last received fill/ack.
    ///
    /// Must be called after processing each message from `recv_fill()`.
    pub fn commit_fill(&mut self) -> Result<()> {
        self.inner.commit_inbound()
    }

    /// Flush outbound orders to disk.
    pub fn flush(&mut self) -> Result<()> {
        self.inner.flush_outbound_sync()
    }

    /// Returns the strategy ID.
    pub fn strategy_id(&self) -> &str {
        &self.strategy_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Queue;
    use crate::trading::paths::{strategy_orders_in_path, strategy_orders_out_path};
    use tempfile::TempDir;

    #[test]
    fn test_strategy_channel_creation() {
        let dir = TempDir::new().unwrap();

        // Pre-create queues so channel can open
        let orders_out = strategy_orders_out_path(dir.path(), "test_strategy").unwrap();
        let orders_in = strategy_orders_in_path(dir.path(), "test_strategy").unwrap();
        Queue::open_publisher(&orders_out).unwrap();
        Queue::open_publisher(&orders_in).unwrap();

        let channel = StrategyChannel::connect(dir.path(), "test_strategy").unwrap();
        assert_eq!(channel.strategy_id(), "test_strategy");
    }

    #[test]
    fn test_strategy_send_order() {
        let dir = TempDir::new().unwrap();

        // Pre-create queues
        let orders_out = strategy_orders_out_path(dir.path(), "test_strategy").unwrap();
        let orders_in = strategy_orders_in_path(dir.path(), "test_strategy").unwrap();
        Queue::open_publisher(&orders_out).unwrap();
        Queue::open_publisher(&orders_in).unwrap();

        let mut strategy = StrategyChannel::connect(dir.path(), "test_strategy").unwrap();
        strategy.send_order(b"BUY 100 AAPL").unwrap();
        strategy.flush().unwrap();
    }
}
