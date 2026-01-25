use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::core::header::{MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, PAD_TYPE_ID, RECORD_ALIGN};
use crate::core::mmap::MmapFile;
use crate::core::{Error, Result};

pub const DEFAULT_SEGMENT_SIZE: usize = 128 * 1024 * 1024;
pub const SEG_HEADER_SIZE: usize = 64;
pub const SEG_DATA_OFFSET: usize = 64;
pub const SEG_MAGIC: u32 = 0x53454730; // 'SEG0'
pub const SEG_VERSION: u32 = 1;
pub const SEG_FLAG_SEALED: u32 = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentHeader {
    pub magic: u32,
    pub version: u32,
    pub segment_id: u32,
    pub flags: u32,
    pub _pad: [u8; 48],
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderMeta {
    pub segment_id: u64,
    pub offset: u64,
    pub last_heartbeat_ns: u64,
    pub generation: u64,
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

impl ReaderMeta {
    pub fn new(segment_id: u64, offset: u64, last_heartbeat_ns: u64, generation: u64) -> Self {
        Self {
            segment_id,
            offset,
            last_heartbeat_ns,
            generation,
        }
    }
}

pub fn segment_filename(id: u64) -> String {
    format!("{:09}.q", id)
}

pub fn segment_temp_filename(id: u64) -> String {
    format!("{:09}.q.tmp", id)
}

pub fn segment_path(root: &Path, id: u64) -> PathBuf {
    root.join(segment_filename(id))
}

pub fn segment_temp_path(root: &Path, id: u64) -> PathBuf {
    root.join(segment_temp_filename(id))
}

pub fn open_segment(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile> {
    let path = segment_path(root, id);
    let mmap = MmapFile::open(&path)?;
    if mmap.len() != segment_size {
        return Err(Error::Corrupt("segment size mismatch"));
    }
    let header = read_segment_header(&mmap)?;
    if header.segment_id != id as u32 {
        return Err(Error::Corrupt("segment id mismatch"));
    }
    Ok(mmap)
}

pub fn create_segment(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile> {
    let path = segment_path(root, id);
    let mut mmap = MmapFile::create(&path, segment_size)?;
    write_segment_header(&mut mmap, id as u32, 0)?;
    Ok(mmap)
}

pub fn open_or_create_segment(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile> {
    let path = segment_path(root, id);
    match open_segment(root, id, segment_size) {
        Ok(mmap) => Ok(mmap),
        Err(Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            match MmapFile::create_new(&path, segment_size) {
                Ok(mut mmap) => {
                    write_segment_header(&mut mmap, id as u32, 0)?;
                    Ok(mmap)
                }
                Err(Error::Io(err)) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    open_segment(root, id, segment_size)
                }
                Err(err) => Err(err),
            }
        }
        Err(err) => Err(err),
    }
}

pub fn prepare_or_open_segment(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile> {
    let path = segment_path(root, id);
    match open_segment(root, id, segment_size) {
        Ok(mmap) => Ok(mmap),
        Err(Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            match MmapFile::create_new(&path, segment_size) {
                Ok(mut mmap) => {
                    write_segment_header(&mut mmap, id as u32, 0)?;
                    prefault_mmap(&mut mmap);
                    Ok(mmap)
                }
                Err(Error::Io(err)) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    open_segment(root, id, segment_size)
                }
                Err(err) => Err(err),
            }
        }
        Err(err) => Err(err),
    }
}

pub fn prepare_segment(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile> {
    let mut mmap = create_segment(root, id, segment_size)?;
    prefault_mmap(&mut mmap);
    Ok(mmap)
}

pub fn prepare_segment_temp(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile> {
    let temp_path = segment_temp_path(root, id);
    let _ = std::fs::remove_file(&temp_path);
    let mut mmap = MmapFile::create(&temp_path, segment_size)?;
    write_segment_header(&mut mmap, id as u32, 0)?;
    prefault_mmap(&mut mmap);
    Ok(mmap)
}

/// Iterates through the memory map and writes a 0 to every 4KB page.
///
/// **Why this matters for HFT:**
/// When the OS allocates memory (via mmap or malloc), it is often "lazy". It assigns
/// virtual addresses but does not allocate physical RAM or reserve disk blocks until
/// the first write. This causes a **Page Fault** (context switch + allocation) on the
/// first access, introducing non-deterministic latency spikes (microseconds to milliseconds)
/// in the critical path.
///
/// By writing to every page during initialization (prefaulting), we force the OS to:
/// 1. Allocate physical RAM.
/// 2. Update page tables to mark pages as "Present" and "Writable".
/// 3. Reserve disk blocks (if file-backed).
///
/// This acts as a "poor man's" `fallocate` or `madvise(MADV_WILLNEED)` but is often more
/// reliable and granular for ensuring memory is "hot" before use.
pub fn prefault_mmap(mmap: &mut MmapFile) {
    let slice = mmap.as_mut_slice();
    let len = slice.len();
    let page_size = 4096;
    let mut offset = page_size;
    // Skip the first page (offset 0) because header writing usually covers it.
    while offset < len {
        // Volatile write to prevent compiler optimization (though simple assignment usually works across FFI boundaries)
        // We use simple assignment here as we are treating the slice as volatile memory in the abstract sense of the OS.
        slice[offset] = 0;
        offset += page_size;
    }
}

pub fn publish_segment(temp: &Path, final_path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let temp_c = CString::new(temp.as_os_str().as_bytes())
            .map_err(|_| Error::Unsupported("segment temp path contains null byte"))?;
        let final_c = CString::new(final_path.as_os_str().as_bytes())
            .map_err(|_| Error::Unsupported("segment path contains null byte"))?;
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                temp_c.as_ptr(),
                libc::AT_FDCWD,
                final_c.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ENOSYS) && err.raw_os_error() != Some(libc::EINVAL) {
            return Err(Error::Io(err));
        }
    }

    if final_path.exists() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "segment already exists",
        )));
    }
    std::fs::rename(temp, final_path)?;
    Ok(())
}

