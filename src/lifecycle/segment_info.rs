//! Segment information for lifecycle management.

use std::path::PathBuf;
use std::time::SystemTime;

/// Information about a segment for lifecycle decisions.
#[derive(Debug, Clone)]
pub struct SegmentInfo {
    /// Full path to the segment file (.q or .q.zst).
    pub path: PathBuf,

    /// Segment ID.
    pub segment_id: u64,

    /// Size in bytes.
    pub size_bytes: u64,

    /// Creation time.
    pub created_at: SystemTime,

    /// Last modification time.
    pub modified_at: SystemTime,

    /// Last access time (if tracked).
    pub last_accessed_at: Option<SystemTime>,

    /// Whether the segment is sealed (not actively being written).
    pub is_sealed: bool,

    /// Number of times the segment has been read (if tracked).
    pub read_count: u64,
}

impl SegmentInfo {
    /// Calculate age since creation.
    pub fn age(&self) -> std::time::Duration {
        SystemTime::now()
            .duration_since(self.created_at)
            .unwrap_or(std::time::Duration::ZERO)
    }

    /// Calculate time since last modification.
    pub fn time_since_modified(&self) -> std::time::Duration {
        SystemTime::now()
            .duration_since(self.modified_at)
            .unwrap_or(std::time::Duration::ZERO)
    }

    /// Calculate time since last access (if tracked).
    pub fn time_since_accessed(&self) -> Option<std::time::Duration> {
        self.last_accessed_at.and_then(|accessed| {
            SystemTime::now()
                .duration_since(accessed)
                .ok()
        })
    }

    /// Check if segment is idle (no access for given duration).
    pub fn is_idle(&self, duration: std::time::Duration) -> bool {
        if let Some(idle_time) = self.time_since_accessed() {
            idle_time >= duration
        } else {
            // If no access tracking, fall back to modification time
            self.time_since_modified() >= duration
        }
    }

    /// Check if segment is compressed (.q.zst).
    pub fn is_compressed(&self) -> bool {
        self.path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e == "zst")
            .unwrap_or(false)
    }

    /// Estimate compression savings (assumes ~50% compression ratio).
    pub fn compression_savings(&self) -> u64 {
        if self.is_compressed() {
            self.size_bytes / 2 // Already compressed, assume original was 2x
        } else {
            self.size_bytes / 2 // Not compressed, estimate 50% savings
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_segment_info_age() {
        let info = SegmentInfo {
            path: "/data/000000000.q".into(),
            segment_id: 0,
            size_bytes: 1024,
            created_at: SystemTime::now() - Duration::from_secs(3600),
            modified_at: SystemTime::now(),
            last_accessed_at: None,
            is_sealed: true,
            read_count: 0,
        };

        let age = info.age();
        assert!(age.as_secs() >= 3599 && age.as_secs() <= 3601);
    }

    #[test]
    fn test_segment_info_is_idle() {
        let mut info = SegmentInfo {
            path: "/data/000000000.q".into(),
            segment_id: 0,
            size_bytes: 1024,
            created_at: SystemTime::now(),
            modified_at: SystemTime::now() - Duration::from_secs(86400 * 8), // 8 days ago
            last_accessed_at: Some(SystemTime::now() - Duration::from_secs(86400 * 8)),
            is_sealed: true,
            read_count: 0,
        };

        assert!(info.is_idle(Duration::from_secs(86400 * 7))); // 7 days
        assert!(!info.is_idle(Duration::from_secs(86400 * 9))); // 9 days

        // Test fallback to modification time when no access tracking
        info.last_accessed_at = None;
        assert!(info.is_idle(Duration::from_secs(86400 * 7)));
    }

    #[test]
    fn test_segment_info_is_compressed() {
        let hot = SegmentInfo {
            path: "/data/000000000.q".into(),
            segment_id: 0,
            size_bytes: 1024,
            created_at: SystemTime::now(),
            modified_at: SystemTime::now(),
            last_accessed_at: None,
            is_sealed: true,
            read_count: 0,
        };

        let cold = SegmentInfo {
            path: "/data/000000000.q.zst".into(),
            segment_id: 0,
            size_bytes: 512,
            created_at: SystemTime::now(),
            modified_at: SystemTime::now(),
            last_accessed_at: None,
            is_sealed: true,
            read_count: 0,
        };

        assert!(!hot.is_compressed());
        assert!(cold.is_compressed());
    }

    #[test]
    fn test_compression_savings() {
        let info = SegmentInfo {
            path: "/data/000000000.q".into(),
            segment_id: 0,
            size_bytes: 1000,
            created_at: SystemTime::now(),
            modified_at: SystemTime::now(),
            last_accessed_at: None,
            is_sealed: true,
            read_count: 0,
        };

        assert_eq!(info.compression_savings(), 500);
    }
}
