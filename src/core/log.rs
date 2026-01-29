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

use std::path::{Path, PathBuf};

use crate::core::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, RECORD_ALIGN};
use crate::core::mmap::MmapFile;
use crate::core::segment_store::{discover_segments, next_segment_id, segment_path, DEFAULT_SEGMENT_SIZE, SEG_DATA_OFFSET};
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

        Ok(Self { writer })
    }

    /// Returns the number of segments written (sealed and published).
    pub fn segments_written(&self) -> u64 {
        self.writer.segments_published()
    }

    /// Returns the current sequence number.
    pub fn seq(&self) -> u64 {
        self.writer.seq()
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
    /// - `Error::PayloadTooLarge`: Payload exceeds maximum size
    /// - `Error::Io`: Failed to write to segment
    pub fn append(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()> {
        let payload_len = payload.len();
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(Error::PayloadTooLarge);
        }

        let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);

        // Roll if record doesn't fit
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
    dir: PathBuf,
    segments: Vec<u64>,
    current_segment_idx: usize,
    current_mmap: Option<MmapFile>,
    offset: usize,
    header_buf: [u8; HEADER_SIZE],
    payload_buf: Vec<u8>,
}

impl LogReader {
    /// Open a log reader for the specified directory.
    ///
    /// Discovers all segment files and prepares for sequential reading.
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to read directory or open segments
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();

        if !dir.exists() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("log directory not found: {}", dir.display()),
            )));
        }

        let mut segments = discover_segments(&dir)?;
        segments.sort_unstable();

        Ok(Self {
            dir,
            segments,
            current_segment_idx: 0,
            current_mmap: None,
            offset: SEG_DATA_OFFSET,
            header_buf: [0u8; HEADER_SIZE],
            payload_buf: vec![0u8; 8192],
        })
    }

    /// Read the next record from the log.
    ///
    /// Returns `None` when reaching the end of all segments.
    pub fn next(&mut self) -> Result<Option<MessageView<'_>>> {
        loop {
            // Load next segment if needed
            if self.current_mmap.is_none() {
                if !self.load_next_segment()? {
                    return Ok(None); // No more segments
                }
            }

            let mmap = self.current_mmap.as_ref().unwrap();
            let start = self.offset;

            // Check if we can read a header
            if start + HEADER_SIZE > mmap.len() {
                // End of current segment, try next
                self.current_mmap = None;
                self.current_segment_idx += 1;
                self.offset = SEG_DATA_OFFSET;
                continue;
            }

            // Read header
            self.header_buf.copy_from_slice(mmap.range(start, HEADER_SIZE)?);

            // Check commit_len first (without validating version)
            let commit_len = MessageHeader::load_commit_len(&self.header_buf[0] as *const u8);
            if commit_len == 0 {
                // Uncommitted or end of segment
                self.current_mmap = None;
                self.current_segment_idx += 1;
                self.offset = SEG_DATA_OFFSET;
                continue;
            }

            // Now parse the full header
            let header = MessageHeader::from_bytes(&self.header_buf)?;

            let payload_len = MessageHeader::payload_len_from_commit(commit_len)?;

            // Check if we can read the full record
            let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
            if start + record_len > mmap.len() {
                // Corrupted or truncated
                self.current_mmap = None;
                self.current_segment_idx += 1;
                self.offset = SEG_DATA_OFFSET;
                continue;
            }

            // Ensure payload buffer is large enough
            if self.payload_buf.len() < payload_len {
                self.payload_buf.resize(payload_len, 0);
            }

            // Read payload
            if payload_len > 0 {
                self.payload_buf[..payload_len]
                    .copy_from_slice(mmap.range(start + HEADER_SIZE, payload_len)?);
            }

            // Verify checksum
            let computed_checksum = MessageHeader::crc32(&self.payload_buf[..payload_len]);
            if header.checksum != computed_checksum {
                return Err(Error::Corrupt("checksum mismatch"));
            }

            self.offset += record_len;

            return Ok(Some(MessageView {
                seq: header.seq,
                timestamp_ns: header.timestamp_ns,
                type_id: header.type_id,
                payload: &self.payload_buf[..payload_len],
            }));
        }
    }

    /// Seek to a specific segment.
    ///
    /// Subsequent reads will start from the beginning of the specified segment.
    ///
    /// # Errors
    ///
    /// - `Error::Unsupported`: Segment ID not found
    pub fn seek_segment(&mut self, segment_id: u64) -> Result<()> {
        let idx = self
            .segments
            .iter()
            .position(|&id| id == segment_id)
            .ok_or(Error::Unsupported("segment not found"))?;

        self.current_segment_idx = idx;
        self.current_mmap = None;
        self.offset = SEG_DATA_OFFSET;
        Ok(())
    }

    /// Returns the list of segment IDs in this log.
    pub fn segments(&self) -> &[u64] {
        &self.segments
    }

    fn load_next_segment(&mut self) -> Result<bool> {
        if self.current_segment_idx >= self.segments.len() {
            return Ok(false);
        }

        let segment_id = self.segments[self.current_segment_idx];
        let path = segment_path(&self.dir, segment_id);

        if !path.exists() {
            // Skip missing segments
            self.current_segment_idx += 1;
            self.offset = SEG_DATA_OFFSET;
            return self.load_next_segment();
        }

        let mmap = MmapFile::open(&path)?;
        self.current_mmap = Some(mmap);
        self.offset = SEG_DATA_OFFSET;

        Ok(true)
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
}