pub fn validate_segment_size(segment_size: u64) -> Result<usize> {
    let size = usize::try_from(segment_size)
        .map_err(|_| Error::Unsupported("segment size exceeds addressable range"))?;
    let min_size = SEG_DATA_OFFSET + HEADER_SIZE;
    if size < min_size {
        return Err(Error::Unsupported("segment size too small"));
    }
    Ok(size)
}

pub fn load_index(path: &Path) -> Result<SegmentIndex> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SegmentIndex::new(0, SEG_DATA_OFFSET as u64))
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

pub fn load_reader_meta(path: &Path) -> Result<ReaderMeta> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReaderMeta::new(0, SEG_DATA_OFFSET as u64, 0, 0))
        }
        Err(err) => return Err(err.into()),
    };
    let len = file.metadata()?.len();
    if len == 8 {
        let mut buf = [0u8; 8];
        file.read_exact(&mut buf)?;
        let offset = u64::from_le_bytes(buf);
        return Ok(ReaderMeta::new(0, offset, 0, 0));
    }
    if len == 16 {
        let mut buf = [0u8; 16];
        file.read_exact(&mut buf)?;
        let segment_id = u64::from_le_bytes(buf[0..8].try_into().expect("slice length"));
        let offset = u64::from_le_bytes(buf[8..16].try_into().expect("slice length"));
        return Ok(ReaderMeta::new(segment_id, offset, 0, 0));
    }
    if len != reader_meta_file_size() as u64 {
        return Err(Error::CorruptMetadata(
            "reader metadata has unexpected size",
        ));
    }
    let mut buf = vec![0u8; reader_meta_file_size()];
    file.read_exact(&mut buf)?;
    let slot0 = parse_reader_slot(&buf[0..reader_meta_slot_size()]);
    let slot1 = parse_reader_slot(&buf[reader_meta_slot_size()..reader_meta_file_size()]);
    match select_reader_slot(slot0, slot1) {
        Some(meta) => Ok(meta),
        None => Err(Error::CorruptMetadata("no valid reader metadata slot")),
    }
}

