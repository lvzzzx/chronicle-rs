//! Append-only log primitive.
//!
//! This module provides a simple append-only log that reuses Chronicle's segment
//! infrastructure for high-performance offline batch processing.
//!
//! # Design
//!
//! The log is a sequence of fixed-size segments stored in a directory:
//! ```text
//! {log_dir}/
//!   000000000.q      ← Segment 0
//!   000000001.q      ← Segment 1
//!   000000002.q      ← Current segment
//! ```
//!
//! # Key Differences from Queue
//!
//! | Feature | Queue | Log |
//! |---------|-------|-----|
//! | Writer | SPSC (writer.lock) | No lock (offline) |
//! | Readers | Multiple with offsets | Sequential replay |
//! | Use case | Hot path streaming | Cold path batch |
//! | Metadata | Reader offsets, heartbeats | None |
//!
//! # Example: Writer
//!
//! ```no_run
//! use chronicle::core::LogWriter;
//!
//! let mut log = LogWriter::open("./my_log", 128 * 1024 * 1024)?;
//!
//! // Append records
//! log.append(0x01, 1000000, b"event data")?;
//! log.append(0x02, 1000100, b"more data")?;
//!
//! // Flush and close
//! log.finish()?;
//! # Ok::<(), chronicle::core::Error>(())
//! ```
//!
//! # Example: Reader
//!
//! ```no_run
//! use chronicle::core::LogReader;
//!
//! let mut reader = LogReader::open("./my_log")?;
//!
//! // Sequential iteration
//! while let Some(msg) = reader.next()? {
//!     println!("seq={} ts={} type={}", msg.seq, msg.timestamp_ns, msg.type_id);
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```

use std::path::Path;

use crate::core::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, RECORD_ALIGN};
use crate::core::segment_cursor::SegmentCursor;
use crate::core::segment_store::{discover_segments, next_segment_id, DEFAULT_SEGMENT_SIZE, SEG_DATA_OFFSET};
use crate::core::segment_writer::SegmentWriter;
use crate::core::{Error, MessageView, Result};

/// Aligns a value up to the nearest multiple of `align`.
#[inline]
fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

/// Append-only log writer with automatic segment rolling.
///
/// Writes records sequentially to segments, automatically rolling to a new
/// segment when the current one fills up.
///
/// Built on top of `SegmentWriter` primitive.
///
/// # Example
///
/// ```no_run
/// use chronicle::core::LogWriter;
///
/// let mut log = LogWriter::open("./replay_log", 128 * 1024 * 1024)?;
/// log.append(0x01, 1000000, b"snapshot")?;
/// log.flush()?;
/// log.finish()?;
/// # Ok::<(), chronicle::core::Error>(())
/// ```
pub struct LogWriter {
    writer: SegmentWriter,
    segment_size: usize,
}

impl LogWriter {
    /// Open a log writer at the specified directory.
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory to store log segments
    /// * `segment_size` - Size of each segment (0 = default 128MB)
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to create directory or open segment
    /// - `Error::Unsupported`: Invalid segment size
    pub fn open(dir: impl AsRef<Path>, segment_size: usize) -> Result<Self> {
        let dir = dir.as_ref();
        let segment_size = if segment_size == 0 {
            DEFAULT_SEGMENT_SIZE
        } else {
            let validated = crate::core::segment::validate_segment_size(segment_size as u64)?;
            validated as usize
        };

        std::fs::create_dir_all(dir)?;

        let segment_id = next_segment_id(dir)?;
        let writer = SegmentWriter::new(dir, segment_id, segment_size);

        Ok(Self {
            writer,
            segment_size,
        })
    }

    /// Returns the number of segments written (sealed and published).
    pub fn segments_written(&self) -> u64 {
        self.writer.segments_published()
    }

    /// Returns the current sequence number.
    pub fn seq(&self) -> u64 {
        self.writer.seq()
    }

    /// Returns the current segment ID.
    pub fn segment_id(&self) -> u64 {
        self.writer.segment_id()
    }

    /// Returns the current write offset within the segment.
    pub fn write_offset(&self) -> u64 {
        self.writer.write_offset()
    }

    /// Append a record to the log.
    ///
    /// If the record doesn't fit in the current segment, automatically rolls
    /// to a new segment.
    ///
    /// # Arguments
    ///
    /// * `type_id` - Message type identifier
    /// * `timestamp_ns` - Nanosecond timestamp
    /// * `payload` - Message payload bytes
    ///
    /// # Errors
    ///
    /// - `Error::PayloadTooLarge`: Payload exceeds maximum size or segment capacity
    /// - `Error::Io`: Failed to write to segment
    pub fn append(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()> {
        let payload_len = payload.len();
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(Error::PayloadTooLarge);
        }

        let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);

