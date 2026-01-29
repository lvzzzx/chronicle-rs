//! Storage lifecycle manager.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use crate::core::segment::{read_segment_flags, SEG_FLAG_SEALED};
use crate::core::Result;
use crate::lifecycle::{
    compress_segment, should_compress, AccessTracker, LifecycleStats, SegmentInfo,
};
use crate::table::CompressionPolicy;

/// Configuration for lifecycle management.
#[derive(Debug, Clone)]
pub struct LifecycleConfig {
    /// Compression policy to apply.
    pub policy: CompressionPolicy,

    /// Zstd compression level (1-22).
    pub compression_level: i32,

    /// Block size for seekable zstd compression.
    pub block_size: usize,

    /// Number of parallel compression workers (not yet implemented).
    pub parallel_workers: usize,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            policy: CompressionPolicy::default(),
            compression_level: 3,
            block_size: 1024 * 1024,
            parallel_workers: 1,
        }
    }
}

/// Manages segment lifecycle (compression, archival, etc.).
pub struct StorageLifecycleManager {
    /// Root directory to manage.
    root: PathBuf,

    /// Lifecycle configuration.
    config: LifecycleConfig,

    /// Access tracker.
    access_tracker: AccessTracker,
}

impl StorageLifecycleManager {
    /// Create a new lifecycle manager.
    ///
    /// # Arguments
    ///
    /// * `root` - Root directory containing segments
    /// * `config` - Lifecycle configuration
    pub fn new(root: impl Into<PathBuf>, config: LifecycleConfig) -> Result<Self> {
        let root = root.into();

        // Determine if access tracking is needed
        let track_access = matches!(
            config.policy,
            CompressionPolicy::IdleAfter { track_reads: true, .. }
        );

        let access_tracker = AccessTracker::new(&root, track_access)?;

        Ok(Self {
            root,
            config,
            access_tracker,
        })
    }

    /// Run lifecycle management once.
    ///
    /// Scans all segments, evaluates policies, and compresses eligible segments.
    pub fn run_once(&mut self) -> Result<LifecycleStats> {
        let start = Instant::now();
        let mut stats = LifecycleStats::new();

        // Discover all .q files recursively
        let mut q_files = Vec::new();
        self.collect_q_files(&self.root, &mut q_files)?;

        stats.scanned_count = q_files.len();

        // Process each segment
        for q_path in q_files {
            match self.process_segment(&q_path, &mut stats) {
                Ok(_) => {}
                Err(e) => {
                    stats.record_error(format!("{}: {}", q_path.display(), e));
                }
            }
        }

        // Flush access tracker
        self.access_tracker.flush()?;

        stats.duration = start.elapsed();
        Ok(stats)
    }

    /// Process a single segment.
    fn process_segment(&mut self, q_path: &Path, stats: &mut LifecycleStats) -> Result<()> {
        // Get segment info
        let info = self.get_segment_info(q_path)?;

        // Evaluate policy
        if !should_compress(&self.config.policy, &info) {
            return Ok(());
        }

        // Compress
        let original_size = info.size_bytes;
        let compressed_size = compress_segment(
            q_path,
            self.config.block_size,
            self.config.compression_level,
        )?;

        stats.record_compression(original_size, compressed_size);

        Ok(())
    }

    /// Get segment information.
    fn get_segment_info(&self, q_path: &Path) -> Result<SegmentInfo> {
        let metadata = std::fs::metadata(q_path)?;

        // Extract segment ID from filename
        let segment_id = q_path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        // Check if sealed
        let is_sealed = read_segment_flags(q_path)
            .map(|flags| (flags & SEG_FLAG_SEALED) != 0)
            .unwrap_or(false);

        // Get access info
        let last_accessed_at = self.access_tracker.last_access(segment_id);
        let read_count = self.access_tracker.read_count(segment_id);

        Ok(SegmentInfo {
            path: q_path.to_path_buf(),
            segment_id,
            size_bytes: metadata.len(),
            created_at: metadata.created().unwrap_or(SystemTime::now()),
            modified_at: metadata.modified().unwrap_or(SystemTime::now()),
            last_accessed_at,
            is_sealed,
            read_count,
        })
    }

