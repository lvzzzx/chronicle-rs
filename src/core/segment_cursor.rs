//! Core segment cursor for sequential reading.
//!
//! Provides low-level sequential reading across multiple segments.
//! Used as a building block for both Queue and Log readers.
//!
//! # Design
//!
//! - Manages list of segment IDs and current position
//! - Opens segments lazily as needed
//! - Reads headers and payloads sequentially
//! - Handles segment transitions automatically
//! - No coordination metadata (reader tracking, wait strategies, etc.)

use std::path::PathBuf;

use crate::core::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, RECORD_ALIGN};
use crate::core::mmap::MmapFile;
use crate::core::segment_store::{open_segment, segment_path, SEG_DATA_OFFSET};
use crate::core::{Error, Result};

/// Aligns a value up to the nearest multiple of `align`.
#[inline]
fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

/// Reference to a message header read from a segment.
///
/// Contains parsed header fields and location information for the payload.
#[derive(Debug, Clone)]
pub struct MessageRef {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub type_id: u16,
    pub payload_offset: usize,
    pub payload_len: usize,
}

/// Core segment cursor for sequential reading across segments.
///
/// This is a low-level primitive that both `QueueReader` and `LogReader` build upon.
/// It manages:
/// - Lazy segment loading
/// - Sequential header reading
/// - Payload access
/// - Automatic segment transitions
///
/// It does NOT manage:
/// - Reader registration or metadata
/// - Wait strategies
/// - Writer status tracking
/// - Commit operations
///
/// # Usage Pattern
///
/// ```text
/// 1. Create cursor with open()
/// 2. Loop:
///    - next_header() to get header and payload location
///    - read_payload() to access payload data
///    - Automatic segment advance when exhausted
/// 3. Drop cursor when done
/// ```
pub struct SegmentCursor {
    /// Directory containing segments
    dir: PathBuf,
    /// List of segment IDs (sorted)
    segments: Vec<u64>,
    /// Index of current segment in segments list
    current_segment_idx: usize,
    /// Segment size in bytes
    segment_size: usize,
    /// Current segment mmap (None if not loaded)
    current_mmap: Option<MmapFile>,
    /// Current read offset within segment
    offset: usize,
}

impl SegmentCursor {
    /// Create a new segment cursor.
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory containing segment files
    /// * `segments` - Sorted list of segment IDs to read
    /// * `segment_size` - Size of each segment in bytes
    ///
    /// Segments are opened lazily on first read.
    pub fn open(dir: impl Into<PathBuf>, segments: Vec<u64>, segment_size: usize) -> Self {
        Self {
            dir: dir.into(),
            segments,
            current_segment_idx: 0,
            segment_size,
            current_mmap: None,
            offset: SEG_DATA_OFFSET,
        }
    }

    /// Get the current segment ID, if any segment is loaded.
    pub fn current_segment_id(&self) -> Option<u64> {
        if self.current_segment_idx < self.segments.len() {
            Some(self.segments[self.current_segment_idx])
        } else {
            None
        }
    }

    /// Get the current read offset within the segment.
    pub fn current_offset(&self) -> usize {
        self.offset
    }

    /// Read the next message header from the segments.
    ///
    /// Returns `None` when all segments are exhausted.
    ///
    /// The returned `MessageRef` contains the parsed header and payload location.
    /// Use `read_payload()` to access the payload data.
    ///
    /// # Errors
    ///
    /// - `Error::Corrupt`: Invalid header or segment data
    /// - `Error::Io`: Failed to open or read segment
    pub fn next_header(&mut self) -> Result<Option<MessageRef>> {
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
                // End of current segment, advance
                self.advance_segment();
                continue;
            }

            // Check commit_len first (peek without full validation)
            let commit_len = MessageHeader::load_commit_len(&mmap.as_slice()[start] as *const u8);
            if commit_len == 0 {
                // Uncommitted or end of segment, advance
                self.advance_segment();
                continue;
            }

            // Parse payload length
            let payload_len = MessageHeader::payload_len_from_commit(commit_len)?;
            if payload_len > MAX_PAYLOAD_LEN {
                return Err(Error::Corrupt("payload length exceeds max"));
            }

