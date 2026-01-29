//! Atomic segment compression operations.

use std::path::Path;

use crate::core::compression::{compress_q_to_zst, ZstdBlockReader};
use crate::core::Result;

/// Atomically compress a segment from .q to .q.zst format.
///
/// This operation is atomic: the .q file is only removed after successful
/// compression and verification.
///
/// # Arguments
///
/// * `q_path` - Path to the .q file
/// * `block_size` - Zstd block size for seekable compression
/// * `compression_level` - Zstd compression level (1-22)
///
/// # Returns
///
/// Size of the compressed file in bytes
///
/// # Errors
///
/// Returns error if:
/// - Compression fails
/// - Verification fails
/// - File operations fail
pub fn compress_segment(
    q_path: &Path,
    block_size: usize,
    _compression_level: i32,
) -> Result<u64> {
    let zst_path = q_path.with_extension("q.zst");
    let idx_path = q_path.with_extension("q.zst.idx");
    let zst_tmp = q_path.with_extension("q.zst.tmp");
    let idx_tmp = q_path.with_extension("q.zst.idx.tmp");

    // Clean up any leftover tmp files
    let _ = std::fs::remove_file(&zst_tmp);
    let _ = std::fs::remove_file(&idx_tmp);

    // Compress to temporary files
    compress_q_to_zst(q_path, &zst_tmp, &idx_tmp, block_size)
        .map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("compression failed: {}", e),
            )
        })?;

    // Verify compressed file is readable
    verify_compressed_file(&zst_tmp, &idx_tmp)?;

    // Get compressed size
    let compressed_size = std::fs::metadata(&zst_tmp)?.len();

    // Atomically rename tmp files to final
    std::fs::rename(&zst_tmp, &zst_path)?;
    std::fs::rename(&idx_tmp, &idx_path)?;

    // Only remove original after successful compression and verification
    std::fs::remove_file(q_path)?;

    Ok(compressed_size)
}

/// Verify that a compressed file is readable.
fn verify_compressed_file(zst_path: &Path, idx_path: &Path) -> Result<()> {
    // Try to open the compressed file
    let mut reader = ZstdBlockReader::open(zst_path, idx_path).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("verification failed: {}", e),
        )
    })?;

    // Try to read first block
    reader.read_block_at(0).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("verification read failed: {}", e),
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::segment::{prepare_segment_temp, publish_segment, segment_temp_path, segment_path as seg_path};
    use crate::core::segment_store::SEG_DATA_OFFSET;
    use tempfile::TempDir;

    #[test]
    fn test_compress_segment_atomic() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        // Create a segment
        let segment_id = 0;
        let segment_size = 1024 * 1024; // 1MB
        let mut mmap = prepare_segment_temp(dir, segment_id, segment_size)?;

        // Write test data
        let test_data = b"Hello, World! ".repeat(1000);
        mmap.range_mut(SEG_DATA_OFFSET, test_data.len())?
            .copy_from_slice(&test_data);
        drop(mmap);

        // Publish segment
        let temp_path = segment_temp_path(dir, segment_id);
        let q_path = seg_path(dir, segment_id);
        publish_segment(&temp_path, &q_path)?;

        // Compress
        let compressed_size = compress_segment(&q_path, 64 * 1024, 3)?;

        // Verify
        assert!(compressed_size > 0);
        assert!(!q_path.exists(), ".q file should be removed");
        assert!(q_path.with_extension("q.zst").exists());
        assert!(q_path.with_extension("q.zst.idx").exists());

        Ok(())
    }

    #[test]
    fn test_compress_segment_cleans_up_tmp() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        // Create a segment
        let segment_id = 0;
        let segment_size = 1024 * 1024;
        let mut mmap = prepare_segment_temp(dir, segment_id, segment_size)?;

        // Write test data
        let test_data = b"Test";
        mmap.range_mut(SEG_DATA_OFFSET, test_data.len())?
            .copy_from_slice(test_data);
        drop(mmap);

        let temp_path = segment_temp_path(dir, segment_id);
        let q_path = seg_path(dir, segment_id);
        publish_segment(&temp_path, &q_path)?;

        // Create leftover tmp files
        std::fs::write(q_path.with_extension("q.zst.tmp"), b"old")?;
        std::fs::write(q_path.with_extension("q.zst.idx.tmp"), b"old")?;

        // Compress
        compress_segment(&q_path, 64 * 1024, 3)?;

        // Verify tmp files are gone
        assert!(!q_path.with_extension("q.zst.tmp").exists());
        assert!(!q_path.with_extension("q.zst.idx.tmp").exists());

        Ok(())
    }
}
