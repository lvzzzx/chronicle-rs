//! Core segment writing primitive.
//!
//! Provides low-level append and roll operations for segment-based storage.
//! Used as a building block for both Queue and Log implementations.
//!
//! # Design
//!
//! - Manages a single active segment (mmap)
//! - Writes MessageHeader + payload in Chronicle format
//! - Handles segment rolling when capacity is reached
//! - Seals and publishes segments atomically
//! - No coordination metadata (writer locks, control files, retention, etc.)

use std::path::PathBuf;

use crate::core::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, RECORD_ALIGN};
use crate::core::mmap::MmapFile;
use crate::core::segment_store::{
    prepare_segment_temp, publish_segment, seal_segment, segment_path, segment_temp_path,
    SEG_DATA_OFFSET,
};
use crate::core::{Error, Result};

/// Aligns a value up to the nearest multiple of `align`.
#[inline]
fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

/// Core segment writer for appending records and rolling segments.
///
/// This is a low-level primitive that both `QueueWriter` and `LogWriter` build upon.
/// It manages:
/// - Writing records (MessageHeader + payload)
/// - Sequence number generation
/// - Segment rolling when full
/// - Sealing and publishing completed segments
///
/// It does NOT manage:
/// - Writer locks or coordination
/// - Control files
/// - Retention or cleanup
/// - Reader tracking
/// - Seek indices
/// - Backpressure
///
/// # Usage Pattern
///
/// ```text
/// 1. Create writer with new()
/// 2. Loop:
///    - append() records
///    - Automatic roll when segment fills
/// 3. finish() to seal final segment
/// ```
pub struct SegmentWriter {
    /// Directory containing segments
    dir: PathBuf,
    /// Current segment ID
    segment_id: u64,
    /// Segment size in bytes
    segment_size: usize,
    /// Current write offset within segment
    write_offset: u64,
    /// Current sequence number
    seq: u64,
    /// Current segment mmap (None if not yet initialized)
    mmap: Option<MmapFile>,
    /// Number of segments published
    segments_published: u64,
    /// Whether current segment has records
    has_records: bool,
}

impl SegmentWriter {
    /// Create a new segment writer.
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory to store segments
    /// * `segment_id` - Starting segment ID
    /// * `segment_size` - Segment size in bytes
    ///
    /// The segment is created lazily on first append.
    pub fn new(dir: impl Into<PathBuf>, segment_id: u64, segment_size: usize) -> Self {
        Self {
            dir: dir.into(),
            segment_id,
            segment_size,
            write_offset: SEG_DATA_OFFSET as u64,
            seq: 0,
            mmap: None,
            segments_published: 0,
            has_records: false,
        }
    }

    /// Get current segment ID.
    pub fn segment_id(&self) -> u64 {
        self.segment_id
    }

    /// Get current write offset within segment.
    pub fn write_offset(&self) -> u64 {
        self.write_offset
    }

    /// Get current sequence number.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Get number of segments published.
    pub fn segments_published(&self) -> u64 {
        self.segments_published
    }

    /// Check if the current segment has any records written.
    pub fn has_records(&self) -> bool {
        self.has_records
    }

    /// Check if a record of given length would require rolling.
    ///
    /// Useful for preemptive rolling decisions.
    pub fn needs_roll(&self, record_len: usize) -> bool {
        (self.write_offset as usize) + record_len > self.segment_size
    }

