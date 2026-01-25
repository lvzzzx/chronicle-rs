use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::core::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, RECORD_ALIGN};
use crate::core::segment::{SEG_DATA_OFFSET, SEG_MAGIC, SEG_VERSION};

#[derive(Debug, Serialize, Deserialize)]
pub struct MetaFile {
    pub symbol_code: Option<String>,
    pub venue: Option<String>,
    pub ingest_time_ns: u64,
    pub event_time_range: Option<[u64; 2]>,
    pub completeness: String,
}

pub fn read_segment_flags(path: &Path) -> Result<u32> {
    let mut file = File::open(path)?;
    let mut buf = [0u8; 64];
    file.read_exact(&mut buf)?;
    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice length"));
    let version = u32::from_le_bytes(buf[4..8].try_into().expect("slice length"));
    let flags = u32::from_le_bytes(buf[12..16].try_into().expect("slice length"));
    if magic != SEG_MAGIC {
        return Err(anyhow!("segment magic mismatch in {}", path.display()));
    }
    if version != SEG_VERSION {
        return Err(anyhow!("segment version mismatch in {}", path.display()));
    }
    Ok(flags)
}

pub fn scan_segment_timestamps(path: &Path) -> Result<Option<[u64; 2]>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len() as usize;
    if len <= SEG_DATA_OFFSET {
        return Ok(None);
    }

    let mut offset = SEG_DATA_OFFSET;
    let mut min_ts: Option<u64> = None;
    let mut max_ts: Option<u64> = None;
    let mut header_buf = [0u8; HEADER_SIZE];

    while offset + HEADER_SIZE <= len {
        file.seek(SeekFrom::Start(offset as u64))?;
        let read = file.read(&mut header_buf)?;
        if read != HEADER_SIZE {
            break;
        }
        let commit_len = u32::from_le_bytes(header_buf[0..4].try_into().expect("slice length"));
        if commit_len == 0 {
            break;
        }
        let payload_len =
            MessageHeader::payload_len_from_commit(commit_len).unwrap_or(MAX_PAYLOAD_LEN + 1);
        if payload_len > MAX_PAYLOAD_LEN {
            break;
        }
        let timestamp = u64::from_le_bytes(header_buf[16..24].try_into().expect("slice length"));
        min_ts = Some(min_ts.map_or(timestamp, |cur| cur.min(timestamp)));
        max_ts = Some(max_ts.map_or(timestamp, |cur| cur.max(timestamp)));

        let total = HEADER_SIZE + payload_len;
        let aligned = align_up(total, RECORD_ALIGN);
        offset = offset.saturating_add(aligned);
    }

    match (min_ts, max_ts) {
        (Some(min), Some(max)) => Ok(Some([min, max])),
        _ => Ok(None),
    }
}

pub fn write_meta(meta_path: &Path, meta: &MetaFile) -> Result<()> {
    let tmp = meta_path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(meta)?;
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp)?;
    file.write_all(&data)?;
    file.sync_all()?;
    std::fs::rename(tmp, meta_path)?;
    Ok(())
}

pub fn write_meta_at_if_missing(meta_path: &Path, meta: &MetaFile) -> Result<()> {
    if meta_path.exists() {
        return Ok(());
    }
    write_meta(meta_path, meta)
}

pub fn write_meta_if_missing(segment_path: &Path, root: &Path) -> Result<()> {
    let meta_path = segment_path
        .parent()
        .ok_or_else(|| anyhow!("segment has no parent"))?
        .join("meta.json");
    if meta_path.exists() {
        return Ok(());
    }

    let (venue, symbol) = infer_partition(segment_path, root);
    let event_time_range = scan_segment_timestamps(segment_path)?;
    let ingest_time_ns = now_ns();

    let meta = MetaFile {
        symbol_code: symbol,
        venue,
        ingest_time_ns,
        event_time_range,
        completeness: "sealed".to_string(),
    };

    write_meta(&meta_path, &meta)
}

fn infer_partition(segment_path: &Path, root: &Path) -> (Option<String>, Option<String>) {
    let relative = segment_path.strip_prefix(root).ok();
    let mut parts = Vec::new();
    if let Some(relative) = relative {
        for part in relative.iter() {
            parts.push(part.to_string_lossy().to_string());
        }
    }

    // Expect v1/{venue}/{symbol}/{date}/...
    if parts.len() >= 4 && parts[0] == "v1" {
        let venue = parts.get(1).cloned();
        let symbol = parts.get(2).cloned();
        return (venue, symbol);
    }

    (None, None)
}

fn align_up(value: usize, alignment: usize) -> usize {
    if alignment == 0 {
        return value;
    }
    let rem = value % alignment;
    if rem == 0 {
        value
    } else {
        value + (alignment - rem)
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
