//! Statistics for lifecycle operations.

use std::time::Duration;

/// Statistics from a lifecycle management run.
#[derive(Debug, Clone, Default)]
pub struct LifecycleStats {
    /// Number of segments scanned.
    pub scanned_count: usize,

    /// Number of segments compressed.
    pub compressed_count: usize,

    /// Total bytes saved by compression.
    pub bytes_saved: u64,

    /// Number of errors encountered.
    pub error_count: usize,

    /// Errors encountered during the run.
    pub errors: Vec<String>,

    /// Time taken for the run.
    pub duration: Duration,
}

impl LifecycleStats {
    /// Create new empty stats.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful compression.
    pub fn record_compression(&mut self, original_size: u64, compressed_size: u64) {
        self.compressed_count += 1;
        self.bytes_saved += original_size.saturating_sub(compressed_size);
    }

    /// Record an error.
    pub fn record_error(&mut self, error: String) {
        self.error_count += 1;
        self.errors.push(error);
    }

    /// Get compression ratio (0.0 to 1.0).
    pub fn compression_ratio(&self) -> f64 {
        if self.compressed_count == 0 {
            0.0
        } else {
            self.bytes_saved as f64 / (self.bytes_saved as f64 + self.compressed_count as f64 * 1024.0)
        }
    }

    /// Check if any errors occurred.
    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    /// Get summary string.
    pub fn summary(&self) -> String {
        format!(
            "Scanned: {}, Compressed: {}, Saved: {} bytes, Errors: {}, Duration: {:?}",
            self.scanned_count,
            self.compressed_count,
            self.bytes_saved,
            self.error_count,
            self.duration
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lifecycle_stats_default() {
        let stats = LifecycleStats::default();
        assert_eq!(stats.scanned_count, 0);
        assert_eq!(stats.compressed_count, 0);
        assert_eq!(stats.bytes_saved, 0);
        assert_eq!(stats.error_count, 0);
        assert!(!stats.has_errors());
    }

    #[test]
    fn test_record_compression() {
        let mut stats = LifecycleStats::new();

        stats.record_compression(1000, 500);
        assert_eq!(stats.compressed_count, 1);
        assert_eq!(stats.bytes_saved, 500);

        stats.record_compression(2000, 1000);
        assert_eq!(stats.compressed_count, 2);
        assert_eq!(stats.bytes_saved, 1500);
    }

    #[test]
    fn test_record_error() {
        let mut stats = LifecycleStats::new();

        stats.record_error("test error".to_string());
        assert_eq!(stats.error_count, 1);
        assert!(stats.has_errors());
        assert_eq!(stats.errors.len(), 1);
    }

    #[test]
    fn test_summary() {
        let mut stats = LifecycleStats::new();
        stats.scanned_count = 10;
        stats.compressed_count = 5;
        stats.bytes_saved = 1000;
        stats.duration = Duration::from_secs(10);

        let summary = stats.summary();
        assert!(summary.contains("Scanned: 10"));
        assert!(summary.contains("Compressed: 5"));
        assert!(summary.contains("Saved: 1000 bytes"));
    }
}
