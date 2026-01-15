use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::mmap::MmapFile;
use crate::{Error, Result};

pub const SEGMENT_SIZE: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentIndex {
    pub current_segment: u64,
    pub write_offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderPosition {
    pub segment_id: u64,
    pub offset: u64,
}

impl SegmentIndex {
    pub fn new(current_segment: u64, write_offset: u64) -> Self {
        Self {
            current_segment,
            write_offset,
        }
    }
}

impl ReaderPosition {
    pub fn new(segment_id: u64, offset: u64) -> Self {
        Self { segment_id, offset }
    }
}

pub fn segment_filename(id: u64) -> String {
    format!("{:09}.q", id)
}

pub fn segment_path(root: &Path, id: u64) -> PathBuf {
    root.join(segment_filename(id))
}

pub fn open_segment(root: &Path, id: u64) -> Result<MmapFile> {
    let path = segment_path(root, id);
    let mmap = MmapFile::open(&path)?;
    if mmap.len() != SEGMENT_SIZE {
        return Err(Error::Corrupt("segment size mismatch"));
    }
    Ok(mmap)
}

pub fn create_segment(root: &Path, id: u64) -> Result<MmapFile> {
    let path = segment_path(root, id);
    MmapFile::create(&path, SEGMENT_SIZE)
}

pub fn load_index(path: &Path) -> Result<SegmentIndex> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SegmentIndex::new(0, 0))
        }
        Err(err) => return Err(err.into()),
    };
    let mut buf = [0u8; 16];
    file.read_exact(&mut buf)?;
    let current_segment = u64::from_le_bytes(buf[0..8].try_into().expect("slice length"));
    let write_offset = u64::from_le_bytes(buf[8..16].try_into().expect("slice length"));
    Ok(SegmentIndex::new(current_segment, write_offset))
}

pub fn store_index(path: &Path, index: &SegmentIndex) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&index.current_segment.to_le_bytes());
    buf[8..16].copy_from_slice(&index.write_offset.to_le_bytes());
    file.write_all(&buf)?;
    file.sync_all()?;
    Ok(())
}

pub fn load_reader_position(path: &Path) -> Result<ReaderPosition> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReaderPosition::new(0, 0))
        }
        Err(err) => return Err(err.into()),
    };
    let len = file.metadata()?.len();
    if len == 8 {
        let mut buf = [0u8; 8];
        file.read_exact(&mut buf)?;
        let offset = u64::from_le_bytes(buf);
        return Ok(ReaderPosition::new(0, offset));
    }
    if len != 16 {
        return Err(Error::Corrupt("reader metadata has unexpected size"));
    }
    let mut buf = [0u8; 16];
    file.read_exact(&mut buf)?;
    let segment_id = u64::from_le_bytes(buf[0..8].try_into().expect("slice length"));
    let offset = u64::from_le_bytes(buf[8..16].try_into().expect("slice length"));
    Ok(ReaderPosition::new(segment_id, offset))
}

pub fn store_reader_position(path: &Path, position: &ReaderPosition) -> Result<()> {
    let tmp_path = path.with_extension("meta.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp_path)?;
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&position.segment_id.to_le_bytes());
    buf[8..16].copy_from_slice(&position.offset.to_le_bytes());
    file.write_all(&buf)?;
    file.sync_all()?;
    std::fs::rename(tmp_path, path)?;
    Ok(())
}
