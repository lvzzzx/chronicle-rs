//! Process orchestration and service discovery.
//!
//! This module provides generic coordination primitives for building
//! distributed systems with Chronicle queues.

pub mod discovery;
pub mod lease;
pub mod ready;
pub mod registration;

// Re-export discovery types (generic only)
pub use discovery::{SubscriberConfig, SubscriberDiscovery, SubscriberEvent};

// Re-export coordination primitives
pub use ready::{is_ready, mark_ready};
pub use registration::ReaderRegistration;

pub fn write_lease(endpoint_dir: &std::path::Path, payload: &[u8]) -> std::io::Result<()> {
    lease::write_lease(endpoint_dir, payload)
}
