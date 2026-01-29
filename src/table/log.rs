//! Time-series log with timestamp-based seeking.
//!
//! Provides fast timestamp-based queries on top of the Log primitive using
//! the existing seek index infrastructure.
//!
//! # Design
//!
//! - **Writer**: Wraps LogWriter and maintains seek index for timestamp lookups
//! - **Reader**: Supports `seek_timestamp()` for approximate seeks within stride
//! - **Index**: Reuses existing `SeekIndexBuilder` (stride=4096 by default)
//!
//! # Performance
//!
//! For a log with 1 billion messages:
//! - Seek complexity: O(log N) with ~8MB read
//! - Range scan: O(M) where M = messages in range
//! - Index overhead: ~1MB per 100M messages
//!
//! # Example
//!
//! ```no_run
//! use chronicle::core::{TimeSeriesWriter, TimeSeriesReader};
//!
//! // Write time-series data
//! let mut writer = TimeSeriesWriter::open("./market_data")?;
//! for tick in ticks {
//!     writer.append(0x01, tick.timestamp_ns, &tick.data)?;
//! }
//! writer.finish()?;
//!
//! // Query time range
//! let mut reader = TimeSeriesReader::open("./market_data")?;
//! reader.seek_timestamp(start_ts)?;
//! while let Some(msg) = reader.next()? {
//!     if msg.timestamp_ns >= end_ts {
//!         break;
//!     }
//!     process(msg);
//! }
//! # Ok::<(), chronicle::core::Error>(())
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::core::header::{MessageHeader, HEADER_SIZE};
use crate::core::log::LogWriter;
use crate::core::seek_index::{
    load_index_entries, load_index_header, SeekIndexBuilder, SeekIndexHeader,
    DEFAULT_INDEX_STRIDE_RECORDS,
};
use crate::core::segment_cursor::SegmentCursor;
use crate::core::segment_store::{discover_segments, DEFAULT_SEGMENT_SIZE, SEG_DATA_OFFSET};
use crate::core::{Error, MessageView, Result};

const DEFAULT_INDEX_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Time-series log writer with automatic timestamp indexing.
///
/// Wraps `LogWriter` and maintains a seek index for efficient timestamp-based queries.
///
/// # Example
///
/// ```no_run
/// use chronicle::core::TimeSeriesWriter;
///
/// let mut writer = TimeSeriesWriter::open("./tslog")?;
/// writer.append(0x01, 1000000, b"event1")?;
/// writer.append(0x01, 2000000, b"event2")?;
/// writer.finish()?;
/// # Ok::<(), chronicle::core::Error>(())
/// ```
pub struct TimeSeriesWriter {
    dir: PathBuf,
    writer: LogWriter,
    index_builder: SeekIndexBuilder,
    index_flush_interval: Duration,
    index_last_flush: Instant,
    segment_size: usize,
}

