use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::segment::load_reader_meta;
use crate::{Error, Result};

const READER_TTL_NS: u64 = 30_000_000_000;
const MAX_RETENTION_LAG: u64 = 10 * 1024 * 1024 * 1024;

pub fn cleanup_segments(root: &Path, head_segment: u64, head_offset: u64) -> Result<Vec<u64>> {
    let readers_dir = root.join("readers");
    if !readers_dir.exists() {
        return Ok(Vec::new());
    }

    let min_segment = min_live_reader_segment(root, head_segment, head_offset)?;

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

pub fn min_live_reader_segment(root: &Path, head_segment: u64, head_offset: u64) -> Result<u64> {
    let readers_dir = root.join("readers");
    if !readers_dir.exists() {
        return Ok(head_segment);
    }

    let head = head_segment
        .saturating_mul(crate::segment::SEGMENT_SIZE as u64)
        .saturating_add(head_offset);
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Unsupported("system time before UNIX epoch"))?
        .as_nanos();
    let now_ns = u64::try_from(now_ns)
        .map_err(|_| Error::Unsupported("system time exceeds timestamp range"))?;

    let mut min_segment: Option<u64> = None;
    for entry in fs::read_dir(&readers_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("meta") {
            continue;
        }
        let meta = load_reader_meta(&path)?;
        if meta.last_heartbeat_ns != 0
            && now_ns.saturating_sub(meta.last_heartbeat_ns) > READER_TTL_NS
        {
            continue;
        }
        let reader_global = meta
            .segment_id
            .saturating_mul(crate::segment::SEGMENT_SIZE as u64)
            .saturating_add(meta.offset);
        if head > reader_global && head - reader_global > MAX_RETENTION_LAG {
            continue;
        }
        min_segment = Some(match min_segment {
            Some(current) => current.min(meta.segment_id),
            None => meta.segment_id,
        });
    }

    Ok(min_segment.unwrap_or(head_segment))
}

pub fn min_live_reader_position(root: &Path, head_segment: u64, head_offset: u64) -> Result<u64> {
    let readers_dir = root.join("readers");
    let head = head_segment
        .saturating_mul(crate::segment::SEGMENT_SIZE as u64)
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

    let mut min_pos: Option<u64> = None;
    for entry in fs::read_dir(&readers_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("meta") {
            continue;
        }
        let meta = load_reader_meta(&path)?;
        if meta.last_heartbeat_ns != 0
            && now_ns.saturating_sub(meta.last_heartbeat_ns) > READER_TTL_NS
        {
            continue;
        }
        let reader_global = meta
            .segment_id
            .saturating_mul(crate::segment::SEGMENT_SIZE as u64)
            .saturating_add(meta.offset);
        if head > reader_global && head - reader_global > MAX_RETENTION_LAG {
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
    stem.parse::<u64>().map_err(|_| Error::Corrupt("invalid segment id")).ok()
}
