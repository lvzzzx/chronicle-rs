use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;

use crate::{Error, Result};

pub const ZSTD_INDEX_MAGIC: [u8; 8] = *b"QZSTIDX1";
pub const ZSTD_INDEX_VERSION: u16 = 1;
pub const ZSTD_INDEX_HEADER_LEN: u16 = 24;
pub const ZSTD_INDEX_ENTRY_LEN: u16 = 24;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZstdSeekHeader {
    pub magic: [u8; 8],
    pub version: u16,
    pub header_len: u16,
    pub block_size: u32,
    pub frame_count: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZstdSeekEntry {
    pub uncompressed_offset: u64,
    pub compressed_offset: u64,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
}

#[derive(Debug, Clone)]
pub struct ZstdSeekIndex {
    pub block_size: u32,
    pub entries: Vec<ZstdSeekEntry>,
}

impl ZstdSeekIndex {
    pub fn entry_for_offset(&self, offset: u64) -> Option<&ZstdSeekEntry> {
        if self.entries.is_empty() {
            return None;
        }
        let mut lo = 0usize;
        let mut hi = self.entries.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            let entry = &self.entries[mid];
            let end = entry
                .uncompressed_offset
                .saturating_add(entry.uncompressed_size as u64);
            if offset < entry.uncompressed_offset {
                hi = mid;
            } else if offset >= end {
                lo = mid + 1;
            } else {
                return Some(entry);
            }
        }
        None
    }
}

pub fn write_seek_index(path: &Path, block_size: u32, entries: &[ZstdSeekEntry]) -> Result<()> {
    let header = ZstdSeekHeader {
        magic: ZSTD_INDEX_MAGIC,
        version: ZSTD_INDEX_VERSION,
        header_len: ZSTD_INDEX_HEADER_LEN,
        block_size,
        frame_count: entries.len() as u64,
    };

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    let header_bytes = encode_header(&header);
    file.write_all(&header_bytes)?;
    for entry in entries {
        let mut buf = [0u8; ZSTD_INDEX_ENTRY_LEN as usize];
        buf[0..8].copy_from_slice(&entry.uncompressed_offset.to_le_bytes());
        buf[8..16].copy_from_slice(&entry.compressed_offset.to_le_bytes());
        buf[16..20].copy_from_slice(&entry.compressed_size.to_le_bytes());
        buf[20..24].copy_from_slice(&entry.uncompressed_size.to_le_bytes());
        file.write_all(&buf)?;
    }
    file.sync_all()?;
    Ok(())
}

pub fn read_seek_index(path: &Path) -> Result<ZstdSeekIndex> {
    let mut file = std::fs::File::open(path)?;
    let mut header_buf = [0u8; ZSTD_INDEX_HEADER_LEN as usize];
    file.read_exact(&mut header_buf)?;
    let header = decode_header(&header_buf)?;

    let count = usize::try_from(header.frame_count)
        .map_err(|_| Error::Corrupt("zstd seek index frame count overflow"))?;
    let mut entries = Vec::with_capacity(count);
    let mut buf = [0u8; ZSTD_INDEX_ENTRY_LEN as usize];
    for _ in 0..count {
        file.read_exact(&mut buf)?;
        let uncompressed_offset =
            u64::from_le_bytes(buf[0..8].try_into().expect("slice length"));
        let compressed_offset =
            u64::from_le_bytes(buf[8..16].try_into().expect("slice length"));
        let compressed_size = u32::from_le_bytes(buf[16..20].try_into().expect("slice length"));
        let uncompressed_size = u32::from_le_bytes(buf[20..24].try_into().expect("slice length"));
        entries.push(ZstdSeekEntry {
            uncompressed_offset,
            compressed_offset,
            compressed_size,
            uncompressed_size,
        });
    }

    Ok(ZstdSeekIndex {
        block_size: header.block_size,
        entries,
    })
}

fn encode_header(header: &ZstdSeekHeader) -> [u8; ZSTD_INDEX_HEADER_LEN as usize] {
    let mut buf = [0u8; ZSTD_INDEX_HEADER_LEN as usize];
    buf[0..8].copy_from_slice(&header.magic);
    buf[8..10].copy_from_slice(&header.version.to_le_bytes());
    buf[10..12].copy_from_slice(&header.header_len.to_le_bytes());
    buf[12..16].copy_from_slice(&header.block_size.to_le_bytes());
    buf[16..24].copy_from_slice(&header.frame_count.to_le_bytes());
    buf
}

fn decode_header(buf: &[u8]) -> Result<ZstdSeekHeader> {
    if buf.len() < ZSTD_INDEX_HEADER_LEN as usize {
        return Err(Error::Corrupt("zstd seek index header too small"));
    }
    if buf[0..8] != ZSTD_INDEX_MAGIC {
        return Err(Error::Corrupt("zstd seek index magic mismatch"));
    }
    let version = u16::from_le_bytes(buf[8..10].try_into().expect("slice length"));
    if version != ZSTD_INDEX_VERSION {
        return Err(Error::UnsupportedVersion(version as u32));
    }
    let header_len = u16::from_le_bytes(buf[10..12].try_into().expect("slice length"));
    if header_len != ZSTD_INDEX_HEADER_LEN {
        return Err(Error::Corrupt("zstd seek index header length mismatch"));
    }
    let block_size = u32::from_le_bytes(buf[12..16].try_into().expect("slice length"));
    let frame_count = u64::from_le_bytes(buf[16..24].try_into().expect("slice length"));
    Ok(ZstdSeekHeader {
        magic: ZSTD_INDEX_MAGIC,
        version,
        header_len,
        block_size,
        frame_count,
    })
}
