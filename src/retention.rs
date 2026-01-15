use std::fs;
use std::path::Path;

use crate::segment::load_reader_position;
use crate::{Error, Result};

pub fn cleanup_segments(root: &Path, current_segment: u64) -> Result<Vec<u64>> {
    let readers_dir = root.join("readers");
    if !readers_dir.exists() {
        return Ok(Vec::new());
    }

    let mut min_segment: Option<u64> = None;
    for entry in fs::read_dir(&readers_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("meta") {
            continue;
        }
        let position = load_reader_position(&path)?;
        min_segment = Some(match min_segment {
            Some(current) => current.min(position.segment_id),
            None => position.segment_id,
        });
    }

    let min_segment = match min_segment {
        Some(value) => value,
        None => return Ok(Vec::new()),
    };

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
        if id < min_segment && id < current_segment {
            fs::remove_file(&path)?;
            deleted.push(id);
        }
    }

    deleted.sort_unstable();
    Ok(deleted)
}

fn parse_segment_id(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".q")?;
    if stem.is_empty() {
        return None;
    }
    stem.parse::<u64>().map_err(|_| Error::Corrupt("invalid segment id")).ok()
}