            // Calculate record length
            let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
            if start + record_len > mmap.len() {
                // Corrupted or truncated record
                return Err(Error::Corrupt("record length out of bounds"));
            }

            // Read and parse full header
            let mut header_buf = [0u8; HEADER_SIZE];
            header_buf.copy_from_slice(&mmap.as_slice()[start..start + HEADER_SIZE]);
            let header = MessageHeader::from_bytes(&header_buf)?;

            // Advance offset for next read
            self.offset = self
                .offset
                .checked_add(record_len)
                .ok_or(Error::Corrupt("read offset overflow"))?;

            return Ok(Some(MessageRef {
                seq: header.seq,
                timestamp_ns: header.timestamp_ns,
                type_id: header.type_id,
                payload_offset: start + HEADER_SIZE,
                payload_len,
            }));
        }
    }

    /// Read payload data into the provided buffer.
    ///
    /// # Arguments
    ///
    /// * `offset` - Payload offset (from MessageRef.payload_offset)
    /// * `len` - Payload length (from MessageRef.payload_len)
    /// * `buf` - Buffer to read into (must be at least `len` bytes)
    ///
    /// # Errors
    ///
    /// - `Error::Corrupt`: Offset or length out of bounds
    /// - `Error::Io`: Failed to read from segment
    ///
    /// # Panics
    ///
    /// Panics if `buf.len() < len`.
    pub fn read_payload(&self, offset: usize, len: usize, buf: &mut [u8]) -> Result<()> {
        assert!(buf.len() >= len, "buffer too small for payload");

        let mmap = self
            .current_mmap
            .as_ref()
            .ok_or(Error::Corrupt("no segment loaded"))?;

        if offset + len > mmap.len() {
            return Err(Error::Corrupt("payload out of bounds"));
        }

        buf[..len].copy_from_slice(&mmap.as_slice()[offset..offset + len]);
        Ok(())
    }

    /// Seek to a specific segment and offset.
    ///
    /// # Arguments
    ///
    /// * `segment_id` - Target segment ID
    /// * `offset` - Offset within segment (must be >= SEG_DATA_OFFSET)
    ///
    /// # Errors
    ///
    /// - `Error::Unsupported`: Segment not found in list
    /// - `Error::Io`: Failed to open segment
    pub fn seek_segment(&mut self, segment_id: u64, offset: usize) -> Result<()> {
        // Find segment index
        let idx = self
            .segments
            .iter()
            .position(|&id| id == segment_id)
            .ok_or(Error::Unsupported("segment not found"))?;

        // Update state
        self.current_segment_idx = idx;
        self.current_mmap = None;
        self.offset = offset;

        // Open segment
        self.load_next_segment()?;
        Ok(())
    }

    /// Load the next segment from the list.
    ///
    /// Returns `false` if no more segments are available.
    fn load_next_segment(&mut self) -> Result<bool> {
        if self.current_segment_idx >= self.segments.len() {
            return Ok(false);
        }

        let segment_id = self.segments[self.current_segment_idx];
        let path = segment_path(&self.dir, segment_id);

        if !path.exists() {
            // Skip missing segments (might have been cleaned up)
            self.current_segment_idx += 1;
            self.offset = SEG_DATA_OFFSET;
            return self.load_next_segment();
        }

        let mmap = open_segment(&self.dir, segment_id, self.segment_size)?;
        self.current_mmap = Some(mmap);
        self.offset = SEG_DATA_OFFSET;

        Ok(true)
    }

    /// Advance to the next segment.
    fn advance_segment(&mut self) {
        self.current_segment_idx += 1;
        self.current_mmap = None;
        self.offset = SEG_DATA_OFFSET;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::segment_store::{discover_segments, DEFAULT_SEGMENT_SIZE};
    use crate::core::segment_writer::SegmentWriter;
    use tempfile::TempDir;

    #[test]
    fn test_cursor_basic() {
        let dir = TempDir::new().unwrap();

        // Write some test data
        let mut writer = SegmentWriter::new(dir.path(), 0, DEFAULT_SEGMENT_SIZE);
        writer.append(0x1000, 1000, b"hello").unwrap();
        writer.append(0x1001, 2000, b"world").unwrap();
        writer.finish().unwrap();

        // Read back with cursor
        let segments = discover_segments(dir.path()).unwrap();
        let mut cursor = SegmentCursor::open(dir.path(), segments, DEFAULT_SEGMENT_SIZE);

        // Read first message
        let msg1 = cursor.next_header().unwrap().unwrap();
        assert_eq!(msg1.seq, 0);
        assert_eq!(msg1.timestamp_ns, 1000);
        assert_eq!(msg1.type_id, 0x1000);
        assert_eq!(msg1.payload_len, 5);

        let mut payload1 = vec![0u8; msg1.payload_len];
        cursor
            .read_payload(msg1.payload_offset, msg1.payload_len, &mut payload1)
            .unwrap();
        assert_eq!(&payload1, b"hello");

        // Read second message
        let msg2 = cursor.next_header().unwrap().unwrap();
        assert_eq!(msg2.seq, 1);
        assert_eq!(msg2.timestamp_ns, 2000);
        assert_eq!(msg2.type_id, 0x1001);

        let mut payload2 = vec![0u8; msg2.payload_len];
        cursor
            .read_payload(msg2.payload_offset, msg2.payload_len, &mut payload2)
            .unwrap();
        assert_eq!(&payload2, b"world");

        // End of stream
        assert!(cursor.next_header().unwrap().is_none());
    }

    #[test]
    fn test_cursor_multiple_segments() {
        let dir = TempDir::new().unwrap();
        let segment_size = 8192; // Small for testing

        // Write across multiple segments
        let mut writer = SegmentWriter::new(dir.path(), 0, segment_size);
        let payload = vec![0u8; 1024];
        let record_len = align_up(HEADER_SIZE + payload.len(), RECORD_ALIGN);

        let mut count = 0;
        // Write to fill segment 0
        while !writer.needs_roll(record_len) {
            writer.append(0x1000, 1000 + count, &payload).unwrap();
            count += 1;
        }
        writer.roll().unwrap();

        // Write some to segment 1
        writer.append(0x1001, 2000, b"segment1").unwrap();
        writer.finish().unwrap();

        // Read back with cursor
        let segments = discover_segments(dir.path()).unwrap();
        assert_eq!(segments.len(), 2);

        let mut cursor = SegmentCursor::open(dir.path(), segments, segment_size);

        // Read all messages
        let mut msg_count = 0;
        while let Some(msg) = cursor.next_header().unwrap() {
            msg_count += 1;
            if msg_count == count + 1 {
                // Last message should be in segment 1
                assert_eq!(msg.type_id, 0x1001);
                assert_eq!(cursor.current_segment_id(), Some(1));
            }
        }

        assert_eq!(msg_count, count + 1);
    }

    #[test]
    fn test_cursor_seek_segment() {
        let dir = TempDir::new().unwrap();

        // Write to multiple segments
        let mut writer = SegmentWriter::new(dir.path(), 0, 8192);
        writer.append(0x1000, 1000, b"seg0").unwrap();
        writer.roll().unwrap();
        writer.append(0x1001, 2000, b"seg1").unwrap();
        writer.finish().unwrap();

        // Create cursor
        let segments = discover_segments(dir.path()).unwrap();
        let mut cursor = SegmentCursor::open(dir.path(), segments, 8192);

        // Seek to segment 1
        cursor.seek_segment(1, SEG_DATA_OFFSET).unwrap();
        assert_eq!(cursor.current_segment_id(), Some(1));

        // Read from segment 1
        let msg = cursor.next_header().unwrap().unwrap();
        assert_eq!(msg.type_id, 0x1001);
        assert_eq!(msg.timestamp_ns, 2000);
    }

    #[test]
    fn test_cursor_empty_segments_list() {
        let dir = TempDir::new().unwrap();
        let mut cursor = SegmentCursor::open(dir.path(), vec![], DEFAULT_SEGMENT_SIZE);
        assert!(cursor.next_header().unwrap().is_none());
    }
}
