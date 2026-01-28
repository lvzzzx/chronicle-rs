//! Trading system domain abstractions.
//!
//! This module provides high-level abstractions for building trading systems on Chronicle queues.
//! It encapsulates trading-specific concepts like strategies, routers, and orders.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │  trading/ (Domain Layer)                                │
//! │  - Strategy/Router abstractions                         │
//! │  - Order flow patterns                                  │
//! │  - Path conventions                                     │
//! ├─────────────────────────────────────────────────────────┤
//! │  ipc/ (Generic Infrastructure)                          │
//! │  - PubSub, Bidirectional, FanIn                        │
//! ├─────────────────────────────────────────────────────────┤
//! │  core/ (Queue Primitive)                                │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! # Path Convention
//!
//! The trading module uses a standard directory layout:
//!
//! ```text
//! {root}/
//! ├── streams/
//! │   ├── raw/{venue}/queue                    ← Market data feeds
//! │   └── clean/{venue}/{symbol}/{stream}/queue
//! └── orders/
//!     └── queue/{strategy_id}/
//!         ├── orders_out/  ← Strategy → Router
//!         └── orders_in/   ← Router → Strategy
//! ```
//!
//! # Example: Strategy
//!
//! ```no_run
//! use chronicle::trading::StrategyChannel;
//!
//! let mut channel = StrategyChannel::connect("./bus", "momentum_strategy")?;
//!
//! // Send order to router
//! channel.send_order(b"BUY 100 AAPL")?;
//!
//! // Receive fills
//! while let Some(fill) = channel.recv_fill()? {
//!     println!("Fill: {:?}", std::str::from_utf8(fill));
//!     channel.commit_fill()?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```
//!
//! # Example: Router
//!
//! ```no_run
//! use chronicle::trading::RouterChannel;
//!
//! let mut channel = RouterChannel::connect("./bus", "momentum_strategy")?;
//!
//! // Receive orders from strategy
//! while let Some(order) = channel.recv_order()? {
//!     println!("Order: {:?}", std::str::from_utf8(order));
//!
//!     // Send fill back
//!     channel.send_fill(b"FILLED 100 AAPL @ 150.00")?;
//!     channel.commit_order()?;
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```

pub mod discovery;
pub mod paths;
pub mod router;
pub mod strategy;

pub use discovery::{DiscoveryEvent, RouterDiscovery, StrategyEndpoints, StrategyId};
pub use paths::{
    clean_stream_path, orders_root_path, raw_feed_path, strategy_orders_in_path,
    strategy_orders_out_path, validate_component, PathError,
};
pub use router::RouterChannel;
pub use strategy::StrategyChannel;