        // Check if record would fit in any segment (not just current)
        let max_record_size = self.segment_size.saturating_sub(SEG_DATA_OFFSET);
        if record_len > max_record_size {
            return Err(Error::PayloadTooLarge);
        }

        // Roll if record doesn't fit in current segment
        if self.writer.needs_roll(record_len) {
            self.writer.roll()?;
        }

        self.writer.append(type_id, timestamp_ns, payload)
    }

    /// Flush pending writes to disk asynchronously.
    ///
    /// Ensures writes are visible but not necessarily durable.
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()
    }

    /// Finish writing and seal the current segment.
    ///
    /// Should be called when done writing to properly close the log.
    pub fn finish(&mut self) -> Result<()> {
        self.writer.finish()
    }
}

/// Sequential reader for append-only logs.
///
/// Reads records sequentially across multiple segments in timestamp order.
///
/// Built on top of `SegmentCursor` primitive.
///
/// # Example
///
/// ```no_run
/// use chronicle::core::LogReader;
///
/// let mut reader = LogReader::open("./replay_log")?;
/// while let Some(msg) = reader.next()? {
///     println!("Read: seq={} type={}", msg.seq, msg.type_id);
/// }
/// # Ok::<(), chronicle::core::Error>(())
/// ```
pub struct LogReader {
    cursor: SegmentCursor,
    payload_buf: Vec<u8>,
}

impl LogReader {
    /// Open a log reader for the specified directory.
    ///
    /// Discovers all segment files and prepares for sequential reading.
    /// Automatically detects segment size from the first segment.
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to read directory or open segments
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();

