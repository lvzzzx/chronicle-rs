//! Segment file lifecycle management.
//!
//! This module provides pure file operations for Chronicle segment management,
//! with no state or coordination logic. Used by both Queue (live SWMR) and
//! Log (offline append-only) primitives.
//!
//! # Responsibilities
//!
//! - Segment file naming and path management
//! - Segment creation (temp → publish workflow)
//! - Segment sealing and header management
//! - Segment discovery and validation
//! - Memory prefaulting for low-latency writes
//!
//! # Design
//!
//! All functions are stateless and operate on paths/mmaps directly, making them
//! composable building blocks for higher-level abstractions.

use std::path::{Path, PathBuf};

use crate::core::mmap::MmapFile;
use crate::core::{Error, Result};

/// Default segment size (128 MB)
pub const DEFAULT_SEGMENT_SIZE: usize = 128 * 1024 * 1024;

/// Size of segment header
pub const SEG_HEADER_SIZE: usize = 64;

/// Offset where data begins (after header)
pub const SEG_DATA_OFFSET: usize = 64;

/// Segment magic number ('SEG0')
pub const SEG_MAGIC: u32 = 0x53454730;

/// Segment version
pub const SEG_VERSION: u32 = 2;

/// Flag indicating segment is sealed (immutable)
pub const SEG_FLAG_SEALED: u32 = 1;

/// Segment header stored at the beginning of each segment file.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentHeader {
    pub magic: u32,
    pub version: u32,
    pub segment_id: u32,
    pub flags: u32,
    pub _pad: [u8; 48],
}

// ============================================================================
// Segment Naming
// ============================================================================

/// Generate segment filename from ID (e.g., "000000042.q").
pub fn segment_filename(id: u64) -> String {
    format!("{:09}.q", id)
}

/// Generate temporary segment filename (e.g., "000000042.q.tmp").
pub fn segment_temp_filename(id: u64) -> String {
    format!("{:09}.q.tmp", id)
}

/// Get path to a segment file.
pub fn segment_path(root: &Path, id: u64) -> PathBuf {
    root.join(segment_filename(id))
}

/// Get path to a temporary segment file.
pub fn segment_temp_path(root: &Path, id: u64) -> PathBuf {
    root.join(segment_temp_filename(id))
}

/// Validate segment filename format.
///
/// Returns segment ID if valid, None otherwise.
pub fn parse_segment_filename(name: &str) -> Option<u64> {
    // Match *.q files (not *.q.tmp)
    if !name.ends_with(".q") || name.ends_with(".q.tmp") {
        return None;
    }

    let base = name.strip_suffix(".q")?;

    // Must be exactly 9 digits
    if base.len() != 9 {
        return None;
    }

    if !base.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    base.parse::<u64>().ok()
}

// ============================================================================
// Segment Discovery
// ============================================================================

/// Discover all segment IDs in a directory.
///
/// Returns sorted list of segment IDs found.
pub fn discover_segments(dir: &Path) -> Result<Vec<u64>> {
    let mut segments = Vec::new();

    if !dir.exists() {
        return Ok(segments);
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let path = entry.path();
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name,
            None => continue,
        };

        if let Some(id) = parse_segment_filename(file_name) {
            segments.push(id);
        }
    }

    segments.sort_unstable();
    Ok(segments)
}

/// Find the next segment ID by scanning a directory.
///
/// Returns 0 if no segments exist, otherwise max_id + 1.
pub fn next_segment_id(dir: &Path) -> Result<u64> {
    let segments = discover_segments(dir)?;
    Ok(segments.last().map_or(0, |&id| id.saturating_add(1)))
}

// ============================================================================
// Segment Creation & Opening
// ============================================================================

/// Validate segment size is within acceptable bounds.
///
/// # Errors
///
/// - `Error::Unsupported`: Size too small or too large
pub fn validate_segment_size(segment_size: u64) -> Result<usize> {
    let size = usize::try_from(segment_size)
        .map_err(|_| Error::Unsupported("segment size exceeds addressable range"))?;

    let min_size = SEG_DATA_OFFSET + 64; // Header + at least one message
    if size < min_size {
        return Err(Error::Unsupported("segment size too small"));
    }

    Ok(size)
}

