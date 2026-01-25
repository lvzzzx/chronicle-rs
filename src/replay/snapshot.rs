use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Result};

pub const SNAPSHOT_MAGIC: [u8; 8] = *b"CHRSNAP1";
pub const SNAPSHOT_VERSION: u16 = 1;
pub const SNAPSHOT_HEADER_LEN: usize = 128;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotHeader {
    pub magic: [u8; 8],
    pub schema_version: u16,
    pub header_len: u16,
    pub endianness: u16,
    pub book_mode: u16,
    pub venue_id: u32,
    pub market_id: u32,
    pub seq_num: u64,
    pub ingest_ts_ns_start: u64,
    pub ingest_ts_ns_end: u64,
    pub exchange_ts_ns_start: u64,
    pub exchange_ts_ns_end: u64,
    pub book_hash: [u8; 16],
    pub payload_len: u64,
    pub flags: u64,
    pub reserved: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotMetadata {
    pub schema_version: u16,
    pub endianness: u16,
    pub book_mode: u16,
    pub venue_id: u32,
    pub market_id: u32,
    pub seq_num: u64,
    pub ingest_ts_ns_start: u64,
    pub ingest_ts_ns_end: u64,
    pub exchange_ts_ns_start: u64,
    pub exchange_ts_ns_end: u64,
    pub book_hash: [u8; 16],
    pub flags: u64,
}

impl SnapshotMetadata {
    pub fn to_header(&self, payload_len: u64) -> SnapshotHeader {
        SnapshotHeader {
            magic: SNAPSHOT_MAGIC,
            schema_version: self.schema_version,
            header_len: SNAPSHOT_HEADER_LEN as u16,
            endianness: self.endianness,
            book_mode: self.book_mode,
            venue_id: self.venue_id,
            market_id: self.market_id,
            seq_num: self.seq_num,
            ingest_ts_ns_start: self.ingest_ts_ns_start,
            ingest_ts_ns_end: self.ingest_ts_ns_end,
            exchange_ts_ns_start: self.exchange_ts_ns_start,
            exchange_ts_ns_end: self.exchange_ts_ns_end,
            book_hash: self.book_hash,
            payload_len,
            flags: self.flags,
            reserved: [0u8; 32],
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotRetention {
    pub max_snapshots: Option<usize>,
    pub max_bytes: Option<u64>,
    pub max_age: Option<Duration>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnapshotPolicy {
    pub min_interval: Option<Duration>,
    pub min_records: Option<u64>,
    pub min_bytes: Option<u64>,
}

pub struct SnapshotPlanner {
    policy: SnapshotPolicy,
    last_snapshot_at: Option<SystemTime>,
    records_since: u64,
    bytes_since: u64,
}

impl SnapshotPlanner {
    pub fn new(policy: SnapshotPolicy) -> Self {
        Self {
            policy,
            last_snapshot_at: None,
            records_since: 0,
            bytes_since: 0,
        }
    }

    pub fn observe(&mut self, records: u64, bytes: u64) {
        self.records_since = self.records_since.saturating_add(records);
        self.bytes_since = self.bytes_since.saturating_add(bytes);
    }

    pub fn should_snapshot(&self, now: SystemTime) -> bool {
        let mut ok = false;
        if let Some(min_records) = self.policy.min_records {
            ok |= self.records_since >= min_records;
        }
        if let Some(min_bytes) = self.policy.min_bytes {
            ok |= self.bytes_since >= min_bytes;
        }
        if let Some(min_interval) = self.policy.min_interval {
            if let Some(last) = self.last_snapshot_at {
                if let Ok(elapsed) = now.duration_since(last) {
                    ok |= elapsed >= min_interval;
                }
            } else {
                ok = true;
            }
        }
        ok
    }

    pub fn mark_snapshot(&mut self, now: SystemTime) {
        self.last_snapshot_at = Some(now);
        self.records_since = 0;
        self.bytes_since = 0;
    }
}

pub struct SnapshotWriter {
    root: PathBuf,
    retention: SnapshotRetention,
}

impl SnapshotWriter {
    pub fn new(root: impl AsRef<Path>, retention: SnapshotRetention) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            retention,
        }
    }

    pub fn snapshot_dir(&self, venue_id: u32, market_id: u32) -> PathBuf {
        snapshot_dir(&self.root, venue_id, market_id)
    }

    pub fn write_snapshot(&self, metadata: &SnapshotMetadata, payload: &[u8]) -> Result<PathBuf> {
        let dir = self.snapshot_dir(metadata.venue_id, metadata.market_id);
        std::fs::create_dir_all(&dir)?;

        let final_path = snapshot_path(&dir, metadata.seq_num);
        let tmp_path = snapshot_tmp_path(&dir, metadata.seq_num);

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;

        let header = metadata.to_header(payload.len() as u64);
        let header_bytes = encode_header(&header);
        file.write_all(&header_bytes)?;
        file.write_all(payload)?;
        file.sync_all()?;
        drop(file);

        std::fs::rename(&tmp_path, &final_path)?;
        sync_dir(&dir)?;

        enforce_retention(&dir, self.retention)?;
        Ok(final_path)
    }
}

pub fn snapshot_path(dir: &Path, seq_num: u64) -> PathBuf {
    dir.join(format!("snapshot_{seq_num}.bin"))
}

pub fn snapshot_dir(root: &Path, venue_id: u32, market_id: u32) -> PathBuf {
    root.join("snapshots")
        .join(venue_id.to_string())
        .join(market_id.to_string())
}

fn snapshot_tmp_path(dir: &Path, seq_num: u64) -> PathBuf {
    dir.join(format!("snapshot_{seq_num}.tmp"))
}

pub fn load_snapshot_header(path: &Path) -> Result<SnapshotHeader> {
    let mut file = File::open(path)?;
    let mut buf = [0u8; SNAPSHOT_HEADER_LEN];
    file.read_exact(&mut buf)?;
    decode_header(&buf)
}

pub fn load_snapshot(path: &Path) -> Result<(SnapshotHeader, Vec<u8>)> {
    let mut file = File::open(path)?;
    let mut buf = [0u8; SNAPSHOT_HEADER_LEN];
    file.read_exact(&mut buf)?;
    let header = decode_header(&buf)?;
    let metadata = file.metadata()?;
    let file_len = metadata.len();
    let expected_len = (SNAPSHOT_HEADER_LEN as u64)
        .checked_add(header.payload_len)
        .ok_or_else(|| anyhow!("snapshot length overflow"))?;
    if file_len != expected_len {
        bail!("snapshot length mismatch: expected {expected_len} bytes, found {file_len} bytes");
    }
    let mut payload = vec![0u8; header.payload_len as usize];
    if header.payload_len > 0 {
        file.read_exact(&mut payload)?;
    }
    Ok((header, payload))
}

fn encode_header(header: &SnapshotHeader) -> [u8; SNAPSHOT_HEADER_LEN] {
    let mut buf = [0u8; SNAPSHOT_HEADER_LEN];
    buf[0..8].copy_from_slice(&header.magic);
    buf[8..10].copy_from_slice(&header.schema_version.to_le_bytes());
    buf[10..12].copy_from_slice(&header.header_len.to_le_bytes());
    buf[12..14].copy_from_slice(&header.endianness.to_le_bytes());
    buf[14..16].copy_from_slice(&header.book_mode.to_le_bytes());
    buf[16..20].copy_from_slice(&header.venue_id.to_le_bytes());
    buf[20..24].copy_from_slice(&header.market_id.to_le_bytes());
    buf[24..32].copy_from_slice(&header.seq_num.to_le_bytes());
    buf[32..40].copy_from_slice(&header.ingest_ts_ns_start.to_le_bytes());
    buf[40..48].copy_from_slice(&header.ingest_ts_ns_end.to_le_bytes());
    buf[48..56].copy_from_slice(&header.exchange_ts_ns_start.to_le_bytes());
    buf[56..64].copy_from_slice(&header.exchange_ts_ns_end.to_le_bytes());
    buf[64..80].copy_from_slice(&header.book_hash);
    buf[80..88].copy_from_slice(&header.payload_len.to_le_bytes());
    buf[88..96].copy_from_slice(&header.flags.to_le_bytes());
    buf[96..128].copy_from_slice(&header.reserved);
    buf
}

fn decode_header(buf: &[u8]) -> Result<SnapshotHeader> {
    if buf.len() < SNAPSHOT_HEADER_LEN {
        bail!("snapshot header too small");
    }
    if buf[0..8] != SNAPSHOT_MAGIC {
        bail!("snapshot magic mismatch");
    }
    let schema_version = u16::from_le_bytes(buf[8..10].try_into().expect("slice length"));
    let header_len = u16::from_le_bytes(buf[10..12].try_into().expect("slice length"));
    if header_len as usize != SNAPSHOT_HEADER_LEN {
        bail!("snapshot header length mismatch");
    }
    let endianness = u16::from_le_bytes(buf[12..14].try_into().expect("slice length"));
    let book_mode = u16::from_le_bytes(buf[14..16].try_into().expect("slice length"));
    let venue_id = u32::from_le_bytes(buf[16..20].try_into().expect("slice length"));
    let market_id = u32::from_le_bytes(buf[20..24].try_into().expect("slice length"));
    let seq_num = u64::from_le_bytes(buf[24..32].try_into().expect("slice length"));
    let ingest_ts_ns_start = u64::from_le_bytes(buf[32..40].try_into().expect("slice length"));
    let ingest_ts_ns_end = u64::from_le_bytes(buf[40..48].try_into().expect("slice length"));
    let exchange_ts_ns_start = u64::from_le_bytes(buf[48..56].try_into().expect("slice length"));
    let exchange_ts_ns_end = u64::from_le_bytes(buf[56..64].try_into().expect("slice length"));
    let book_hash: [u8; 16] = buf[64..80].try_into().expect("slice length");
    let payload_len = u64::from_le_bytes(buf[80..88].try_into().expect("slice length"));
    let flags = u64::from_le_bytes(buf[88..96].try_into().expect("slice length"));
    let reserved: [u8; 32] = buf[96..128].try_into().expect("slice length");

    Ok(SnapshotHeader {
        magic: SNAPSHOT_MAGIC,
        schema_version,
        header_len,
        endianness,
        book_mode,
        venue_id,
        market_id,
        seq_num,
        ingest_ts_ns_start,
        ingest_ts_ns_end,
        exchange_ts_ns_start,
        exchange_ts_ns_end,
        book_hash,
        payload_len,
        flags,
        reserved,
    })
}

fn sync_dir(path: &Path) -> Result<()> {
    let dir = File::open(path)?;
    dir.sync_all()?;
    Ok(())
}

#[derive(Debug, Clone)]
struct SnapshotEntry {
    path: PathBuf,
    seq_num: u64,
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub path: PathBuf,
    pub header: SnapshotHeader,
}

pub fn list_snapshots(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = load_snapshot_entries(dir)?;
    Ok(entries.into_iter().map(|entry| entry.path).collect())
}

pub fn list_snapshot_headers(dir: &Path) -> Result<Vec<SnapshotInfo>> {
    let entries = load_snapshot_entries(dir)?;
    let mut snapshots = Vec::with_capacity(entries.len());
    for entry in entries {
        let header = load_snapshot_header(&entry.path)?;
        snapshots.push(SnapshotInfo {
            path: entry.path,
            header,
        });
    }
    Ok(snapshots)
}

pub fn latest_snapshot_before_seq(dir: &Path, seq_num: u64) -> Result<Option<SnapshotInfo>> {
    let entries = load_snapshot_entries(dir)?;
    let mut best: Option<SnapshotInfo> = None;
    for entry in entries {
        let header = load_snapshot_header(&entry.path)?;
        if header.seq_num > seq_num {
            continue;
        }
        let replace = match &best {
            Some(current) => header.seq_num > current.header.seq_num,
            None => true,
        };
        if replace {
            best = Some(SnapshotInfo {
                path: entry.path,
                header,
            });
        }
    }
    Ok(best)
}

pub fn latest_snapshot_before_exchange_ts(dir: &Path, ts_ns: u64) -> Result<Option<SnapshotInfo>> {
    let entries = load_snapshot_entries(dir)?;
    let mut best: Option<SnapshotInfo> = None;
    for entry in entries {
        let header = load_snapshot_header(&entry.path)?;
        if header.exchange_ts_ns_end > ts_ns {
            continue;
        }
        let replace = match &best {
            Some(current) => header.exchange_ts_ns_end > current.header.exchange_ts_ns_end,
            None => true,
        };
        if replace {
            best = Some(SnapshotInfo {
                path: entry.path,
                header,
            });
        }
    }
    Ok(best)
}

pub fn latest_snapshot_before_ingest_ts(dir: &Path, ts_ns: u64) -> Result<Option<SnapshotInfo>> {
    let entries = load_snapshot_entries(dir)?;
    let mut best: Option<SnapshotInfo> = None;
    for entry in entries {
        let header = load_snapshot_header(&entry.path)?;
        if header.ingest_ts_ns_end > ts_ns {
            continue;
        }
        let replace = match &best {
            Some(current) => header.ingest_ts_ns_end > current.header.ingest_ts_ns_end,
            None => true,
        };
        if replace {
            best = Some(SnapshotInfo {
                path: entry.path,
                header,
            });
        }
    }
    Ok(best)
}

pub fn enforce_retention(dir: &Path, retention: SnapshotRetention) -> Result<Vec<PathBuf>> {
    if retention.max_snapshots.is_none()
        && retention.max_bytes.is_none()
        && retention.max_age.is_none()
    {
        return Ok(Vec::new());
    }

    let mut entries = load_snapshot_entries(dir)?;
    let mut removed = Vec::new();

    if let Some(max_age) = retention.max_age {
        let now = SystemTime::now();
        entries.retain(|entry| {
            if let Some(modified) = entry.modified {
                if let Ok(elapsed) = now.duration_since(modified) {
                    if elapsed > max_age {
                        let _ = std::fs::remove_file(&entry.path);
                        removed.push(entry.path.clone());
                        return false;
                    }
                }
            }
            true
        });
    }

    entries.sort_by_key(|entry| entry.seq_num);

    if let Some(max_snapshots) = retention.max_snapshots {
        while entries.len() > max_snapshots {
            if let Some(entry) = entries.first() {
                let _ = std::fs::remove_file(&entry.path);
                removed.push(entry.path.clone());
            }
            entries.remove(0);
        }
    }

    if let Some(max_bytes) = retention.max_bytes {
        let mut total: u64 = entries.iter().map(|entry| entry.size).sum();
        while total > max_bytes && !entries.is_empty() {
            let entry = entries.remove(0);
            let _ = std::fs::remove_file(&entry.path);
            total = total.saturating_sub(entry.size);
            removed.push(entry.path.clone());
        }
    }

    Ok(removed)
}

fn load_snapshot_entries(dir: &Path) -> Result<Vec<SnapshotEntry>> {
    let mut entries = Vec::new();
    if !dir.exists() {
        return Ok(entries);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("bin") {
            continue;
        }
        let name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name,
            None => continue,
        };
        let seq_num = match parse_snapshot_filename(name) {
            Some(seq_num) => seq_num,
            None => continue,
        };
        let metadata = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        let size = metadata.len();
        let modified = metadata.modified().ok();
        entries.push(SnapshotEntry {
            path,
            seq_num,
            size,
            modified,
        });
    }
    Ok(entries)
}

fn parse_snapshot_filename(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".bin")?;
    let seq = stem.strip_prefix("snapshot_")?;
    seq.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_metadata(seq: u64) -> SnapshotMetadata {
        SnapshotMetadata {
            schema_version: SNAPSHOT_VERSION,
            endianness: 0,
            book_mode: 0,
            venue_id: 7,
            market_id: 11,
            seq_num: seq,
            ingest_ts_ns_start: 100,
            ingest_ts_ns_end: 200,
            exchange_ts_ns_start: 120,
            exchange_ts_ns_end: 180,
            book_hash: [0u8; 16],
            flags: 0,
        }
    }

    #[test]
    fn snapshot_header_round_trip() -> Result<()> {
        let header = sample_metadata(42).to_header(64);
        let encoded = encode_header(&header);
        let decoded = decode_header(&encoded)?;
        assert_eq!(decoded, header);
        Ok(())
    }

    #[test]
    fn snapshot_write_and_load() -> Result<()> {
        let dir = tempdir()?;
        let writer = SnapshotWriter::new(dir.path(), SnapshotRetention::default());
        let metadata = sample_metadata(99);
        let payload = vec![1u8; 32];
        let path = writer.write_snapshot(&metadata, &payload)?;
        assert!(path.exists());
        assert!(!path.with_extension("tmp").exists());

        let (header, loaded) = load_snapshot(&path)?;
        assert_eq!(header.seq_num, 99);
        assert_eq!(header.payload_len, 32);
        assert_eq!(loaded.len(), 32);
        Ok(())
    }

    #[test]
    fn snapshot_retention_keeps_newest() -> Result<()> {
        let dir = tempdir()?;
        let writer = SnapshotWriter::new(dir.path(), SnapshotRetention::default());
        for seq in 1..=4_u64 {
            let metadata = sample_metadata(seq);
            let payload = vec![0u8; 8];
            writer.write_snapshot(&metadata, &payload)?;
        }

        let removed = enforce_retention(
            &writer.snapshot_dir(7, 11),
            SnapshotRetention {
                max_snapshots: Some(2),
                max_bytes: None,
                max_age: None,
            },
        )?;
        assert_eq!(removed.len(), 2);
        let remaining = list_snapshots(&writer.snapshot_dir(7, 11))?;
        assert_eq!(remaining.len(), 2);
        Ok(())
    }

    #[test]
    fn snapshot_planner_triggers() {
        let policy = SnapshotPolicy {
            min_interval: None,
            min_records: Some(10),
            min_bytes: Some(100),
        };
        let mut planner = SnapshotPlanner::new(policy);
        planner.observe(5, 50);
        assert!(!planner.should_snapshot(SystemTime::now()));
        planner.observe(5, 60);
        assert!(planner.should_snapshot(SystemTime::now()));
    }

    #[test]
    fn snapshot_select_latest_by_seq() -> Result<()> {
        let dir = tempdir()?;
        let writer = SnapshotWriter::new(dir.path(), SnapshotRetention::default());
        for seq in [10_u64, 20, 30] {
            let metadata = sample_metadata(seq);
            writer.write_snapshot(&metadata, &[0u8; 4])?;
        }

        let snap_dir = writer.snapshot_dir(7, 11);
        let latest = latest_snapshot_before_seq(&snap_dir, 25)?.expect("expected snapshot");
        assert_eq!(latest.header.seq_num, 20);
        Ok(())
    }
}