    /// Append a record to the current segment.
    ///
    /// Automatically ensures a segment is open. The caller should check
    /// `needs_roll()` and call `roll()` before appending if needed.
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
    /// - `Error::Corrupt`: Internal state error
    /// - `Error::Io`: I/O error during write
    ///
    /// # Panics
    ///
    /// Panics if the record would exceed segment size. Callers should check
    /// `needs_roll()` and call `roll()` first.
    pub fn append(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()> {
        let payload_len = payload.len();
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(Error::PayloadTooLarge);
        }

        let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
        let max_payload = self.segment_size.saturating_sub(SEG_DATA_OFFSET);
        if record_len > max_payload {
            return Err(Error::PayloadTooLarge);
        }

        // Ensure we have a segment open
        self.ensure_segment()?;

        // Caller should handle rolling before append
        debug_assert!(
            !self.needs_roll(record_len),
            "append called when roll needed"
        );

        let offset = self.write_offset as usize;
        let mmap = self
            .mmap
            .as_mut()
            .ok_or(Error::Corrupt("segment mmap missing"))?;

        // Write payload first
        if payload_len > 0 {
            mmap.range_mut(offset + HEADER_SIZE, payload_len)?
                .copy_from_slice(payload);
        }

        // Calculate checksum and write header
        let checksum = MessageHeader::crc32(payload);
        let header = MessageHeader::new_uncommitted(self.seq, timestamp_ns, type_id, 0, checksum);
        let header_bytes = header.to_bytes();
        mmap.range_mut(offset, HEADER_SIZE)?
            .copy_from_slice(&header_bytes);

        // Commit the record atomically
        let commit_len = MessageHeader::commit_len_for_payload(payload_len)?;
        let header_ptr = unsafe { mmap.as_mut_slice().as_mut_ptr().add(offset) };
        MessageHeader::store_commit_len(header_ptr, commit_len);

        // Update state
        self.seq = self.seq.wrapping_add(1);
        self.write_offset = self
            .write_offset
            .checked_add(record_len as u64)
            .ok_or(Error::Corrupt("write offset overflow"))?;
        self.has_records = true;

        Ok(())
    }

    /// Roll to a new segment.
    ///
    /// Seals and publishes the current segment (if it has records), then prepares
    /// a new segment for writing.
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to seal, publish, or create new segment
    pub fn roll(&mut self) -> Result<()> {
        // Seal and publish current segment if it has records
        if self.has_records {
            self.seal_and_publish_current()?;
        } else if let Some(mmap) = self.mmap.take() {
            // Discard empty temp segment
            drop(mmap);
            let temp_path = segment_temp_path(&self.dir, self.segment_id);
            let _ = std::fs::remove_file(temp_path);
        }

        // Move to next segment
        self.segment_id += 1;
        self.write_offset = SEG_DATA_OFFSET as u64;
        self.has_records = false;
        self.mmap = None;

        Ok(())
    }

    /// Seal and publish the current segment.
    ///
    /// This makes the segment immutable and visible to readers.
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to seal or publish segment
    fn seal_and_publish_current(&mut self) -> Result<()> {
        if let Some(mut mmap) = self.mmap.take() {
            // Seal the segment (mark as immutable)
            seal_segment(&mut mmap)?;

            // Sync to disk
            mmap.flush_sync()?;

            // Drop mmap before rename (releases file handle)
            drop(mmap);

            // Atomically publish (rename .tmp → final)
            let temp_path = segment_temp_path(&self.dir, self.segment_id);
            let final_path = segment_path(&self.dir, self.segment_id);
            publish_segment(&temp_path, &final_path)?;

            self.segments_published += 1;
        }

        Ok(())
    }

    /// Flush pending writes to disk asynchronously.
    ///
    /// Ensures writes are visible but not necessarily durable.
    pub fn flush(&mut self) -> Result<()> {
        if let Some(mmap) = &mut self.mmap {
            mmap.flush_async()?;
        }
        Ok(())
    }

    /// Finish writing and seal the current segment.
    ///
    /// Should be called when done writing to properly close the writer.
    /// Empty segments are discarded.
    pub fn finish(&mut self) -> Result<()> {
        if !self.has_records {
            // Clean up empty temp segment
            if let Some(mmap) = self.mmap.take() {
                drop(mmap);
                let temp_path = segment_temp_path(&self.dir, self.segment_id);
                let _ = std::fs::remove_file(temp_path);
            }
            return Ok(());
        }

        self.seal_and_publish_current()
    }