pub fn store_reader_meta(path: &Path, meta: &mut ReaderMeta) -> Result<()> {
    let slot_size = reader_meta_slot_size();
    let file_size = reader_meta_file_size();
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)?;
    file.set_len(file_size as u64)?;
    meta.generation = meta.generation.saturating_add(1);
    let slot = (meta.generation % 2) as usize;
    let offset = slot * slot_size;
    let buf = encode_reader_slot(meta)?;
    file.seek(SeekFrom::Start(offset as u64))?;
    file.write_all(&buf)?;
    file.sync_all()?;
    Ok(())
}

pub fn read_segment_header(mmap: &MmapFile) -> Result<SegmentHeader> {
    if mmap.len() < SEG_HEADER_SIZE {
        return Err(Error::Corrupt("segment too small for header"));
    }
    let mut buf = [0u8; SEG_HEADER_SIZE];
    buf.copy_from_slice(&mmap.as_slice()[0..SEG_HEADER_SIZE]);
    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice length"));
    let version = u32::from_le_bytes(buf[4..8].try_into().expect("slice length"));
    let segment_id = u32::from_le_bytes(buf[8..12].try_into().expect("slice length"));
    let flags = u32::from_le_bytes(buf[12..16].try_into().expect("slice length"));
    if magic != SEG_MAGIC {
        return Err(Error::Corrupt("segment magic mismatch"));
    }
    if version != SEG_VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    Ok(SegmentHeader {
        magic,
        version,
        segment_id,
        flags,
        _pad: [0u8; 48],
    })
}

pub fn write_segment_header(mmap: &mut MmapFile, segment_id: u32, flags: u32) -> Result<()> {
    if mmap.len() < SEG_HEADER_SIZE {
        return Err(Error::Corrupt("segment too small for header"));
    }
    let mut buf = [0u8; SEG_HEADER_SIZE];
    buf[0..4].copy_from_slice(&SEG_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&SEG_VERSION.to_le_bytes());
    buf[8..12].copy_from_slice(&segment_id.to_le_bytes());
    buf[12..16].copy_from_slice(&flags.to_le_bytes());
    mmap.range_mut(0, SEG_HEADER_SIZE)?.copy_from_slice(&buf);
    Ok(())
}

pub fn seal_segment(mmap: &mut MmapFile) -> Result<()> {
    let header = read_segment_header(mmap)?;
    if (header.flags & SEG_FLAG_SEALED) != 0 {
        return Ok(());
    }
    write_segment_header(mmap, header.segment_id, header.flags | SEG_FLAG_SEALED)?;
    Ok(())
}