        if !dir.exists() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("log directory not found: {}", dir.display()),
            )));
        }

        let mut segments = discover_segments(dir)?;
        segments.sort_unstable();

        // Discover segment size from first segment (default to DEFAULT_SEGMENT_SIZE if no segments)
        let segment_size = if !segments.is_empty() {
            use crate::core::segment_store::segment_path;
            let first_segment_path = segment_path(dir, segments[0]);
            let metadata = std::fs::metadata(&first_segment_path)?;
            metadata.len() as usize
        } else {
            DEFAULT_SEGMENT_SIZE
        };

        let cursor = SegmentCursor::open(dir, segments, segment_size);

        Ok(Self {
            cursor,
            payload_buf: vec![0u8; 8192],
        })
    }

    /// Read the next record from the log.
    ///
    /// Returns `None` when reaching the end of all segments.
    pub fn next(&mut self) -> Result<Option<MessageView<'_>>> {
        // Get next message header from cursor
        let msg_ref = match self.cursor.next_header()? {
            Some(m) => m,
            None => return Ok(None),
        };

        // Ensure payload buffer is large enough
        if self.payload_buf.len() < msg_ref.payload_len {
            self.payload_buf.resize(msg_ref.payload_len, 0);
        }

        // Read payload into buffer
        self.cursor.read_payload(
            msg_ref.payload_offset,
            msg_ref.payload_len,
            &mut self.payload_buf,
        )?;

        // Verify checksum
        let computed_checksum = MessageHeader::crc32(&self.payload_buf[..msg_ref.payload_len]);
        if msg_ref.checksum != computed_checksum {
            return Err(Error::Corrupt("checksum mismatch"));
        }

        Ok(Some(MessageView {
            seq: msg_ref.seq,
            timestamp_ns: msg_ref.timestamp_ns,
            type_id: msg_ref.type_id,
            payload: &self.payload_buf[..msg_ref.payload_len],
        }))
    }

    /// Seek to a specific segment.
    ///
    /// Subsequent reads will start from the beginning of the specified segment.
    ///
    /// # Errors
    ///
    /// - `Error::Unsupported`: Segment ID not found
    pub fn seek_segment(&mut self, segment_id: u64) -> Result<()> {
        self.cursor.seek_segment(segment_id, SEG_DATA_OFFSET)
    }

    /// Returns the list of segment IDs in this log.
    pub fn segments(&self) -> &[u64] {
        self.cursor.segments()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_log_write_read_basic() {
        let dir = TempDir::new().unwrap();

        // Write some records
        let mut writer = LogWriter::open(dir.path(), 1024 * 1024).unwrap();
        writer.append(0x01, 1000, b"first").unwrap();
        writer.append(0x02, 2000, b"second").unwrap();
        writer.append(0x03, 3000, b"third").unwrap();
        writer.finish().unwrap();

        assert_eq!(writer.segments_written(), 1);

        // Read them back
        let mut reader = LogReader::open(dir.path()).unwrap();

        let msg1 = reader.next().unwrap().unwrap();
        assert_eq!(msg1.seq, 0);
        assert_eq!(msg1.timestamp_ns, 1000);
        assert_eq!(msg1.type_id, 0x01);
        assert_eq!(msg1.payload, b"first");

        let msg2 = reader.next().unwrap().unwrap();
        assert_eq!(msg2.seq, 1);
        assert_eq!(msg2.timestamp_ns, 2000);
        assert_eq!(msg2.type_id, 0x02);
        assert_eq!(msg2.payload, b"second");

        let msg3 = reader.next().unwrap().unwrap();
        assert_eq!(msg3.seq, 2);
        assert_eq!(msg3.timestamp_ns, 3000);
        assert_eq!(msg3.type_id, 0x03);
        assert_eq!(msg3.payload, b"third");

        assert!(reader.next().unwrap().is_none());
    }

    #[test]
    fn test_log_segment_rolling() {
        let dir = TempDir::new().unwrap();

        // Small segment size to force rolling
        let segment_size = 8192;
        let mut writer = LogWriter::open(dir.path(), segment_size).unwrap();

        let payload = vec![0u8; 1024];
        for i in 0..20 {
            writer.append(0x01, i * 1000, &payload).unwrap();
        }
        writer.finish().unwrap();

        // Should have created multiple segments
        assert!(writer.segments_written() > 1);

        // Read all records back
        let mut reader = LogReader::open(dir.path()).unwrap();
        let mut count = 0;
        while let Some(_msg) = reader.next().unwrap() {
            count += 1;
        }
        assert_eq!(count, 20);
    }

    #[test]
    fn test_log_empty() {
        let dir = TempDir::new().unwrap();

        let mut writer = LogWriter::open(dir.path(), 1024 * 1024).unwrap();
        writer.finish().unwrap();

        // Should not create any segments
        assert_eq!(writer.segments_written(), 0);

        // Reader should handle empty log
        let mut reader = LogReader::open(dir.path()).unwrap();
        assert!(reader.next().unwrap().is_none());
    }

    #[test]
    fn test_log_seek_segment() {
        let dir = TempDir::new().unwrap();

        // Write records across multiple segments
        let segment_size = 8192;
        let mut writer = LogWriter::open(dir.path(), segment_size).unwrap();
        let payload = vec![0u8; 1024];
        for i in 0..20 {
            writer.append(0x01, i * 1000, &payload).unwrap();
        }
        writer.finish().unwrap();

        // Open reader
        let mut reader = LogReader::open(dir.path()).unwrap();
        let segments = reader.segments().to_vec();
        assert!(segments.len() > 1);

        // Seek to second segment
        reader.seek_segment(segments[1]).unwrap();

        // Should read from second segment onwards
        let msg = reader.next().unwrap().unwrap();
        assert!(msg.seq > 0); // Not the first record
    }

    #[test]
    fn test_log_payload_too_large() {
        let dir = TempDir::new().unwrap();
        let mut writer = LogWriter::open(dir.path(), 1024 * 1024).unwrap();

        let huge_payload = vec![0u8; MAX_PAYLOAD_LEN + 1];
        let result = writer.append(0x01, 1000, &huge_payload);
        assert!(matches!(result, Err(Error::PayloadTooLarge)));
    }

    #[test]
    fn test_log_no_segment_gap_on_oversized_payload() {
        let dir = TempDir::new().unwrap();

        // Small segment size to make testing easier
        let segment_size = 8192;
        let mut writer = LogWriter::open(dir.path(), segment_size).unwrap();

        // Write a normal record to segment 0
        writer.append(0x01, 1000, b"normal").unwrap();

        // Try to write a payload that's too large for any segment
        // (fits under MAX_PAYLOAD_LEN but not in segment capacity)
        let oversized = vec![0u8; segment_size - SEG_DATA_OFFSET + 100];
        let result = writer.append(0x02, 2000, &oversized);
        assert!(matches!(result, Err(Error::PayloadTooLarge)));

        // Write another normal record - should still be in segment 0
        writer.append(0x03, 3000, b"after_error").unwrap();
        writer.finish().unwrap();

        // Should only have created segment 0 (no gap from failed append)
        let mut reader = LogReader::open(dir.path()).unwrap();
        let segments = reader.segments();
        assert_eq!(segments, &[0], "Should only have segment 0, no gaps");

        // Verify we can read both successful records
        let msg1 = reader.next().unwrap().unwrap();
        assert_eq!(msg1.type_id, 0x01);

        let msg2 = reader.next().unwrap().unwrap();
        assert_eq!(msg2.type_id, 0x03);

        assert!(reader.next().unwrap().is_none());
    }
}
