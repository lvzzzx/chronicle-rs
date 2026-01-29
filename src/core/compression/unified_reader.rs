//! Unified segment reader that transparently handles both hot (.q) and cold (.q.zst) storage.

use std::path::Path;

use crate::core::mmap::MmapFile;
use crate::core::segment_store::segment_path;
use crate::core::{Error, Result};

use super::ZstdBlockReader;

/// Unified segment reader that transparently reads from .q or .q.zst files.
///
/// This reader automatically detects whether a segment is stored in hot (uncompressed .q)
/// or cold (compressed .q.zst) format and uses the appropriate reading strategy.
///
/// # Storage Tiers
///
/// - **Hot storage (.q)**: Memory-mapped uncompressed files for fast random access
/// - **Cold storage (.q.zst)**: Seekable zstd compressed files for space efficiency
///
/// # Usage
///
/// ```rust,ignore
/// use chronicle::core::compression::UnifiedSegmentReader;
///
/// // Opens .q if it exists, falls back to .q.zst
/// let mut reader = UnifiedSegmentReader::open("data_dir", 0)?;
///
/// // Read transparently regardless of storage tier
/// let mut header_buf = [0u8; HEADER_SIZE];
/// reader.read_at(offset, &mut header_buf)?;
/// ```
pub struct UnifiedSegmentReader {
    inner: ReaderInner,
}

enum ReaderInner {
    /// Hot storage: memory-mapped .q file
    Hot {
        mmap: MmapFile,
    },
    /// Cold storage: seekable zstd compressed .q.zst file
    Cold {
        reader: ZstdBlockReader,
        /// Block size used for zstd compression
        block_size: usize,
        /// Current cached block
        cached_block: Option<CachedBlock>,
    },
}

struct CachedBlock {
    /// Uncompressed offset where this block starts
    uncompressed_offset: u64,
    /// Decompressed block data
    data: Vec<u8>,
}

