//! Table configuration.
//!
//! Defines configuration options for tables including segment size, indexing, and compression.

use serde::{Deserialize, Serialize};

/// Configuration for a table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableConfig {
    /// Segment size in bytes.
    /// Default: 64 MB
    pub segment_size: usize,

    /// Index stride (number of records between index entries).
    /// Default: 100
    pub index_stride: u32,

    /// Validate partition order (prevent backwards rolling).
    /// Default: true
    pub validate_partition_order: bool,

    /// Compression policy for this table.
    /// Default: Compress after 7 days of no reads
    #[serde(default)]
    pub compression_policy: CompressionPolicy,

    /// Zstd compression level (1-22).
    /// Default: 3 (balanced speed/ratio)
    pub compression_level: i32,

    /// Block size for seekable zstd compression.
    /// Default: 1 MB
    pub compression_block_size: usize,

    /// Track segment access times for smarter compression.
    /// Default: true
    pub track_access: bool,
}

impl Default for TableConfig {
    fn default() -> Self {
        Self {
            segment_size: 64 * 1024 * 1024, // 64 MB
            index_stride: 100,
            validate_partition_order: true,
            compression_policy: CompressionPolicy::IdleAfter {
                days: 7,
                track_reads: true,
            },
            compression_level: 3,
            compression_block_size: 1024 * 1024, // 1 MB
            track_access: true,
        }
    }
}

/// Compression policy determining when segments are compressed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CompressionPolicy {
    /// Never compress (always keep hot).
    Never,

    /// Compress after N days of no access (idle data).
    /// Best for IPC: compress old/idle data only.
    IdleAfter {
        /// Number of days without reads before compressing
        days: u32,
        /// Track read access times (requires access tracking)
        track_reads: bool,
    },

    /// Compress after age, regardless of reads.
    /// Best for time-series: compress yesterday's data.
    AgeAfter {
        /// Number of days after creation before compressing
        days: u32,
    },

    /// Compress immediately after segment is sealed.
    /// Best for archival use cases.
    Immediate,

    /// Compress when segment size exceeds threshold.
    SizeThreshold {
        /// Threshold in bytes
        bytes: u64,
    },
}

impl Default for CompressionPolicy {
    fn default() -> Self {
        CompressionPolicy::IdleAfter {
            days: 7,
            track_reads: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_config_default() {
        let config = TableConfig::default();
        assert_eq!(config.segment_size, 64 * 1024 * 1024);
        assert_eq!(config.index_stride, 100);
        assert_eq!(config.validate_partition_order, true);
        assert_eq!(config.compression_level, 3);
        assert_eq!(config.compression_block_size, 1024 * 1024);
        assert_eq!(config.track_access, true);
    }

    #[test]
    fn test_compression_policy_default() {
        let policy = CompressionPolicy::default();
        match policy {
            CompressionPolicy::IdleAfter { days, track_reads } => {
                assert_eq!(days, 7);
                assert_eq!(track_reads, true);
            }
            _ => panic!("Expected IdleAfter policy"),
        }
    }

    #[test]
    fn test_table_config_serialization() {
        let config = TableConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: TableConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(config.segment_size, deserialized.segment_size);
        assert_eq!(config.index_stride, deserialized.index_stride);
    }

    #[test]
    fn test_compression_policy_never() {
        let policy = CompressionPolicy::Never;
        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: CompressionPolicy = serde_json::from_str(&json).unwrap();

        matches!(deserialized, CompressionPolicy::Never);
    }

    #[test]
    fn test_compression_policy_immediate() {
        let policy = CompressionPolicy::Immediate;
        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: CompressionPolicy = serde_json::from_str(&json).unwrap();

        matches!(deserialized, CompressionPolicy::Immediate);
    }
}
