use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::core::segment::load_reader_meta;
use crate::core::{Error, Result};

/// Default reader TTL: 30 seconds
const DEFAULT_READER_TTL: Duration = Duration::from_secs(30);
/// Default max retention lag: 10GB
const DEFAULT_MAX_RETENTION_LAG: u64 = 10 * 1024 * 1024 * 1024;

/// Configuration for segment retention and cleanup.
#[derive(Debug, Clone, Copy)]
pub struct RetentionConfig {
    /// Time-to-live for reader heartbeats. Readers that haven't updated their
    /// heartbeat within this duration are considered inactive and won't block
    /// segment cleanup.
    pub reader_ttl: Duration,
    /// Maximum lag (in bytes) a reader can fall behind the writer before it's
    /// considered too slow and won't block segment cleanup.
    pub max_reader_lag: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            reader_ttl: DEFAULT_READER_TTL,
            max_reader_lag: DEFAULT_MAX_RETENTION_LAG,
        }
    }
}

pub fn cleanup_segments(
    root: &Path,
    head_segment: u64,
    head_offset: u64,
    segment_size: u64,
    config: &RetentionConfig,
) -> Result<Vec<u64>> {
    let readers_dir = root.join("readers");
    if !readers_dir.exists() {
        return Ok(Vec::new());
    }

    let min_segment = min_live_reader_segment(root, head_segment, head_offset, segment_size, config)?;

    let mut deleted = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("q") {
            continue;
        }
        let name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name,
            None => continue,
        };
        let id = match parse_segment_id(name) {
            Some(id) => id,
            None => continue,
        };
        if id < min_segment && id < head_segment {
            fs::remove_file(&path)?;
            deleted.push(id);
        }
    }

    deleted.sort_unstable();
    Ok(deleted)
}

pub fn retention_candidates(
    root: &Path,
    head_segment: u64,
    head_offset: u64,
    segment_size: u64,
    config: &RetentionConfig,
) -> Result<Vec<u64>> {
    let readers_dir = root.join("readers");
    if !readers_dir.exists() {
        return Ok(Vec::new());
    }

    let min_segment = min_live_reader_segment(root, head_segment, head_offset, segment_size, config)?;

    let mut candidates = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("q") {
            continue;
        }
        let name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name,
            None => continue,
        };
        let id = match parse_segment_id(name) {
            Some(id) => id,
            None => continue,
        };
        if id < min_segment && id < head_segment {
            candidates.push(id);
        }
    }

    candidates.sort_unstable();
    Ok(candidates)
}

pub fn min_live_reader_segment(
    root: &Path,
    head_segment: u64,
    head_offset: u64,
    segment_size: u64,
    config: &RetentionConfig,
) -> Result<u64> {
    let readers_dir = root.join("readers");
    if !readers_dir.exists() {
        return Ok(head_segment);
    }

    let head = head_segment
        .saturating_mul(segment_size)
        .saturating_add(head_offset);
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Unsupported("system time before UNIX epoch"))?
        .as_nanos();
    let now_ns = u64::try_from(now_ns)
        .map_err(|_| Error::Unsupported("system time exceeds timestamp range"))?;
    let reader_ttl_ns = config.reader_ttl.as_nanos() as u64;

    let mut min_segment: Option<u64> = None;
    for entry in fs::read_dir(&readers_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("meta") {
            continue;
        }
        let meta = load_reader_meta(&path)?;
        if meta.last_heartbeat_ns != 0
            && now_ns.saturating_sub(meta.last_heartbeat_ns) > reader_ttl_ns
        {
            continue;
        }
        let reader_global = meta
            .segment_id
            .saturating_mul(segment_size)
            .saturating_add(meta.offset);
        if head > reader_global && head - reader_global > config.max_reader_lag {
            continue;
        }
        min_segment = Some(match min_segment {
            Some(current) => current.min(meta.segment_id),
            None => meta.segment_id,
        });
    }

    Ok(min_segment.unwrap_or(head_segment))
}

pub fn min_live_reader_position(
    root: &Path,
    head_segment: u64,
    head_offset: u64,
    segment_size: u64,
    config: &RetentionConfig,
) -> Result<u64> {
    let readers_dir = root.join("readers");
    let head = head_segment
        .saturating_mul(segment_size)
        .saturating_add(head_offset);
    if !readers_dir.exists() {
        return Ok(head);
    }

    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Unsupported("system time before UNIX epoch"))?
        .as_nanos();
    let now_ns = u64::try_from(now_ns)
        .map_err(|_| Error::Unsupported("system time exceeds timestamp range"))?;
    let reader_ttl_ns = config.reader_ttl.as_nanos() as u64;

    let mut min_pos: Option<u64> = None;
    for entry in fs::read_dir(&readers_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("meta") {
            continue;
        }
        let meta = load_reader_meta(&path)?;
        if meta.last_heartbeat_ns != 0
            && now_ns.saturating_sub(meta.last_heartbeat_ns) > reader_ttl_ns
        {
            continue;
        }
        let reader_global = meta
            .segment_id
            .saturating_mul(segment_size)
            .saturating_add(meta.offset);
        if head > reader_global && head - reader_global > config.max_reader_lag {
            continue;
        }
        min_pos = Some(match min_pos {
            Some(current) => current.min(reader_global),
            None => reader_global,
        });
    }

    Ok(min_pos.unwrap_or(head))
}

fn parse_segment_id(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".q")?;
    if stem.is_empty() {
        return None;
    }
    stem.parse::<u64>()
        .map_err(|_| Error::Corrupt("invalid segment id"))
        .ok()
}