pub fn repair_unsealed_tail(mmap: &mut MmapFile, segment_size: usize) -> Result<()> {
    let header = read_segment_header(mmap)?;
    if (header.flags & SEG_FLAG_SEALED) != 0 {
        return Ok(());
    }

    let mut end_offset = SEG_DATA_OFFSET;
    let mut offset = SEG_DATA_OFFSET;
    while offset + HEADER_SIZE <= segment_size {
        let commit = MessageHeader::load_commit_len(&mmap.as_slice()[offset] as *const u8);
        if commit == 0 {
            end_offset = offset;
            break;
        }
        let payload_len = match MessageHeader::payload_len_from_commit(commit) {
            Ok(len) => len,
            Err(_) => {
                end_offset = offset;
                break;
            }
        };
        if payload_len > MAX_PAYLOAD_LEN {
            end_offset = offset;
            break;
        }
        let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
        if offset + record_len > segment_size {
            end_offset = offset;
            break;
        }
        offset += record_len;
        end_offset = offset;
    }

    if end_offset + HEADER_SIZE <= segment_size {
        let remaining = segment_size - end_offset;
        if remaining >= HEADER_SIZE {
            let payload_len = remaining - HEADER_SIZE;
            let commit_len = MessageHeader::commit_len_for_payload(payload_len)?;
            let header = MessageHeader::new_uncommitted(0, 0, PAD_TYPE_ID, 0, 0);
            let header_bytes = header.to_bytes();
            mmap.range_mut(end_offset, HEADER_SIZE)?
                .copy_from_slice(&header_bytes);
            if payload_len > 0 {
                mmap.range_mut(end_offset + HEADER_SIZE, payload_len)?
                    .fill(0);
            }
            let header_ptr = unsafe { mmap.as_mut_slice().as_mut_ptr().add(end_offset) };
            MessageHeader::store_commit_len(header_ptr, commit_len);
        }
    }

    seal_segment(mmap)?;
    Ok(())
}

fn align_up(value: usize, align: usize) -> usize {
    if align == 0 {
        return value;
    }
    (value + align - 1) & !(align - 1)
}

fn reader_meta_slot_size() -> usize {
    40
}

fn reader_meta_file_size() -> usize {
    reader_meta_slot_size() * 2
}

fn encode_reader_slot(meta: &ReaderMeta) -> Result<[u8; 40]> {
    let mut buf = [0u8; 40];
    buf[0..8].copy_from_slice(&meta.segment_id.to_le_bytes());
    buf[8..16].copy_from_slice(&meta.offset.to_le_bytes());
    buf[16..24].copy_from_slice(&meta.last_heartbeat_ns.to_le_bytes());
    buf[24..32].copy_from_slice(&meta.generation.to_le_bytes());
    let crc = reader_meta_crc(&buf[0..32]);
    buf[32..36].copy_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

fn parse_reader_slot(buf: &[u8]) -> Option<ReaderMeta> {
    if buf.len() != 40 {
        return None;
    }
    let crc = u32::from_le_bytes(buf[32..36].try_into().ok()?);
    if reader_meta_crc(&buf[0..32]) != crc {
        return None;
    }
    let segment_id = u64::from_le_bytes(buf[0..8].try_into().ok()?);
    let offset = u64::from_le_bytes(buf[8..16].try_into().ok()?);
    let last_heartbeat_ns = u64::from_le_bytes(buf[16..24].try_into().ok()?);
    let generation = u64::from_le_bytes(buf[24..32].try_into().ok()?);
    Some(ReaderMeta::new(
        segment_id,
        offset,
        last_heartbeat_ns,
        generation,
    ))
}

fn select_reader_slot(slot0: Option<ReaderMeta>, slot1: Option<ReaderMeta>) -> Option<ReaderMeta> {
    match (slot0, slot1) {
        (None, None) => None,
        (Some(meta), None) | (None, Some(meta)) => Some(meta),
        (Some(a), Some(b)) => {
            if b.generation > a.generation {
                Some(b)
            } else {
                Some(a)
            }
        }
    }
}

fn reader_meta_crc(payload: &[u8]) -> u32 {
    use crc32fast::Hasher;
    let mut hasher = Hasher::new();
    hasher.update(payload);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_or_create_preserves_preallocated() -> Result<()> {
        let dir = tempdir()?;
        let root = dir.path();
        let segment_id = 1_u64;
        let segment_size = 4096_usize;
        let mut mmap = create_segment(root, segment_id, segment_size)?;
        let sentinel_offset = segment_size - 1;
        mmap.as_mut_slice()[sentinel_offset] = 0xAB;
        mmap.flush_sync()?;
        drop(mmap);

        let opened = open_or_create_segment(root, segment_id, segment_size)?;
        assert_eq!(opened.as_slice()[sentinel_offset], 0xAB);
        Ok(())
    }
}
