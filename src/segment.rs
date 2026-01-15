use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use crate::Result;

pub const SEGMENT_SIZE: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentIndex {
    pub current_segment: u64,
    pub write_offset: u64,
}

impl SegmentIndex {
    pub fn new(current_segment: u64, write_offset: u64) -> Self {
        Self {
            current_segment,
            write_offset,
        }
    }
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
