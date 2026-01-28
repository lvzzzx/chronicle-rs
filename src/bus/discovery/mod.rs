//! Generic service discovery patterns.
//!
//! This module provides reusable discovery mechanisms for Chronicle queues.

pub mod subscriber;

pub use subscriber::{SubscriberConfig, SubscriberDiscovery, SubscriberEvent};
