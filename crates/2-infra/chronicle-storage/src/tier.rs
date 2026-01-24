use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use chronicle_core::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, RECORD_ALIGN};
use chronicle_core::segment::{SEG_DATA_OFFSET, SEG_FLAG_SEALED, SEG_MAGIC, SEG_VERSION};

use crate::compress_q_to_zst;

const DEFAULT_BLOCK_SIZE: usize = 1 << 20;

#[derive(Debug, Clone)]
pub struct TierConfig {
    pub root: PathBuf,
    pub block_size: usize,
    pub cold_after: Duration,
    pub remote_after: Duration,
    pub remote_root: Option<PathBuf>,
}

impl TierConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            block_size: DEFAULT_BLOCK_SIZE,
            cold_after: Duration::from_secs(0),
            remote_after: Duration::from_secs(0),
            remote_root: None,
        }
    }
}

pub struct TierManager {
    config: TierConfig,
}

impl TierManager {
    pub fn new(config: TierConfig) -> Self {
        Self { config }
    }

    pub fn run_once(&self) -> Result<()> {
        let mut q_files = Vec::new();
        collect_q_files(&self.config.root, &mut q_files)?;

        for path in q_files {
            self.handle_q_file(&path)?;
        }

        if let Some(remote_root) = &self.config.remote_root {
            let mut zst_files = Vec::new();
            collect_zst_files(&self.config.root, &mut zst_files)?;
            for path in zst_files {
                self.handle_remote_move(&path, remote_root)?;
            }
        }

        Ok(())
    }

    fn handle_q_file(&self, path: &Path) -> Result<()> {
        if !is_eligible_for_cold(path, self.config.cold_after)? {
            return Ok(());
        }

    let flags = read_segment_flags(path)?;
    if (flags & SEG_FLAG_SEALED) == 0 {
            return Ok(());
        }

        let zst_path = path.with_extension("q.zst");
        let idx_path = path.with_extension("q.zst.idx");
        if zst_path.exists() && idx_path.exists() {
            return Ok(());
        }

        let zst_tmp = path.with_extension("q.zst.tmp");
        let idx_tmp = path.with_extension("q.zst.idx.tmp");
        let _ = std::fs::remove_file(&zst_tmp);
        let _ = std::fs::remove_file(&idx_tmp);

        compress_q_to_zst(path, &zst_tmp, &idx_tmp, self.config.block_size)?;

        std::fs::rename(&zst_tmp, &zst_path)?;
        std::fs::rename(&idx_tmp, &idx_path)?;

        write_meta_if_missing(path, &self.config.root)?;

        std::fs::remove_file(path)?;

        Ok(())
    }

    fn handle_remote_move(&self, zst_path: &Path, remote_root: &Path) -> Result<()> {
        if !is_eligible_for_remote(zst_path, self.config.remote_after)? {
            return Ok(());
        }

        let idx_path = zst_path.with_extension("q.zst.idx");
        if !idx_path.exists() {
            return Ok(());
        }

        let stub_path = zst_path.with_extension("q.zst.remote.json");
        if stub_path.exists() {
            return Ok(());
        }

        let relative = zst_path
            .strip_prefix(&self.config.root)
            .unwrap_or(zst_path);
        let remote_zst = remote_root.join(relative);
        let remote_idx = remote_zst.with_extension("q.zst.idx");

        std::fs::create_dir_all(
            remote_zst
                .parent()
                .ok_or_else(|| anyhow!("remote path has no parent"))?,
        )?;

        let bytes = std::fs::copy(zst_path, &remote_zst)?;
        let idx_bytes = std::fs::copy(&idx_path, &remote_idx)?;

        let stub = RemoteStub {
            remote_uri: remote_zst.to_string_lossy().to_string(),
            size_bytes: bytes,
            idx_size_bytes: idx_bytes,
        };
        write_stub(&stub_path, &stub)?;

        std::fs::remove_file(zst_path)?;
        std::fs::remove_file(idx_path)?;

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RemoteStub {
    remote_uri: String,
    size_bytes: u64,
    idx_size_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct MetaFile {
    symbol_code: Option<String>,
    venue: Option<String>,
    ingest_time_ns: u64,
    event_time_range: Option<[u64; 2]>,
    completeness: String,
}

fn write_stub(path: &Path, stub: &RemoteStub) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(stub)?;
    let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(&tmp)?;
    file.write_all(&data)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn write_meta_if_missing(segment_path: &Path, root: &Path) -> Result<()> {
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

    let tmp = meta_path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(&meta)?;
    let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(&tmp)?;
    file.write_all(&data)?;
    file.sync_all()?;
    std::fs::rename(tmp, meta_path)?;
    Ok(())
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

fn collect_q_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_q_files(&path, files)?;
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.ends_with(".q") && !name.ends_with(".q.tmp") {
                files.push(path);
            }
        }
    }
    Ok(())
}

fn collect_zst_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_zst_files(&path, files)?;
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.ends_with(".q.zst") {
                files.push(path);
            }
        }
    }
    Ok(())
}

fn read_segment_flags(path: &Path) -> Result<u32> {
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

fn scan_segment_timestamps(path: &Path) -> Result<Option<[u64; 2]>> {
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
        let commit_len =
            u32::from_le_bytes(header_buf[0..4].try_into().expect("slice length"));
        if commit_len == 0 {
            break;
        }
        let payload_len = MessageHeader::payload_len_from_commit(commit_len)
            .unwrap_or(MAX_PAYLOAD_LEN + 1);
        if payload_len > MAX_PAYLOAD_LEN {
            break;
        }
        let timestamp = u64::from_le_bytes(
            header_buf[16..24].try_into().expect("slice length"),
        );
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

fn is_eligible_for_cold(path: &Path, cold_after: Duration) -> Result<bool> {
    if cold_after.as_secs() == 0 {
        return Ok(true);
    }
    let metadata = path.metadata()?;
    let modified = metadata.modified().ok();
    if let Some(modified) = modified {
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::from_secs(0));
        return Ok(age >= cold_after);
    }
    Ok(false)
}

fn is_eligible_for_remote(path: &Path, remote_after: Duration) -> Result<bool> {
    if remote_after.as_secs() == 0 {
        return Ok(false);
    }
    let metadata = path.metadata()?;
    let modified = metadata.modified().ok();
    if let Some(modified) = modified {
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::from_secs(0));
        return Ok(age >= remote_after);
    }
    Ok(false)
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