/// Open an existing segment file.
///
/// Validates header and segment size.
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

/// Create a new segment file.
pub fn create_segment(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile> {
    let path = segment_path(root, id);
    let mut mmap = MmapFile::create(&path, segment_size)?;
    write_segment_header(&mut mmap, id as u32, 0)?;
    Ok(mmap)
}

/// Open existing segment or create new one if it doesn't exist.
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

/// Prepare a segment for writing (create + prefault).
///
/// Creates a new segment at the final path and prefaults memory.
pub fn prepare_segment(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile> {
    let mut mmap = create_segment(root, id, segment_size)?;
    prefault_mmap(&mut mmap);
    Ok(mmap)
}

/// Prepare a segment using temp file workflow (create .q.tmp + prefault).
///
/// Creates segment as .q.tmp which must later be published via `publish_segment()`.
/// Removes any existing temp file first.
pub fn prepare_segment_temp(root: &Path, id: u64, segment_size: usize) -> Result<MmapFile> {
    let temp_path = segment_temp_path(root, id);
    let _ = std::fs::remove_file(&temp_path);

    let mut mmap = MmapFile::create(&temp_path, segment_size)?;
    write_segment_header(&mut mmap, id as u32, 0)?;
    prefault_mmap(&mut mmap);

    Ok(mmap)
}

/// Prepare segment for writing, reusing if already exists.
///
/// Like `open_or_create_segment` but with prefaulting.
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

// ============================================================================
// Segment Publishing
// ============================================================================

/// Atomically publish a temp segment file to its final name.
///
/// Renames temp_path → final_path atomically. On Linux, uses `renameat2(RENAME_NOREPLACE)`
/// to ensure no accidental overwrites.
///
/// # Errors
///
/// - `Error::Io(AlreadyExists)`: Final path already exists
/// - `Error::Io`: Rename failed
pub fn publish_segment(temp_path: &Path, final_path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;

        let temp_c = CString::new(temp_path.as_os_str().as_bytes())
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
        if err.raw_os_error() != Some(libc::ENOSYS)
            && err.raw_os_error() != Some(libc::EINVAL)
        {
            return Err(Error::Io(err));
        }
    }

    // Fallback for non-Linux or if renameat2 not available
    if final_path.exists() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "segment already exists",
        )));
    }

    std::fs::rename(temp_path, final_path)?;
    Ok(())
}

// ============================================================================
// Segment Header Management
// ============================================================================

/// Read segment header from mmap.
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

/// Write segment header to mmap.
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

/// Seal a segment by setting the SEALED flag in its header.
///
/// Marks the segment as immutable. Idempotent - safe to call multiple times.
pub fn seal_segment(mmap: &mut MmapFile) -> Result<()> {
    let header = read_segment_header(mmap)?;

    if (header.flags & SEG_FLAG_SEALED) != 0 {
        return Ok(()); // Already sealed
    }

    write_segment_header(mmap, header.segment_id, header.flags | SEG_FLAG_SEALED)?;
    Ok(())
}

// ============================================================================
// Memory Prefaulting
// ============================================================================

