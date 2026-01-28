//! Router-side trading channel abstractions.
//!
//! This module provides domain-specific wrappers for the router process to communicate
//! with strategies using Chronicle queues.

use crate::core::{MessageView, Result, ReaderConfig, WriterConfig};
use crate::ipc::BidirectionalChannel;
use crate::layout::IpcLayout;
use std::path::Path;

/// Type ID for order messages (strategy → router).
pub const TYPE_ORDER: u16 = 0x2000;

/// Type ID for fill/ack messages (router → strategy).
pub const TYPE_FILL: u16 = 0x2001;

/// Router-side bidirectional communication channel with a strategy.
///
/// ```text
/// Router Process (per strategy):
///   recv_order()     ← orders_out ← Strategy
///   send_fill()      → orders_in  → Strategy
/// ```
pub struct RouterChannel {
    inner: BidirectionalChannel,
    strategy_id: String,
}

impl RouterChannel {
    /// Connect to a strategy's order queues.
    ///
    /// # Arguments
    ///
    /// * `root` - Bus root directory
    /// * `strategy_id` - Strategy to connect to
    ///
    /// # Errors
    ///
    /// - `Error::WriterAlreadyActive`: Another router is already connected
    /// - `Error::Io`: Failed to open queues
    pub fn connect(root: &Path, strategy_id: impl Into<String>) -> Result<Self> {
        let strategy_id = strategy_id.into();
        let layout = IpcLayout::new(root);
        let orders = layout.orders();

        let endpoints = orders
            .strategy_endpoints(&crate::layout::StrategyId(strategy_id.clone()))
            .map_err(|_| crate::core::Error::Unsupported("invalid strategy_id"))?;

        // Router reads from orders_out, writes to orders_in
        let inner = BidirectionalChannel::open(
            &endpoints.orders_in,   // Router's outbox
            &endpoints.orders_out,  // Router's inbox
            "router",
            WriterConfig::default(),
            ReaderConfig::default(),
        )?;

        Ok(Self { inner, strategy_id })
    }

    /// Try to connect to a strategy (non-blocking).
    ///
    /// Returns `None` if the strategy hasn't marked itself as ready yet.
    pub fn try_connect(root: &Path, strategy_id: impl Into<String>) -> Result<Option<Self>> {
        let strategy_id = strategy_id.into();
        let layout = IpcLayout::new(root);
        let orders = layout.orders();

        let endpoints = orders
            .strategy_endpoints(&crate::layout::StrategyId(strategy_id.clone()))
            .map_err(|_| crate::core::Error::Unsupported("invalid strategy_id"))?;

        // Try to open (strategy might not be ready yet)
        match BidirectionalChannel::try_open(
            &endpoints.orders_in,   // Router's outbox
            &endpoints.orders_out,  // Router's inbox
            "router",
            WriterConfig::default(),
            ReaderConfig::default(),
        )? {
            Some(inner) => Ok(Some(Self { inner, strategy_id })),
            None => Ok(None),
        }
    }

    /// Receive an order from the strategy (non-blocking).
    ///
    /// Returns `None` if no orders are available.
    /// Call `commit_order()` after processing each order.
    pub fn recv_order(&mut self) -> Result<Option<MessageView<'_>>> {
        self.inner.recv_inbound()
    }

    /// Commit the last received order.
    ///
    /// Must be called after processing each order from `recv_order()`.
    pub fn commit_order(&mut self) -> Result<()> {
        self.inner.commit_inbound()
    }

    /// Send a fill/ack back to the strategy.
    pub fn send_fill(&mut self, fill: &[u8]) -> Result<()> {
        self.inner.send_outbound(TYPE_FILL, fill)
    }

    /// Flush outbound fills to disk.
    pub fn flush(&mut self) -> Result<()> {
        self.inner.flush_outbound_sync()
    }

    /// Returns the strategy ID this channel is connected to.
    pub fn strategy_id(&self) -> &str {
        &self.strategy_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Queue;
    use crate::layout::IpcLayout;
    use crate::trading::strategy::StrategyChannel;
    use tempfile::TempDir;

    #[test]
    fn test_router_channel_creation() {
        let dir = TempDir::new().unwrap();
        let layout = IpcLayout::new(dir.path());
        let orders = layout.orders();
        let endpoints = orders
            .strategy_endpoints(&crate::layout::StrategyId("test_strategy".to_string()))
            .unwrap();

        // Pre-create both queues
        Queue::open_publisher(&endpoints.orders_out).unwrap();
        Queue::open_publisher(&endpoints.orders_in).unwrap();

        // Strategy must connect first to create the queues
        let _strategy = StrategyChannel::connect(dir.path(), "test_strategy").unwrap();

        let router = RouterChannel::connect(dir.path(), "test_strategy").unwrap();
        assert_eq!(router.strategy_id(), "test_strategy");
    }

    #[test]
    fn test_try_connect_not_ready() {
        let dir = TempDir::new().unwrap();

        // Strategy hasn't connected, so try_connect should return None
        let result = RouterChannel::try_connect(dir.path(), "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_router_recv_order() {
        let dir = TempDir::new().unwrap();
        let layout = IpcLayout::new(dir.path());
        let orders = layout.orders();
        let endpoints = orders
            .strategy_endpoints(&crate::layout::StrategyId("test_strategy".to_string()))
            .unwrap();

        // Pre-create both queues
        Queue::open_publisher(&endpoints.orders_out).unwrap();
        Queue::open_publisher(&endpoints.orders_in).unwrap();

        // Strategy sends order
        let mut strategy = StrategyChannel::connect(dir.path(), "test_strategy").unwrap();
        strategy.send_order(b"BUY 100 AAPL").unwrap();
        strategy.flush().unwrap();

        // Router receives order
        let mut router = RouterChannel::connect(dir.path(), "test_strategy").unwrap();
        let order = router.recv_order().unwrap();
        assert!(order.is_some());
        let msg = order.unwrap();
        assert_eq!(msg.payload, b"BUY 100 AAPL");
        assert_eq!(msg.type_id, TYPE_ORDER);
        router.commit_order().unwrap();
    }
}