    /// Recursively collect .q files.
    fn collect_q_files(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if entry.file_type()?.is_dir() {
                self.collect_q_files(&path, files)?;
                continue;
            }

            // Check if this is a .q file (not .q.tmp, .q.zst)
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(".q") && !name.ends_with(".q.tmp") {
                    files.push(path);
                }
            }
        }

        Ok(())
    }

    /// Compress a specific partition directory.
    pub fn compress_partition(&mut self, partition_path: &Path) -> Result<LifecycleStats> {
        let start = Instant::now();
        let mut stats = LifecycleStats::new();

        let mut q_files = Vec::new();
        self.collect_q_files(partition_path, &mut q_files)?;

        stats.scanned_count = q_files.len();

        for q_path in q_files {
            match self.process_segment(&q_path, &mut stats) {
                Ok(_) => {}
                Err(e) => {
                    stats.record_error(format!("{}: {}", q_path.display(), e));
                }
            }
        }

        self.access_tracker.flush()?;

        stats.duration = start.elapsed();
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::segment::{prepare_segment_temp, publish_segment, seal_segment, segment_temp_path, segment_path as seg_path};
    use crate::core::segment_store::SEG_DATA_OFFSET;
    use tempfile::TempDir;

    #[test]
    fn test_lifecycle_manager_immediate_policy() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        // Create a sealed segment
        let segment_id = 0;
        let segment_size = 1024 * 1024;
        let mut mmap = prepare_segment_temp(dir, segment_id, segment_size)?;

        // Write test data
        let test_data = b"Hello, World! ".repeat(100);
        mmap.range_mut(SEG_DATA_OFFSET, test_data.len())?
            .copy_from_slice(&test_data);

        seal_segment(&mut mmap)?;
        drop(mmap);

        let temp_path = segment_temp_path(dir, segment_id);
        let q_path = seg_path(dir, segment_id);
        publish_segment(&temp_path, &q_path)?;

        // Create manager with immediate compression
        let config = LifecycleConfig {
            policy: CompressionPolicy::Immediate,
            ..Default::default()
        };

        let mut manager = StorageLifecycleManager::new(dir, config)?;

        // Run once
        let stats = manager.run_once()?;

        // Verify
        assert_eq!(stats.scanned_count, 1);
        assert_eq!(stats.compressed_count, 1);
        assert!(stats.bytes_saved > 0);
        assert!(!stats.has_errors());

        // Check compressed file exists
        assert!(q_path.with_extension("q.zst").exists());
        assert!(!q_path.exists());

        Ok(())
    }

    #[test]
    fn test_lifecycle_manager_never_policy() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();

        // Create a sealed segment
        let segment_id = 0;
        let segment_size = 1024 * 1024;
        let mut mmap = prepare_segment_temp(dir, segment_id, segment_size)?;
        seal_segment(&mut mmap)?;
        drop(mmap);

        let temp_path = segment_temp_path(dir, segment_id);
        let q_path = seg_path(dir, segment_id);
        publish_segment(&temp_path, &q_path)?;

        // Create manager with Never policy
        let config = LifecycleConfig {
            policy: CompressionPolicy::Never,
            ..Default::default()
        };

        let mut manager = StorageLifecycleManager::new(dir, config)?;

        // Run once
        let stats = manager.run_once()?;

        // Verify - should not compress
        assert_eq!(stats.scanned_count, 1);
        assert_eq!(stats.compressed_count, 0);
        assert_eq!(stats.bytes_saved, 0);

        // Original file should still exist
        assert!(q_path.exists());
        assert!(!q_path.with_extension("q.zst").exists());

        Ok(())
    }
}
