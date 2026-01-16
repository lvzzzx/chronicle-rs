use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
#[cfg(target_os = "linux")]
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::control::ControlFile;
use crate::header::{
    MessageHeader, HEADER_SIZE, MAX_PAYLOAD_LEN, PAD_TYPE_ID, RECORD_ALIGN,
};
use crate::mmap::MmapFile;
use crate::segment::{
    create_segment, load_index, open_segment, read_segment_header, seal_segment, segment_path,
    store_index, SegmentIndex, SEG_DATA_OFFSET, SEG_FLAG_SEALED, SEGMENT_SIZE,
};
use crate::wait::futex_wake;
use crate::{Error, Result};

const INDEX_FILE: &str = "index.meta";
const CONTROL_FILE: &str = "control.meta";
const WRITER_LOCK_FILE: &str = "writer.lock";

pub struct Queue;

pub struct QueueWriter {
    path: PathBuf,
    control: ControlFile,
    mmap: MmapFile,
    segment_id: u32,
    write_offset: u64,
    seq: u64,
    _lock: WriterLock,
}

impl Queue {
    pub fn open_publisher(path: impl AsRef<Path>) -> Result<QueueWriter> {
        let path = path.as_ref().to_path_buf();
        std::fs::create_dir_all(&path)?;

        let control_path = path.join(CONTROL_FILE);
        let lock = WriterLock::acquire(&path.join(WRITER_LOCK_FILE), 0)?;

        let control = if control_path.exists() {
            let control = ControlFile::open(&control_path)?;
            control.wait_ready()?;
            control
        } else {
            ControlFile::create(&control_path, 0, SEG_DATA_OFFSET as u64, 0)?
        };
        let writer_epoch = control.writer_epoch().wrapping_add(1).max(1);
        control.set_writer_epoch(writer_epoch);
        lock.update_epoch(writer_epoch)?;

        let index_path = path.join(INDEX_FILE);
        let index = load_index(&index_path)?;
        let mut segment_id = index.current_segment as u32;
        let mut mmap = if segment_path(&path, segment_id as u64).exists() {
            open_segment(&path, segment_id as u64)?
        } else {
            create_segment(&path, segment_id as u64)?
        };

        let (tail_offset, tail_partial) = scan_segment_tail(&mmap, index.write_offset as usize)?;
        let mut write_offset = tail_offset as u64;

        if tail_partial {
            if let Some(next_segment) = repair_tail_and_roll(&mut mmap, &path, segment_id)? {
                segment_id = next_segment;
                mmap = open_segment(&path, segment_id as u64)?;
                write_offset = SEG_DATA_OFFSET as u64;
            } else {
                write_offset = SEG_DATA_OFFSET as u64;
            }
        }

        let header = read_segment_header(&mmap)?;
        let next_segment_id = segment_id + 1;
        let next_path = segment_path(&path, next_segment_id as u64);
        if !tail_partial && ((header.flags & SEG_FLAG_SEALED) != 0 || next_path.exists()) {
            if (header.flags & SEG_FLAG_SEALED) == 0 {
                seal_segment(&mut mmap)?;
                mmap.sync()?;
            }
            if next_path.exists() {
                segment_id = next_segment_id;
                mmap = open_segment(&path, segment_id as u64)?;
            } else {
                mmap = create_segment(&path, next_segment_id as u64)?;
                segment_id = next_segment_id;
            }
            write_offset = SEG_DATA_OFFSET as u64;
        }

        control.set_current_segment(segment_id);
        control.set_write_offset(write_offset);

        Ok(QueueWriter {
            path,
            control,
            mmap,
            segment_id,
            write_offset,
            seq: 0,
            _lock: lock,
        })
    }
}