/// Prefault memory by touching every 4KB page.
///
/// **Why this matters for HFT:**
///
/// When the OS allocates memory (via mmap), it is often "lazy". Virtual addresses
/// are assigned but physical RAM is not allocated until the first write. This causes
/// a **Page Fault** (context switch + allocation) on first access, introducing
/// non-deterministic latency spikes (μs to ms) in the critical path.
///
/// By writing to every page during initialization, we force the OS to:
/// 1. Allocate physical RAM
/// 2. Update page tables to mark pages as "Present" and "Writable"
/// 3. Reserve disk blocks (if file-backed)
///
/// This acts as a "poor man's" `fallocate` or `madvise(MADV_WILLNEED)` but is
/// more reliable and granular for ensuring memory is "hot" before use.
pub fn prefault_mmap(mmap: &mut MmapFile) {
    let slice = mmap.as_mut_slice();
    let len = slice.len();
    let page_size = 4096;

    // Skip the first page (offset 0) - header writing usually covers it
    let mut offset = page_size;
    while offset < len {
        slice[offset] = 0;
        offset += page_size;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_segment_naming() {
        assert_eq!(segment_filename(0), "000000000.q");
        assert_eq!(segment_filename(42), "000000042.q");
        assert_eq!(segment_filename(999_999_999), "999999999.q");

        assert_eq!(segment_temp_filename(0), "000000000.q.tmp");
        assert_eq!(segment_temp_filename(42), "000000042.q.tmp");
    }

    #[test]
    fn test_parse_segment_filename() {
        assert_eq!(parse_segment_filename("000000042.q"), Some(42));
        assert_eq!(parse_segment_filename("000000000.q"), Some(0));
        assert_eq!(parse_segment_filename("999999999.q"), Some(999_999_999));

        // Invalid formats
        assert_eq!(parse_segment_filename("000000042.q.tmp"), None);
        assert_eq!(parse_segment_filename("42.q"), None);
        assert_eq!(parse_segment_filename("abc.q"), None);
        assert_eq!(parse_segment_filename("000000042.txt"), None);
    }

    #[test]
    fn test_discover_segments() {
        let dir = TempDir::new().unwrap();

        // Empty directory
        let segments = discover_segments(dir.path()).unwrap();
        assert!(segments.is_empty());

        // Create some segments
        prepare_segment_temp(dir.path(), 0, 1024 * 1024).unwrap();
        publish_segment(
            &segment_temp_path(dir.path(), 0),
            &segment_path(dir.path(), 0),
        )
        .unwrap();

        prepare_segment_temp(dir.path(), 5, 1024 * 1024).unwrap();
        publish_segment(
            &segment_temp_path(dir.path(), 5),
            &segment_path(dir.path(), 5),
        )
        .unwrap();

        let segments = discover_segments(dir.path()).unwrap();
        assert_eq!(segments, vec![0, 5]);
    }

    #[test]
    fn test_next_segment_id() {
        let dir = TempDir::new().unwrap();

        // Empty directory → 0
        assert_eq!(next_segment_id(dir.path()).unwrap(), 0);

        // Create segment 0 → next is 1
        prepare_segment_temp(dir.path(), 0, 1024 * 1024).unwrap();
        publish_segment(
            &segment_temp_path(dir.path(), 0),
            &segment_path(dir.path(), 0),
        )
        .unwrap();
        assert_eq!(next_segment_id(dir.path()).unwrap(), 1);

        // Create segment 5 → next is 6
        prepare_segment_temp(dir.path(), 5, 1024 * 1024).unwrap();
        publish_segment(
            &segment_temp_path(dir.path(), 5),
            &segment_path(dir.path(), 5),
        )
        .unwrap();
        assert_eq!(next_segment_id(dir.path()).unwrap(), 6);
    }

    #[test]
    fn test_segment_lifecycle() {
        let dir = TempDir::new().unwrap();
        let segment_size = 1024 * 1024;

        // Prepare temp
        let mut mmap = prepare_segment_temp(dir.path(), 0, segment_size).unwrap();

        // Verify header
        let header = read_segment_header(&mmap).unwrap();
        assert_eq!(header.magic, SEG_MAGIC);
        assert_eq!(header.version, SEG_VERSION);
        assert_eq!(header.segment_id, 0);
        assert_eq!(header.flags, 0);

        // Seal
        seal_segment(&mut mmap).unwrap();
        let header = read_segment_header(&mmap).unwrap();
        assert_eq!(header.flags, SEG_FLAG_SEALED);

        // Drop mmap before publishing
        drop(mmap);

        // Publish
        publish_segment(
            &segment_temp_path(dir.path(), 0),
            &segment_path(dir.path(), 0),
        )
        .unwrap();

        // Verify published segment
        let mmap = open_segment(dir.path(), 0, segment_size).unwrap();
        let header = read_segment_header(&mmap).unwrap();
        assert_eq!(header.flags, SEG_FLAG_SEALED);
    }

    #[test]
    fn test_validate_segment_size() {
        // Valid sizes
        assert!(validate_segment_size(1024 * 1024).is_ok());
        assert!(validate_segment_size(128 * 1024 * 1024).is_ok());

        // Too small
        assert!(validate_segment_size(64).is_err());
    }
}
