//! Chronicle-style low-latency persisted messaging queue.
//!
//! This crate is in early bootstrap. Modules are defined per the design
//! document and will be filled in during subsequent phases.

pub mod error;
pub mod header;
pub mod control;
pub mod mmap;
pub mod merge;
pub mod reader;
pub mod retention;
pub mod segment;
pub mod wait;
pub mod writer;

pub use error::{Error, Result};
pub use reader::{MessageView, QueueReader, WaitStrategy};
pub use writer::{Queue, QueueWriter};