impl UnifiedSegmentReader {
    /// Open a segment, automatically detecting hot or cold storage.
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory containing segment files
    /// * `segment_id` - Segment ID to open
    ///
    /// # Returns
    ///
    /// Returns a reader that works transparently with both .q and .q.zst files.
    ///
    /// # Errors
    ///
    /// Returns error if neither .q nor .q.zst file exists or cannot be opened.
    pub fn open(dir: impl AsRef<Path>, segment_id: u64) -> Result<Self> {
        let dir = dir.as_ref();
        let q_path = segment_path(dir, segment_id);
        let zst_path = q_path.with_extension("q.zst");
        let idx_path = q_path.with_extension("q.zst.idx");

        // Prefer hot storage if available
        if q_path.exists() {
            let mmap = MmapFile::open(&q_path)?;
            Ok(Self {
                inner: ReaderInner::Hot { mmap },
            })
        } else if zst_path.exists() && idx_path.exists() {
            // Use cold storage
            let reader = ZstdBlockReader::open(&zst_path, &idx_path)
                .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

            // Read index to get block size (we can infer from first entry)
            let block_size = 1024 * 1024; // Default 1MB, matches compression default

            Ok(Self {
                inner: ReaderInner::Cold {
                    reader,
                    block_size,
                    cached_block: None,
                },
            })
        } else {
            Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("segment {} not found in {:?}", segment_id, dir),
            )))
        }
    }

    /// Read data at a specific offset.
    ///
    /// This method works transparently with both hot and cold storage.
    /// For cold storage, it caches decompressed blocks to optimize sequential reads.
    ///
    /// # Arguments
    ///
    /// * `offset` - Offset within the segment (as if it were uncompressed)
    /// * `buf` - Buffer to read into
    ///
    /// # Returns
    ///
    /// Number of bytes read (may be less than buf.len() if near end of segment).
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        match &mut self.inner {
            ReaderInner::Hot { mmap } => {
                // Simple mmap read
                let offset_usize = offset as usize;
                if offset_usize >= mmap.len() {
                    return Ok(0);
                }

                let available = mmap.len() - offset_usize;
                let to_read = buf.len().min(available);

                buf[..to_read].copy_from_slice(&mmap.as_slice()[offset_usize..offset_usize + to_read]);
                Ok(to_read)
            }
            ReaderInner::Cold {
                reader,
                block_size,
                cached_block,
            } => {
                // Calculate which block we need
                let block_offset = (offset / *block_size as u64) * *block_size as u64;

                // Check if we have the right block cached
                let need_reload = if let Some(ref cached) = cached_block {
                    cached.uncompressed_offset != block_offset
                } else {
                    true
                };

                // Load block if needed
                if need_reload {
                    let data = reader
                        .read_block_at(block_offset)
                        .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

                    *cached_block = Some(CachedBlock {
                        uncompressed_offset: block_offset,
                        data,
                    });
                }

                // Now read from cached block
                let block_data = &cached_block.as_ref().unwrap().data;
                let offset_in_block = (offset - block_offset) as usize;

                if offset_in_block >= block_data.len() {
                    return Ok(0);
                }

                let available = block_data.len() - offset_in_block;
                let to_read = buf.len().min(available);

                buf[..to_read].copy_from_slice(&block_data[offset_in_block..offset_in_block + to_read]);
                Ok(to_read)
            }
        }
    }

    /// Get a slice of data at a specific offset (hot storage only).
    ///
    /// This is an optimization for hot storage where we can return a direct reference
    /// to the mmap'd data without copying. For cold storage, this method will return None.
    ///
    /// # Returns
    ///
    /// - `Some(&[u8])` if hot storage and range is valid
    /// - `None` if cold storage or range is invalid
    pub fn slice_at(&self, offset: usize, len: usize) -> Option<&[u8]> {
        match &self.inner {
            ReaderInner::Hot { mmap } => {
                if offset + len <= mmap.len() {
                    Some(&mmap.as_slice()[offset..offset + len])
                } else {
                    None
                }
            }
            ReaderInner::Cold { .. } => {
                // Cold storage doesn't support zero-copy slices
                None
            }
        }
    }

    /// Check if this reader is using hot (uncompressed) storage.
    pub fn is_hot(&self) -> bool {
        matches!(self.inner, ReaderInner::Hot { .. })
    }

    /// Check if this reader is using cold (compressed) storage.
    pub fn is_cold(&self) -> bool {
        matches!(self.inner, ReaderInner::Cold { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::segment::prepare_segment_temp;
    use crate::core::segment_store::publish_segment;
    use crate::core::segment_store::segment_temp_path;
    use crate::core::segment::segment_path as seg_path;
    use tempfile::TempDir;

    #[test]
    fn test_unified_reader_hot_segment() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        // Create a hot segment
        let segment_id = 0;
        let segment_size = 1024 * 1024; // 1MB
        let mut mmap = prepare_segment_temp(dir, segment_id, segment_size)?;

        // Write some test data at SEG_DATA_OFFSET
        let test_data = b"Hello, World!";
        let offset = crate::core::segment_store::SEG_DATA_OFFSET;
        mmap.range_mut(offset, test_data.len())?.copy_from_slice(test_data);
        drop(mmap);

        // Publish segment
        let temp_path = segment_temp_path(dir, segment_id);
        let final_path = seg_path(dir, segment_id);
        publish_segment(&temp_path, &final_path)?;

        // Open with UnifiedSegmentReader
        let mut reader = UnifiedSegmentReader::open(dir, segment_id)?;

        // Should be hot storage
        assert!(reader.is_hot());
        assert!(!reader.is_cold());

        // Read the data
        let mut buf = vec![0u8; test_data.len()];
        let read_len = reader.read_at(offset as u64, &mut buf)?;

        assert_eq!(read_len, test_data.len());
        assert_eq!(&buf, test_data);

        Ok(())
    }

    #[test]
    fn test_unified_reader_segment_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        // Try to open non-existent segment
        let result = UnifiedSegmentReader::open(dir, 999);
        assert!(result.is_err());
    }

    #[test]
    fn test_unified_reader_is_hot_is_cold() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        // Create a hot segment
        let segment_id = 0;
        let segment_size = 1024 * 1024;
        let mmap = prepare_segment_temp(dir, segment_id, segment_size)?;
        drop(mmap);

        let temp_path = segment_temp_path(dir, segment_id);
        let final_path = seg_path(dir, segment_id);
        publish_segment(&temp_path, &final_path)?;

        // Open and verify it's hot
        let reader = UnifiedSegmentReader::open(dir, segment_id)?;
        assert!(reader.is_hot());
        assert!(!reader.is_cold());

        Ok(())
    }
}