impl QueueWriter {
    pub fn append(&mut self, type_id: u16, payload: &[u8]) -> Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Unsupported("system time before UNIX epoch"))?;
        let timestamp_ns = u64::try_from(timestamp.as_nanos())
            .map_err(|_| Error::Unsupported("system time exceeds timestamp range"))?;
        self.append_with_timestamp(type_id, payload, timestamp_ns)
    }

    pub fn append_with_timestamp(
        &mut self,
        type_id: u16,
        payload: &[u8],
        timestamp_ns: u64,
    ) -> Result<()> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(Error::PayloadTooLarge);
        }
        let record_len = align_up(HEADER_SIZE + payload.len(), RECORD_ALIGN);
        if record_len > SEGMENT_SIZE - SEG_DATA_OFFSET {
            return Err(Error::PayloadTooLarge);
        }

        if (self.write_offset as usize) + record_len > SEGMENT_SIZE {
            self.roll_segment()?;
        }

        let checksum = MessageHeader::crc32(payload);
        let header = MessageHeader::new_uncommitted(
            self.seq,
            timestamp_ns,
            type_id,
            0,
            checksum,
        );
        let header_bytes = header.to_bytes();
        let offset = self.write_offset as usize;
        self.mmap
            .range_mut(offset, HEADER_SIZE)?
            .copy_from_slice(&header_bytes);
        if !payload.is_empty() {
            self.mmap
                .range_mut(offset + HEADER_SIZE, payload.len())?
                .copy_from_slice(payload);
        }

        let commit_len = MessageHeader::commit_len_for_payload(payload.len())?;
        let header_ptr = unsafe { self.mmap.as_mut_slice().as_mut_ptr().add(offset) };
        MessageHeader::store_commit_len(header_ptr, commit_len);

        self.seq = self.seq.wrapping_add(1);
        self.write_offset = self
            .write_offset
            .checked_add(record_len as u64)
            .ok_or(Error::Corrupt("write offset overflow"))?;
        self.control.set_write_offset(self.write_offset);

        self.control
            .notify_seq()
            .fetch_add(1, Ordering::Relaxed);
        futex_wake(self.control.notify_seq())?;
        Ok(())
    }

    pub fn flush_async(&mut self) -> Result<()> {
        self.mmap.flush_async()?;
        Ok(())
    }

    pub fn flush_sync(&mut self) -> Result<()> {
        self.mmap.flush_sync()?;
        let index_path = self.path.join(INDEX_FILE);
        let index = SegmentIndex::new(self.segment_id as u64, self.write_offset);
        store_index(&index_path, &index)?;
        Ok(())
    }

    pub fn cleanup(&self) -> Result<Vec<u64>> {
        crate::retention::cleanup_segments(&self.path, self.segment_id as u64, self.write_offset)
    }

    fn roll_segment(&mut self) -> Result<()> {
        let next_segment = self.segment_id + 1;
        create_segment(&self.path, next_segment as u64)?;
        self.control.set_current_segment(next_segment);
        self.control.set_write_offset(SEG_DATA_OFFSET as u64);

        seal_segment(&mut self.mmap)?;
        self.mmap.sync()?;

        self.segment_id = next_segment;
        self.write_offset = SEG_DATA_OFFSET as u64;
        self.mmap = open_segment(&self.path, next_segment as u64)?;

        self.control
            .notify_seq()
            .fetch_add(1, Ordering::Relaxed);
        futex_wake(self.control.notify_seq())?;
        Ok(())
    }
}

struct WriterLock {
    _file: File,
}

impl WriterLock {
    fn acquire(path: &Path, writer_epoch: u64) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        loop {
            if try_lock(&file)? {
                write_lock_record(&file, writer_epoch)?;
                return Ok(Self { _file: file });
            }
            if !lock_owner_alive(&file)? {
                continue;
            }
            return Err(Error::WriterAlreadyActive);
        }
    }

    fn update_epoch(&self, writer_epoch: u64) -> Result<()> {
        write_lock_record(&self._file, writer_epoch)
    }
}

fn align_up(value: usize, align: usize) -> usize {
    if align == 0 {
        return value;
    }
    (value + align - 1) & !(align - 1)
}

fn scan_segment_tail(mmap: &MmapFile, start_offset: usize) -> Result<(usize, bool)> {
    let mut offset = start_offset.max(SEG_DATA_OFFSET);
    let mut partial = false;

    loop {
        if offset + HEADER_SIZE > SEGMENT_SIZE {
            break;
        }
        let commit = MessageHeader::load_commit_len(&mmap.as_slice()[offset] as *const u8);
        if commit == 0 {
            let header_bytes = &mmap.as_slice()[offset..offset + HEADER_SIZE];
            if header_bytes.iter().any(|&b| b != 0) {
                partial = true;
            }
            break;
        }
        let payload_len = match MessageHeader::payload_len_from_commit(commit) {
            Ok(len) => len,
            Err(_) => {
                partial = true;
                break;
            }
        };
        if payload_len > MAX_PAYLOAD_LEN {
            partial = true;
            break;
        }
        let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
        if offset + record_len > SEGMENT_SIZE {
            partial = true;
            break;
        }
        offset += record_len;
    }

    Ok((offset, partial))
}

fn repair_tail_and_roll(mmap: &mut MmapFile, root: &Path, segment_id: u32) -> Result<Option<u32>> {
    let header = read_segment_header(mmap)?;
    if (header.flags & crate::segment::SEG_FLAG_SEALED) != 0 {
        let next_segment = segment_id + 1;
        create_segment(root, next_segment as u64)?;
        return Ok(Some(next_segment));
    }

    let mut end_offset = SEG_DATA_OFFSET;
    let mut offset = SEG_DATA_OFFSET;
    while offset + HEADER_SIZE <= SEGMENT_SIZE {
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
        let record_len = align_up(HEADER_SIZE + payload_len, RECORD_ALIGN);
        if offset + record_len > SEGMENT_SIZE {
            end_offset = offset;
            break;
        }
        offset += record_len;
        end_offset = offset;
    }

    if end_offset + HEADER_SIZE > SEGMENT_SIZE {
        seal_segment(mmap)?;
        let next_segment = segment_id + 1;
        create_segment(root, next_segment as u64)?;
        return Ok(Some(next_segment));
    }

    let remaining = SEGMENT_SIZE - end_offset;
    if remaining >= HEADER_SIZE {
        let payload_len = remaining - HEADER_SIZE;
        let commit_len = MessageHeader::commit_len_for_payload(payload_len)?;
        let header = MessageHeader::new_uncommitted(0, 0, PAD_TYPE_ID, 0, 0);
        let header_bytes = header.to_bytes();
        mmap.range_mut(end_offset, HEADER_SIZE)?.copy_from_slice(&header_bytes);
        if payload_len > 0 {
            mmap.range_mut(end_offset + HEADER_SIZE, payload_len)?.fill(0);
        }
        let header_ptr = unsafe { mmap.as_mut_slice().as_mut_ptr().add(end_offset) };
        MessageHeader::store_commit_len(header_ptr, commit_len);
    }

    seal_segment(mmap)?;
    let next_segment = segment_id + 1;
    create_segment(root, next_segment as u64)?;
    Ok(Some(next_segment))
}