    /// Ensure a segment is open for writing.
    ///
    /// Creates a temp segment if none exists.
    fn ensure_segment(&mut self) -> Result<()> {
        if self.mmap.is_some() {
            return Ok(());
        }

        let mmap = prepare_segment_temp(&self.dir, self.segment_id, self.segment_size)?;
        self.write_offset = SEG_DATA_OFFSET as u64;
        self.has_records = false;
        self.mmap = Some(mmap);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::segment_store::discover_segments;
    use tempfile::TempDir;

    #[test]
    fn test_segment_writer_basic() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new(dir.path(), 0, 1024 * 1024);

        // Append a record
        writer.append(0x1000, 1000, b"hello").unwrap();
        assert_eq!(writer.seq(), 1);
        assert!(writer.has_records());

        // Finish
        writer.finish().unwrap();
        assert_eq!(writer.segments_published(), 1);

        // Verify segment exists
        let segments = discover_segments(dir.path()).unwrap();
        assert_eq!(segments, vec![0]);
    }

    #[test]
    fn test_segment_writer_empty_finish() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new(dir.path(), 0, 1024 * 1024);

        // Finish without writing
        writer.finish().unwrap();
        assert_eq!(writer.segments_published(), 0);

        // No segments should exist
        let segments = discover_segments(dir.path()).unwrap();
        assert!(segments.is_empty());
    }

    #[test]
    fn test_segment_writer_roll() {
        let dir = TempDir::new().unwrap();
        let segment_size = 8192; // Small segment for testing
        let mut writer = SegmentWriter::new(dir.path(), 0, segment_size);

        // Write records until roll needed
        let payload = vec![0u8; 1024];
        let record_len = align_up(HEADER_SIZE + payload.len(), RECORD_ALIGN);

        let mut count = 0;
        while !writer.needs_roll(record_len) {
            writer.append(0x1000, 1000 + count, &payload).unwrap();
            count += 1;
        }

        assert!(count > 0);
        assert_eq!(writer.segment_id(), 0);

        // Roll to next segment
        writer.roll().unwrap();
        assert_eq!(writer.segment_id(), 1);
        assert_eq!(writer.write_offset(), SEG_DATA_OFFSET as u64);
        assert!(!writer.has_records());

        // Write to new segment
        writer.append(0x1000, 2000, b"new segment").unwrap();
        assert!(writer.has_records());

        writer.finish().unwrap();

        // Verify both segments published
        assert_eq!(writer.segments_published(), 2);
        let segments = discover_segments(dir.path()).unwrap();
        assert_eq!(segments, vec![0, 1]);
    }

    #[test]
    fn test_segment_writer_sequence() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new(dir.path(), 0, 1024 * 1024);

        for i in 0..10 {
            writer.append(0x1000, 1000 + i, b"test").unwrap();
            assert_eq!(writer.seq(), i + 1);
        }

        writer.finish().unwrap();
    }

    #[test]
    fn test_segment_writer_payload_too_large() {
        let dir = TempDir::new().unwrap();
        let mut writer = SegmentWriter::new(dir.path(), 0, 1024 * 1024);

        let large_payload = vec![0u8; MAX_PAYLOAD_LEN + 1];
        let err = writer.append(0x1000, 1000, &large_payload).unwrap_err();
        assert!(matches!(err, Error::PayloadTooLarge));
    }

    #[test]
    fn test_segment_writer_multiple_rolls() {
        let dir = TempDir::new().unwrap();
        let segment_size = 4096; // Very small for testing
        let mut writer = SegmentWriter::new(dir.path(), 10, segment_size);

        let payload = vec![0u8; 512];
        let record_len = align_up(HEADER_SIZE + payload.len(), RECORD_ALIGN);

        // Write across 3 segments
        for _ in 0..3 {
            while !writer.needs_roll(record_len) {
                writer.append(0x1000, 1000, &payload).unwrap();
            }
            writer.roll().unwrap();
        }

        writer.finish().unwrap();

        // Should have segments 10, 11, 12 (13 is rolled to but empty)
        let segments = discover_segments(dir.path()).unwrap();
        assert_eq!(segments, vec![10, 11, 12]);
    }
}
