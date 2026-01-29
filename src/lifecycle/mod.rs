//! Storage lifecycle management.
//!
//! This module provides automated lifecycle management for segments, including:
//! - Compression policy evaluation
//! - Access tracking for idle data detection
//! - Atomic hot → cold storage transitions
//! - Statistics and monitoring
//!
//! # Overview
//!
//! The lifecycle manager automatically compresses segments based on configurable
//! policies (age, idle time, size, etc.) while maintaining data integrity and
//! transparent access through `UnifiedSegmentReader`.
//!
//! # Example
//!
//! ```rust,ignore
//! use chronicle::lifecycle::{StorageLifecycleManager, LifecycleConfig};
//! use chronicle::table::CompressionPolicy;
//!
//! let config = LifecycleConfig {
//!     policy: CompressionPolicy::IdleAfter {
//!         days: 7,
//!         track_reads: true,
//!     },
//!     compression_level: 3,
//!     block_size: 1024 * 1024,
//!     parallel_workers: 4,
//! };
//!
//! let manager = StorageLifecycleManager::new("./data", config);
//!
//! // Run lifecycle management
//! let stats = manager.run_once()?;
//! println!("Compressed {} segments, saved {} bytes",
//!     stats.compressed_count, stats.bytes_saved);
//! ```

mod access_tracker;
mod compressor;
mod manager;
mod policy;
mod segment_info;
mod stats;

pub use access_tracker::AccessTracker;
pub use compressor::compress_segment;
pub use manager::{LifecycleConfig, StorageLifecycleManager};
pub use policy::should_compress;
pub use segment_info::SegmentInfo;
pub use stats::LifecycleStats;
