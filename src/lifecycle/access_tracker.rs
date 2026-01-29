//! Segment access tracking for lifecycle management.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::core::Result;

/// Tracks segment access times for compression policy decisions.
///
/// Persists access data to `.access_log` files in the segment directory.
#[derive(Debug)]
pub struct AccessTracker {
    /// Directory containing segments.
    dir: PathBuf,

    /// In-memory cache of access times (segment_id → last_access_time).
    cache: HashMap<u64, SystemTime>,

    /// Whether tracking is enabled.
    enabled: bool,
}

impl AccessTracker {
    /// Create a new access tracker.
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory containing segments
    /// * `enabled` - Whether to enable access tracking
    pub fn new(dir: impl Into<PathBuf>, enabled: bool) -> Result<Self> {
        let dir = dir.into();
        let mut cache = HashMap::new();

        if enabled {
            // Load existing access log
            if let Ok(log) = Self::load_log(&dir) {
                cache = log;
            }
        }

        Ok(Self {
            dir,
            cache,
            enabled,
        })
    }

    /// Record a segment access.
    pub fn record_access(&mut self, segment_id: u64) {
        if !self.enabled {
            return;
        }

        self.cache.insert(segment_id, SystemTime::now());
    }

    /// Get last access time for a segment.
    pub fn last_access(&self, segment_id: u64) -> Option<SystemTime> {
        if !self.enabled {
            return None;
        }

        self.cache.get(&segment_id).copied()
    }

    /// Get read count (simplified - just 0 or 1+ for now).
    pub fn read_count(&self, segment_id: u64) -> u64 {
        if self.cache.contains_key(&segment_id) {
            1
        } else {
            0
        }
    }

    /// Persist access log to disk.
    pub fn flush(&self) -> Result<()> {
        if !self.enabled || self.cache.is_empty() {
            return Ok(());
        }

        let log_path = self.dir.join(".access_log");
        let tmp_path = self.dir.join(".access_log.tmp");

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;

        for (segment_id, access_time) in &self.cache {
            let timestamp = access_time
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            writeln!(file, "{},{}", segment_id, timestamp)?;
        }

        file.sync_all()?;
        std::fs::rename(tmp_path, log_path)?;

        Ok(())
    }

    /// Load access log from disk.
    fn load_log(dir: &Path) -> Result<HashMap<u64, SystemTime>> {
        let log_path = dir.join(".access_log");

        if !log_path.exists() {
            return Ok(HashMap::new());
        }

        let file = File::open(&log_path)?;
        let reader = BufReader::new(file);
        let mut cache = HashMap::new();

        for line in reader.lines() {
            let line = line?;
            let parts: Vec<&str> = line.split(',').collect();

            if parts.len() != 2 {
                continue;
            }

            if let (Ok(segment_id), Ok(timestamp)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>()) {
                let access_time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(timestamp);
                cache.insert(segment_id, access_time);
            }
        }

        Ok(cache)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_access_tracker_disabled() {
        let temp_dir = TempDir::new().unwrap();
        let mut tracker = AccessTracker::new(temp_dir.path(), false).unwrap();

        tracker.record_access(0);
        assert_eq!(tracker.last_access(0), None);
    }

    #[test]
    fn test_access_tracker_record_and_retrieve() {
        let temp_dir = TempDir::new().unwrap();
        let mut tracker = AccessTracker::new(temp_dir.path(), true).unwrap();

        let before = SystemTime::now();
        tracker.record_access(0);
        let after = SystemTime::now();

        let access_time = tracker.last_access(0).unwrap();
        assert!(access_time >= before && access_time <= after);
    }

    #[test]
    fn test_access_tracker_flush_and_load() -> Result<()> {
        let temp_dir = TempDir::new().unwrap();

        // Create tracker, record access, flush
        {
            let mut tracker = AccessTracker::new(temp_dir.path(), true)?;
            tracker.record_access(0);
            tracker.record_access(1);
            tracker.flush()?;
        }

        // Load in new tracker
        {
            let tracker = AccessTracker::new(temp_dir.path(), true)?;
            assert!(tracker.last_access(0).is_some());
            assert!(tracker.last_access(1).is_some());
            assert!(tracker.last_access(2).is_none());
        }

        Ok(())
    }

    #[test]
    fn test_read_count() {
        let temp_dir = TempDir::new().unwrap();
        let mut tracker = AccessTracker::new(temp_dir.path(), true).unwrap();

        assert_eq!(tracker.read_count(0), 0);
        tracker.record_access(0);
        assert_eq!(tracker.read_count(0), 1);
    }
}
