//! Chronicle-style low-latency persisted messaging queue.
//!
//! This crate is in early bootstrap. Modules are defined per the design
//! document and will be filled in during subsequent phases.

pub mod error;
pub mod header;
pub mod mmap;
pub mod merge;
pub mod notifier;
pub mod reader;
pub mod segment;
pub mod writer;

pub use error::{Error, Result};
pub use reader::QueueReader;
pub use writer::{Queue, QueueWriter};
