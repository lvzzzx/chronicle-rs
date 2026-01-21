//! Chronicle-style low-latency persisted messaging queue.
//!
//! This crate is in early bootstrap. Modules are defined per the design
//! document and will be filled in during subsequent phases.

pub mod clock;
pub mod control;
pub mod error;
pub mod header;
pub mod merge;
pub mod mmap;
pub mod reader;
pub mod retention;
pub mod segment;
mod seek_index;
pub mod wait;
pub mod writer;
mod writer_lock;

pub use clock::{Clock, QuantaClock, SystemClock};
pub use error::{Error, Result};
pub use reader::{
    DisconnectReason, MessageView, QueueReader, ReaderConfig, StartMode, WaitStrategy,
    WriterStatus,
};
pub use writer::{BackpressurePolicy, Queue, QueueWriter, WriterConfig};
pub use writer_lock::{lock_owner_alive, read_lock_info, WriterLockInfo};