impl TimeSeriesWriter {
    /// Open a time-series log writer.
    ///
    /// Uses default segment size (128MB) and index stride (4096 records).
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to create directory or open log
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_config(dir, DEFAULT_SEGMENT_SIZE, DEFAULT_INDEX_STRIDE_RECORDS)
    }

    /// Open with custom segment size and index stride.
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory to store log segments
    /// * `segment_size` - Size of each segment in bytes
    /// * `index_stride` - Number of records between index entries
    pub fn open_with_config(
        dir: impl AsRef<Path>,
        segment_size: usize,
        index_stride: u32,
    ) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let writer = LogWriter::open(&dir, segment_size)?;

        let index_builder = SeekIndexBuilder::new(
            0,
            segment_size as u64,
            SEG_DATA_OFFSET as u32,
            index_stride,
        );

        Ok(Self {
            dir,
            writer,
            index_builder,
            index_flush_interval: DEFAULT_INDEX_FLUSH_INTERVAL,
            index_last_flush: Instant::now(),
            segment_size,
        })
    }

    /// Set index flush interval.
    ///
    /// Index is flushed periodically to disk. Default is 1 second.
    pub fn set_index_flush_interval(&mut self, interval: Duration) {
        self.index_flush_interval = interval;
    }

    /// Append a record with timestamp.
    ///
    /// Timestamps should be monotonically increasing for best seek performance,
    /// but out-of-order writes are supported.
    ///
    /// # Errors
    ///
    /// - `Error::PayloadTooLarge`: Payload exceeds maximum size
    /// - `Error::Io`: Failed to write to segment
    pub fn append(&mut self, type_id: u16, timestamp_ns: u64, payload: &[u8]) -> Result<()> {
        // Get position before append
        let seq = self.writer.seq();
        let offset = self.writer.write_offset();
        let segment_id = self.writer.segment_id();

        // Check if we'll roll to new segment
        let record_len = crate::core::header::RECORD_ALIGN
            + ((HEADER_SIZE + payload.len() + crate::core::header::RECORD_ALIGN - 1)
                / crate::core::header::RECORD_ALIGN
                * crate::core::header::RECORD_ALIGN
                - crate::core::header::RECORD_ALIGN);
        let will_roll = (offset as usize) + record_len > self.segment_size;

        if will_roll {
            // Flush current segment's index before rolling
            self.index_builder.flush(&self.dir)?;
            self.index_last_flush = Instant::now();
        }

        // Append to log
        self.writer.append(type_id, timestamp_ns, payload)?;

        if will_roll {
            // Reset index builder for new segment
            let new_segment_id = self.writer.segment_id();
            self.index_builder
                .reset(new_segment_id, self.segment_size as u64);
        }

        // Update index with (seq, timestamp_ns, offset)
        let actual_segment_id = self.writer.segment_id();
        if actual_segment_id != segment_id {
            // We rolled, index entry should be for new segment
            let new_offset = self.writer.write_offset() - record_len as u64;
            self.index_builder.observe(seq, timestamp_ns, new_offset);
        } else {
            self.index_builder.observe(seq, timestamp_ns, offset);
        }

        // Periodic index flush
        if self.index_last_flush.elapsed() >= self.index_flush_interval {
            self.flush_index()?;
        }

        Ok(())
    }

    /// Flush index to disk.
    pub fn flush_index(&mut self) -> Result<()> {
        self.index_builder.flush(&self.dir)?;
        self.index_last_flush = Instant::now();
        Ok(())
    }

    /// Flush pending writes to disk asynchronously.
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()
    }

    /// Finish writing and seal the log.
    ///
    /// Flushes the final index and closes the log.
    pub fn finish(&mut self) -> Result<()> {
        self.flush_index()?;
        self.writer.finish()
    }

    /// Get the current segment ID.
    pub fn segment_id(&self) -> u64 {
        self.writer.segment_id()
    }

    /// Get the current write offset within the segment.
    pub fn write_offset(&self) -> u64 {
        self.writer.write_offset()
    }

    /// Get the current sequence number.
    pub fn seq(&self) -> u64 {
        self.writer.seq()
    }

    /// Get the number of segments written.
    pub fn segments_written(&self) -> u64 {
        self.writer.segments_written()
    }
}

/// Result of a timestamp seek operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekResult {
    /// Successfully seeked to target timestamp or later.
    Found,
    /// Target timestamp is after the end of the log.
    EndOfLog,
    /// Target timestamp is before the start of the log.
    BeforeStart,
}

/// Time-series log reader with timestamp-based seeking.
///
/// Provides efficient timestamp seeks using the seek index, followed by
/// linear scans within the index stride.
///
/// # Example
///
/// ```no_run
/// use chronicle::core::TimeSeriesReader;
///
/// let mut reader = TimeSeriesReader::open("./tslog")?;
/// reader.seek_timestamp(1500000)?;
/// while let Some(msg) = reader.next()? {
///     println!("ts={}", msg.timestamp_ns);
/// }
/// # Ok::<(), chronicle::core::Error>(())
/// ```
pub struct TimeSeriesReader {
    dir: PathBuf,
    cursor: SegmentCursor,
    segments: Vec<u64>,
    current_segment_idx: usize,
    payload_buf: Vec<u8>,
    #[allow(dead_code)] // Reserved for future use
    segment_size: usize,
}

impl TimeSeriesReader {
    /// Open a time-series log reader.
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to read directory or open segments
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();

