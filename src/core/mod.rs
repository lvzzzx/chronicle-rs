//! Chronicle-style low-latency persisted messaging queue.
//!
//! This module is in early bootstrap. Modules are defined per the design
//! document and will be filled in during subsequent phases.

pub mod clock;
pub mod control;
pub mod error;
pub mod header;
pub mod merge;
pub mod mmap;
pub mod reader;
pub mod retention;
mod seek_index;
pub mod segment;
pub mod wait;
pub mod writer;
mod writer_lock;
pub mod zstd_seek;

pub use clock::{Clock, QuantaClock, SystemClock};
pub use error::{Error, Result};
pub use reader::{
    DisconnectReason, MessageView, QueueReader, ReaderConfig, StartMode, WaitStrategy, WriterStatus,
};
pub use writer::{BackpressurePolicy, Queue, QueueWriter, WriterConfig};
pub use writer_lock::{lock_owner_alive, read_lock_info, WriterLockInfo};