#[cfg(target_os = "linux")]
fn try_lock(file: &File) -> Result<bool> {
    let res = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if res == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(false);
    }
    Err(Error::Io(err))
}

#[cfg(target_os = "linux")]
fn lock_owner_alive(file: &File) -> Result<bool> {
    let (pid, start_time) = read_lock_record(file)?;
    if pid == 0 {
        return Ok(false);
    }
    let proc_start = proc_start_time(pid)?;
    Ok(proc_start == start_time)
}

#[cfg(target_os = "linux")]
fn proc_start_time(pid: u32) -> Result<u64> {
    let path = format!("/proc/{pid}/stat");
    let mut contents = String::new();
    File::open(&path)?.read_to_string(&mut contents)?;
    let end = contents.rfind(')').ok_or(Error::CorruptMetadata("stat parse"))?;
    let after = &contents[end + 1..];
    let mut fields = after.split_whitespace();
    for _ in 0..20 {
        fields.next();
    }
    let start = fields
        .next()
        .ok_or(Error::CorruptMetadata("stat missing starttime"))?;
    let start_time = start
        .parse::<u64>()
        .map_err(|_| Error::CorruptMetadata("stat starttime invalid"))?;
    Ok(start_time)
}

#[cfg(target_os = "linux")]
fn write_lock_record(file: &File, writer_epoch: u64) -> Result<()> {
    let pid = std::process::id();
    let start_time = proc_start_time(pid)?;
    let record = format!("{pid} {start_time} {writer_epoch}\n");
    let mut handle = file.try_clone()?;
    handle.set_len(0)?;
    handle.seek(SeekFrom::Start(0))?;
    handle.write_all(record.as_bytes())?;
    handle.sync_all()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_lock_record(file: &File) -> Result<(u32, u64)> {
    let mut contents = String::new();
    let mut clone = file.try_clone()?;
    clone.seek(SeekFrom::Start(0))?;
    clone.read_to_string(&mut contents)?;
    let mut parts = contents.split_whitespace();
    let pid = parts
        .next()
        .unwrap_or("0")
        .parse::<u32>()
        .unwrap_or(0);
    let start_time = parts
        .next()
        .unwrap_or("0")
        .parse::<u64>()
        .unwrap_or(0);
    Ok((pid, start_time))
}

#[cfg(not(target_os = "linux"))]
fn try_lock(file: &File) -> Result<bool> {
    let res = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if res == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(false);
    }
    Err(Error::Io(err))
}

#[cfg(not(target_os = "linux"))]
fn lock_owner_alive(_file: &File) -> Result<bool> {
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
fn write_lock_record(file: &File, writer_epoch: u64) -> Result<()> {
    let pid = std::process::id();
    let record = format!("{pid} 0 {writer_epoch}\n");
    let mut handle = file.try_clone()?;
    handle.set_len(0)?;
    handle.seek(SeekFrom::Start(0))?;
    handle.write_all(record.as_bytes())?;
    handle.sync_all()?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn read_lock_record(_file: &File) -> Result<(u32, u64)> {
    Ok((0, 0))
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn proc_start_time(_pid: u32) -> Result<u64> {
    Ok(0)
}

use std::os::unix::io::AsRawFd;

#[cfg(test)]
mod tests {
    use super::Queue;
    use crate::header::HEADER_SIZE;
    use crate::segment::{SEG_DATA_OFFSET, SEGMENT_SIZE};
    use crate::Error;
    use tempfile::tempdir;

    #[test]
    fn payload_size_accounts_for_segment_header() {
        let dir = tempdir().expect("tempdir");
        let mut writer = Queue::open_publisher(dir.path()).expect("open publisher");
        let max_record_len = SEGMENT_SIZE - SEG_DATA_OFFSET;
        let max_payload_len = max_record_len - HEADER_SIZE;
        let payload = vec![0u8; max_payload_len];

        writer
            .append_with_timestamp(1, &payload, 0)
            .expect("append max payload");

        let oversized = vec![0u8; max_payload_len + 1];
        let err = writer
            .append_with_timestamp(1, &oversized, 0)
            .expect_err("oversized payload should fail");
        assert!(matches!(err, Error::PayloadTooLarge));
    }
}
