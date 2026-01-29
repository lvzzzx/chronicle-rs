//! Compression layer for Chronicle-RS storage.
//!
//! This module provides unified compression support for both hot (.q) and cold (.q.zst) storage.
//! The compression is based on seekable zstd format, allowing random access to compressed data.
//!
//! # Key Components
//!
//! - `zstd`: Basic zstd compression/decompression operations
//! - `zstd_seek`: Seekable zstd index format for random access
//! - `unified_reader`: Transparent reader that works with both .q and .q.zst files
//!
//! # Usage
//!
//! ```rust,ignore
//! use chronicle::core::compression::{compress_q_to_zst, UnifiedSegmentReader};
//!
//! // Compress a segment
//! compress_q_to_zst("data.q", "data.q.zst", "data.q.zst.idx", 1024 * 1024)?;
//!
//! // Read transparently (automatically detects format)
//! let reader = UnifiedSegmentReader::open("data", 0)?;
//! ```

mod zstd;
mod zstd_seek;
mod unified_reader;

pub use zstd::{compress_q_to_zst, ZstdBlockReader};
pub use zstd_seek::{
    read_seek_index, write_seek_index, ZstdSeekEntry, ZstdSeekHeader, ZstdSeekIndex,
    ZSTD_INDEX_ENTRY_LEN, ZSTD_INDEX_HEADER_LEN, ZSTD_INDEX_MAGIC, ZSTD_INDEX_VERSION,
};
pub use unified_reader::UnifiedSegmentReader;
