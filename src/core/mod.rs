//! Chronicle-style low-latency persisted messaging queue.
//!
//! This module is in early bootstrap. Modules are defined per the design
//! document and will be filled in during subsequent phases.

pub mod clock;
pub mod compression;
pub mod control;
pub mod error;
pub mod header;
pub mod log;
pub mod mmap;
pub mod reader;
pub mod retention;
pub mod seek_index;
pub mod segment;
pub mod segment_cursor;
pub mod segment_store;
pub mod segment_writer;
pub mod wait;
pub mod writer;
mod writer_lock;

pub use clock::{Clock, QuantaClock, SystemClock};
pub use error::{Error, Result};
pub use header::MSG_VERSION;
pub use log::{LogReader, LogWriter};
pub use reader::{
    DisconnectReason, MessageView, OwnedMessage, QueueReader, ReaderConfig, StartMode,
    WaitStrategy, WriterStatus,
};
pub use retention::RetentionConfig;
pub use writer::{BackpressurePolicy, Queue, QueueWriter, WriterConfig, WriterMetrics};
pub use writer_lock::{lock_owner_alive, read_lock_info, WriterLockInfo};

// Re-export timeseries types from table module for backward compatibility
pub use crate::table::{SeekResult, TimeSeriesReader, TimeSeriesWriter};