        if !dir.exists() {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("time-series log directory not found: {}", dir.display()),
            )));
        }

        let mut segments = discover_segments(&dir)?;
        segments.sort_unstable();

        // Discover segment size from first segment
        let segment_size = if !segments.is_empty() {
            use crate::core::segment_store::segment_path;
            let first_segment_path = segment_path(&dir, segments[0]);
            let metadata = std::fs::metadata(&first_segment_path)?;
            metadata.len() as usize
        } else {
            DEFAULT_SEGMENT_SIZE
        };

        let cursor = SegmentCursor::open(&dir, segments.clone(), segment_size);

        Ok(Self {
            dir,
            cursor,
            segments,
            current_segment_idx: 0,
            payload_buf: vec![0u8; 8192],
            segment_size,
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

    /// Seek to the first message at or after the target timestamp.
    ///
    /// Uses the seek index for approximate positioning (within stride),
    /// then performs a linear scan to find the exact timestamp.
    ///
    /// # Errors
    ///
    /// - `Error::Io`: Failed to read index files
    /// - `Error::Corrupt`: Invalid index data
    pub fn seek_timestamp(&mut self, target_ts_ns: u64) -> Result<SeekResult> {
        if self.segments.is_empty() {
            return Ok(SeekResult::EndOfLog);
        }

        // Step 1: Find segment containing target timestamp
        let mut target_segment: Option<(u64, SeekIndexHeader)> = None;
        let mut before_first = false;

        for &segment_id in &self.segments {
            let Some(header) = load_index_header(&self.dir, segment_id)? else {
                // No index for this segment, assume it spans infinity
                target_segment = Some((segment_id, create_dummy_header(segment_id)));
                break;
            };

            if target_ts_ns < header.min_ts_ns {
                // Target is before this segment
                if target_segment.is_none() {
                    // First segment, and target is before it
                    before_first = true;
                    target_segment = Some((segment_id, header));
                }
                break;
            }

            if target_ts_ns <= header.max_ts_ns {
                // Target is within this segment!
                target_segment = Some((segment_id, header));
                break;
            }

            // Target is after this segment, keep searching
            target_segment = Some((segment_id, header));
        }

        let Some((segment_id, header)) = target_segment else {
            return Ok(SeekResult::EndOfLog);
        };

        if before_first {
            // Target is before the first segment, position at start
            self.cursor.seek_segment(segment_id, SEG_DATA_OFFSET)?;
            self.current_segment_idx = 0;
            return Ok(SeekResult::BeforeStart);
        }

        // Step 2: Load index entries and binary search
        let entries = if header.entry_count > 0 {
            load_index_entries(&self.dir, &header)?
        } else {
            Vec::new()
        };

        if entries.is_empty() {
            // No index entries, seek to segment start
            self.cursor.seek_segment(segment_id, SEG_DATA_OFFSET)?;
            self.current_segment_idx = self
                .segments
                .iter()
                .position(|&id| id == segment_id)
                .unwrap_or(0);
            return self.linear_scan_to_timestamp(target_ts_ns);
        }

        // Binary search for closest entry before or at target
        let idx = match entries.binary_search_by_key(&target_ts_ns, |e| e.timestamp_ns) {
            Ok(i) => i, // Exact match
            Err(0) => 0, // Target is before first entry
            Err(i) => i - 1, // Start one entry before
        };

        let entry = &entries[idx];

        // Step 3: Seek to indexed offset
        self.cursor.seek_segment(segment_id, entry.offset as usize)?;
        self.current_segment_idx = self
            .segments
            .iter()
            .position(|&id| id == segment_id)
            .unwrap_or(0);

        // Step 4: Linear scan to exact timestamp
        self.linear_scan_to_timestamp(target_ts_ns)
    }

    /// Linear scan from current position to find target timestamp.
    fn linear_scan_to_timestamp(&mut self, target_ts_ns: u64) -> Result<SeekResult> {
        loop {
            let Some(msg_ref) = self.cursor.next_header()? else {
                // Reached end of log without finding timestamp
                return Ok(SeekResult::EndOfLog);
            };

            if msg_ref.timestamp_ns >= target_ts_ns {
                // Found a message at or after target!
                // Need to rewind cursor to before this message
                let current_segment = self.cursor.current_segment_id().unwrap();
                let rewind_offset = msg_ref.payload_offset - HEADER_SIZE;
                self.cursor.seek_segment(current_segment, rewind_offset)?;
                return Ok(SeekResult::Found);
            }
        }
    }

    /// Returns the list of segment IDs in this log.
    pub fn segments(&self) -> &[u64] {
        &self.segments
    }

    /// Seek to a specific segment.
    pub fn seek_segment(&mut self, segment_id: u64) -> Result<()> {
        self.cursor.seek_segment(segment_id, SEG_DATA_OFFSET)
    }
}

/// Create a dummy header when index is missing.
fn create_dummy_header(segment_id: u64) -> SeekIndexHeader {
    SeekIndexHeader {
        magic: crate::core::seek_index::INDEX_MAGIC,
        version: crate::core::seek_index::INDEX_VERSION,
        header_len: crate::core::seek_index::INDEX_HEADER_LEN,
        flags: 0,
        segment_id,
        segment_size: DEFAULT_SEGMENT_SIZE as u64,
        data_offset: SEG_DATA_OFFSET as u32,
        entry_stride: DEFAULT_INDEX_STRIDE_RECORDS,
        min_seq: 0,
        max_seq: u64::MAX,
        min_ts_ns: 0,
        max_ts_ns: u64::MAX,
        entry_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_timeseries_write_read_basic() {
        let dir = TempDir::new().unwrap();

        // Write some time-series data
        let mut writer = TimeSeriesWriter::open(dir.path()).unwrap();
        writer.append(0x01, 1000, b"first").unwrap();
        writer.append(0x02, 2000, b"second").unwrap();
        writer.append(0x03, 3000, b"third").unwrap();
        writer.finish().unwrap();

        // Read back
        let mut reader = TimeSeriesReader::open(dir.path()).unwrap();

        let msg1 = reader.next().unwrap().unwrap();
        assert_eq!(msg1.timestamp_ns, 1000);
        assert_eq!(msg1.payload, b"first");

        let msg2 = reader.next().unwrap().unwrap();
        assert_eq!(msg2.timestamp_ns, 2000);

        let msg3 = reader.next().unwrap().unwrap();
        assert_eq!(msg3.timestamp_ns, 3000);

        assert!(reader.next().unwrap().is_none());
    }

    #[test]
    fn test_seek_exact_timestamp() {
        let dir = TempDir::new().unwrap();

        // Write messages with timestamps 1000, 2000, 3000, ..., 10000
        let mut writer = TimeSeriesWriter::open_with_config(dir.path(), 1024 * 1024, 2).unwrap();
        for i in 1..=10 {
            let ts = i * 1000;
            let data = format!("msg{}", i);
            writer.append(0x01, ts, data.as_bytes()).unwrap();
        }
        writer.finish().unwrap();

        // Seek to exact timestamp 5000
        let mut reader = TimeSeriesReader::open(dir.path()).unwrap();
        let result = reader.seek_timestamp(5000).unwrap();
        assert_eq!(result, SeekResult::Found);

        let msg = reader.next().unwrap().unwrap();
        assert_eq!(msg.timestamp_ns, 5000);
        assert_eq!(msg.payload, b"msg5");
    }

    #[test]
    fn test_seek_between_timestamps() {
        let dir = TempDir::new().unwrap();

        // Write messages at timestamps 1000, 3000, 5000
        let mut writer = TimeSeriesWriter::open(dir.path()).unwrap();
        writer.append(0x01, 1000, b"msg1").unwrap();
        writer.append(0x01, 3000, b"msg2").unwrap();
        writer.append(0x01, 5000, b"msg3").unwrap();
        writer.finish().unwrap();

        // Seek to 2500 (between 1000 and 3000)
        let mut reader = TimeSeriesReader::open(dir.path()).unwrap();
        let result = reader.seek_timestamp(2500).unwrap();
        assert_eq!(result, SeekResult::Found);

        // Should get msg at 3000 (first >= 2500)
        let msg = reader.next().unwrap().unwrap();
        assert_eq!(msg.timestamp_ns, 3000);
        assert_eq!(msg.payload, b"msg2");
    }

    #[test]
    fn test_seek_before_start() {
        let dir = TempDir::new().unwrap();

        // Earliest message is at timestamp 10000
        let mut writer = TimeSeriesWriter::open(dir.path()).unwrap();
        writer.append(0x01, 10000, b"first").unwrap();
        writer.append(0x01, 20000, b"second").unwrap();
        writer.finish().unwrap();

        // Seek to 5000 (before start)
        let mut reader = TimeSeriesReader::open(dir.path()).unwrap();
        let result = reader.seek_timestamp(5000).unwrap();
        assert_eq!(result, SeekResult::BeforeStart);

        // Should still be positioned at first message
        let msg = reader.next().unwrap().unwrap();
        assert_eq!(msg.timestamp_ns, 10000);
    }

    #[test]
    fn test_seek_after_end() {
        let dir = TempDir::new().unwrap();

        // Latest message is at timestamp 10000
        let mut writer = TimeSeriesWriter::open(dir.path()).unwrap();
        writer.append(0x01, 5000, b"first").unwrap();
        writer.append(0x01, 10000, b"last").unwrap();
        writer.finish().unwrap();

        // Seek to 20000 (after end)
        let mut reader = TimeSeriesReader::open(dir.path()).unwrap();
        let result = reader.seek_timestamp(20000).unwrap();
        assert_eq!(result, SeekResult::EndOfLog);

        // Should be at end
        assert!(reader.next().unwrap().is_none());
    }

    #[test]
    fn test_multiple_segments() {
        let dir = TempDir::new().unwrap();

        // Write across multiple segments with small segment size
        let segment_size = 8192;
        let mut writer =
            TimeSeriesWriter::open_with_config(dir.path(), segment_size, 10).unwrap();

        let payload = vec![0u8; 512];
        for i in 0..50 {
            let ts = i * 1000;
            writer.append(0x01, ts, &payload).unwrap();
        }
        writer.finish().unwrap();

        assert!(writer.segments_written() > 1);

        // Seek to middle timestamp
        let mut reader = TimeSeriesReader::open(dir.path()).unwrap();
        let result = reader.seek_timestamp(25000).unwrap();
        assert_eq!(result, SeekResult::Found);

        let msg = reader.next().unwrap().unwrap();
        assert_eq!(msg.timestamp_ns, 25000);
    }

    #[test]
    fn test_time_range_scan() {
        let dir = TempDir::new().unwrap();

        // Write 100 messages with timestamps 0, 1000, 2000, ..., 99000
        let mut writer = TimeSeriesWriter::open(dir.path()).unwrap();
        for i in 0..100 {
            let ts = i * 1000;
            let data = format!("msg{}", i);
            writer.append(0x01, ts, data.as_bytes()).unwrap();
        }
        writer.finish().unwrap();

        // Scan range [30000, 40000)
        let mut reader = TimeSeriesReader::open(dir.path()).unwrap();
        reader.seek_timestamp(30000).unwrap();

        let mut count = 0;
        while let Some(msg) = reader.next().unwrap() {
            if msg.timestamp_ns >= 40000 {
                break;
            }
            assert!(msg.timestamp_ns >= 30000);
            assert!(msg.timestamp_ns < 40000);
            count += 1;
        }

        assert_eq!(count, 10); // Messages 30-39
    }

    #[test]
    fn test_empty_log() {
        let dir = TempDir::new().unwrap();

        let mut writer = TimeSeriesWriter::open(dir.path()).unwrap();
        writer.finish().unwrap();

        let mut reader = TimeSeriesReader::open(dir.path()).unwrap();
        assert!(reader.next().unwrap().is_none());

        let result = reader.seek_timestamp(1000).unwrap();
        assert_eq!(result, SeekResult::EndOfLog);
    }

    #[test]
    fn test_out_of_order_writes() {
        let dir = TempDir::new().unwrap();

        // Write messages with out-of-order timestamps
        let mut writer = TimeSeriesWriter::open(dir.path()).unwrap();
        writer.append(0x01, 1000, b"msg1").unwrap();
        writer.append(0x01, 3000, b"msg3").unwrap();
        writer.append(0x01, 2000, b"msg2").unwrap(); // Out of order!
        writer.append(0x01, 4000, b"msg4").unwrap();
        writer.finish().unwrap();

        // Seek should still work (finds first >= target)
        let mut reader = TimeSeriesReader::open(dir.path()).unwrap();
        reader.seek_timestamp(2500).unwrap();

        // Should find msg3 (ts=3000, first >= 2500)
        let msg = reader.next().unwrap().unwrap();
        assert_eq!(msg.timestamp_ns, 3000);
        assert_eq!(msg.payload, b"msg3");
    }
}
