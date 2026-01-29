//! Compression policy evaluation logic.

use std::time::Duration;

use crate::lifecycle::SegmentInfo;
use crate::table::CompressionPolicy;

/// Evaluate if a segment should be compressed based on policy.
///
/// # Arguments
///
/// * `policy` - The compression policy to evaluate
/// * `info` - Information about the segment
///
/// # Returns
///
/// `true` if the segment should be compressed, `false` otherwise
pub fn should_compress(policy: &CompressionPolicy, info: &SegmentInfo) -> bool {
    // Never compress if not sealed
    if !info.is_sealed {
        return false;
    }

    // Already compressed?
    if info.is_compressed() {
        return false;
    }

    match policy {
        CompressionPolicy::Never => false,

        CompressionPolicy::IdleAfter { days, track_reads } => {
            let idle_duration = Duration::from_secs((*days as u64) * 86400);

            if *track_reads {
                // Use access tracking if available
                if let Some(idle_time) = info.time_since_accessed() {
                    idle_time >= idle_duration
                } else {
                    // Fall back to modification time if access tracking unavailable
                    info.time_since_modified() >= idle_duration
                }
            } else {
                // Just check modification time
                info.time_since_modified() >= idle_duration
            }
        }

        CompressionPolicy::AgeAfter { days } => {
            let age_duration = Duration::from_secs((*days as u64) * 86400);
            info.age() >= age_duration
        }

        CompressionPolicy::Immediate => {
            // Compress as soon as sealed
            true
        }

        CompressionPolicy::SizeThreshold { bytes } => {
            info.size_bytes >= *bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn make_segment(
        age_days: u64,
        modified_days: u64,
        accessed_days: Option<u64>,
        sealed: bool,
        compressed: bool,
        size_bytes: u64,
    ) -> SegmentInfo {
        let now = SystemTime::now();
        let path = if compressed {
            PathBuf::from("/data/000000000.q.zst")
        } else {
            PathBuf::from("/data/000000000.q")
        };

        SegmentInfo {
            path,
            segment_id: 0,
            size_bytes,
            created_at: now - Duration::from_secs(age_days * 86400),
            modified_at: now - Duration::from_secs(modified_days * 86400),
            last_accessed_at: accessed_days.map(|d| now - Duration::from_secs(d * 86400)),
            is_sealed: sealed,
            read_count: if accessed_days.is_some() { 1 } else { 0 },
        }
    }

    #[test]
    fn test_policy_never() {
        let policy = CompressionPolicy::Never;
        let info = make_segment(10, 10, None, true, false, 1000);

        assert!(!should_compress(&policy, &info));
    }

    #[test]
    fn test_policy_immediate() {
        let policy = CompressionPolicy::Immediate;
        let info_sealed = make_segment(0, 0, None, true, false, 1000);
        let info_unsealed = make_segment(0, 0, None, false, false, 1000);

        assert!(should_compress(&policy, &info_sealed));
        assert!(!should_compress(&policy, &info_unsealed));
    }

    #[test]
    fn test_policy_idle_after_with_tracking() {
        let policy = CompressionPolicy::IdleAfter {
            days: 7,
            track_reads: true,
        };

        // Accessed 8 days ago - should compress
        let info_idle = make_segment(10, 10, Some(8), true, false, 1000);
        assert!(should_compress(&policy, &info_idle));

        // Accessed 6 days ago - should not compress
        let info_active = make_segment(10, 10, Some(6), true, false, 1000);
        assert!(!should_compress(&policy, &info_active));

        // No access tracking, falls back to modification time
        let info_no_tracking = make_segment(10, 8, None, true, false, 1000);
        assert!(should_compress(&policy, &info_no_tracking));
    }

    #[test]
    fn test_policy_idle_after_without_tracking() {
        let policy = CompressionPolicy::IdleAfter {
            days: 7,
            track_reads: false,
        };

        // Modified 8 days ago - should compress
        let info_old = make_segment(10, 8, Some(1), true, false, 1000);
        assert!(should_compress(&policy, &info_old));

        // Modified 6 days ago - should not compress
        let info_new = make_segment(10, 6, Some(10), true, false, 1000);
        assert!(!should_compress(&policy, &info_new));
    }

    #[test]
    fn test_policy_age_after() {
        let policy = CompressionPolicy::AgeAfter { days: 7 };

        // Created 8 days ago - should compress
        let info_old = make_segment(8, 1, Some(1), true, false, 1000);
        assert!(should_compress(&policy, &info_old));

        // Created 6 days ago - should not compress
        let info_new = make_segment(6, 10, Some(10), true, false, 1000);
        assert!(!should_compress(&policy, &info_new));
    }

    #[test]
    fn test_policy_size_threshold() {
        let policy = CompressionPolicy::SizeThreshold {
            bytes: 100 * 1024 * 1024, // 100MB
        };

        // 200MB segment - should compress
        let info_large = make_segment(1, 1, None, true, false, 200 * 1024 * 1024);
        assert!(should_compress(&policy, &info_large));

        // 50MB segment - should not compress
        let info_small = make_segment(1, 1, None, true, false, 50 * 1024 * 1024);
        assert!(!should_compress(&policy, &info_small));
    }

    #[test]
    fn test_unsealed_never_compressed() {
        let policies = vec![
            CompressionPolicy::Immediate,
            CompressionPolicy::IdleAfter {
                days: 0,
                track_reads: true,
            },
            CompressionPolicy::AgeAfter { days: 0 },
            CompressionPolicy::SizeThreshold { bytes: 0 },
        ];

        for policy in policies {
            let info = make_segment(100, 100, Some(100), false, false, 1000000);
            assert!(!should_compress(&policy, &info));
        }
    }

    #[test]
    fn test_already_compressed_skipped() {
        let policy = CompressionPolicy::Immediate;
        let info = make_segment(0, 0, None, true, true, 1000);

        assert!(!should_compress(&policy, &info));
    }
}
